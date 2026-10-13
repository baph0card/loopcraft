//! CLAP host: load a library, list its plugins, and run instances.
//!
//! An instance is split in two:
//! - [`ClapShared`] owns the plugin and everything the main thread does with
//!   it (editor window, state, timers, restarts). It lives in an `Arc` kept
//!   by the [`crate::HostFactory`] pool, so the same instance survives engine
//!   graph rebuilds.
//! - [`ClapDevice`] is the audio-thread side the engine owns: its own event
//!   lists and buffers, calling the plugin's `process`. Only one engine
//!   graph processes at a time, so `process` is never called concurrently.

use crate::host_util::{EditorWindow, RunLoop, b64_decode, b64_encode};
use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::gui::{CLAP_EXT_GUI, CLAP_WINDOW_API_X11, clap_host_gui, clap_plugin_gui, clap_window, clap_window_handle};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_host_latency, clap_plugin_latency};
use clap_sys::ext::log::{CLAP_EXT_LOG, clap_host_log, clap_log_severity};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, CLAP_PARAM_IS_HIDDEN, CLAP_PARAM_IS_READONLY, clap_host_params, clap_param_clear_flags, clap_param_info, clap_param_rescan_flags,
    clap_plugin_params,
};
use clap_sys::ext::posix_fd_support::{CLAP_EXT_POSIX_FD_SUPPORT, clap_host_posix_fd_support, clap_plugin_posix_fd_support, clap_posix_fd_flags};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state, clap_plugin_state};
use clap_sys::ext::thread_check::{CLAP_EXT_THREAD_CHECK, clap_host_thread_check};
use clap_sys::ext::timer_support::{CLAP_EXT_TIMER_SUPPORT, clap_host_timer_support, clap_plugin_timer_support};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::fixedpoint::CLAP_BEATTIME_FACTOR;
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::clap_plugin;
use clap_sys::plugin_features::CLAP_PLUGIN_FEATURE_INSTRUMENT;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::{CLAP_VERSION, clap_version_is_compatible};
use loopcraft_dsp::plugin_params::{PluginParam, register};
use loopcraft_dsp::{Effect, Instrument, ParamInfo, ProcessContext};
use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering::*};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

#[derive(Debug, Clone)]
pub struct ClapPluginInfo {
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub features: Vec<String>,
    pub is_instrument: bool,
}

/// A loaded `.clap` library. `deinit` runs when the last reference drops.
pub struct ClapLibrary {
    entry: *const clap_plugin_entry,
    factory: *const clap_plugin_factory,
    pub plugins: Vec<ClapPluginInfo>,
    // Must be dropped last: everything above points into it.
    _lib: libloading::Library,
}

// The entry and factory are immutable tables the CLAP spec requires to be
// thread-safe.
unsafe impl Send for ClapLibrary {}
unsafe impl Sync for ClapLibrary {}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

impl ClapLibrary {
    pub fn load(path: &Path) -> Result<Self, String> {
        unsafe {
            let lib = libloading::Library::new(path).map_err(|e| format!("cannot load library: {e}"))?;
            let sym = lib.get::<*const clap_plugin_entry>(c"clap_entry").map_err(|e| format!("not a CLAP plugin (no clap_entry): {e}"))?;
            let entry: *const clap_plugin_entry = *sym;
            if entry.is_null() || !clap_version_is_compatible((*entry).clap_version) {
                return Err("incompatible CLAP version".into());
            }
            let cpath = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
            let init = (*entry).init.ok_or("missing init")?;
            if !init(cpath.as_ptr()) {
                return Err("plugin entry init failed".into());
            }
            let get_factory = (*entry).get_factory.ok_or("missing get_factory")?;
            let factory = get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) as *const clap_plugin_factory;
            if factory.is_null() {
                if let Some(deinit) = (*entry).deinit {
                    deinit();
                }
                return Err("library has no plugin factory".into());
            }
            let count = (*factory).get_plugin_count.map(|f| f(factory)).unwrap_or(0);
            let mut plugins = Vec::new();
            for i in 0..count {
                let Some(getd) = (*factory).get_plugin_descriptor else { break };
                let d = getd(factory, i);
                if d.is_null() {
                    continue;
                }
                let mut features = Vec::new();
                let mut f = (*d).features;
                while !f.is_null() && !(*f).is_null() {
                    features.push(cstr(*f));
                    f = f.add(1);
                }
                let instrument = CLAP_PLUGIN_FEATURE_INSTRUMENT.to_string_lossy();
                plugins.push(ClapPluginInfo {
                    id: cstr((*d).id),
                    name: cstr((*d).name),
                    vendor: cstr((*d).vendor),
                    version: cstr((*d).version),
                    is_instrument: features.iter().any(|x| *x == instrument),
                    features,
                });
            }
            Ok(Self { entry, factory, plugins, _lib: lib })
        }
    }
}

impl Drop for ClapLibrary {
    fn drop(&mut self) {
        unsafe {
            if let Some(deinit) = (*self.entry).deinit {
                deinit();
            }
        }
    }
}

const HOST_NAME: &CStr = c"LoopCraft";
const HOST_VENDOR: &CStr = c"LoopCraft contributors";
const HOST_URL: &CStr = c"https://github.com/loopcraft/loopcraft";
const HOST_VERSION: &CStr = c"0.3.0";

thread_local! {
    /// Set on threads that have called a plugin's `process`.
    static AUDIO_THREAD: Cell<bool> = const { Cell::new(false) };
}

// ---------------------------------------------------------------------------
// The shared (main-thread) half of an instance
// ---------------------------------------------------------------------------

#[allow(clippy::large_enum_variant)]
enum Gui {
    Embedded(EditorWindow),
    Floating,
}

pub struct ClapShared {
    plugin: AtomicPtr<clap_plugin>,
    host: Box<clap_host>,
    pub name: String,
    pub is_instrument: bool,
    params: &'static [ParamInfo],
    param_ids: Vec<clap_id>,
    /// (param id, index), sorted by id, for mapping plugin-sent events.
    id_index: Vec<(clap_id, usize)>,
    latency: AtomicUsize,
    sample_rate: f64,
    max_block: u32,
    main_thread: ThreadId,
    /// Values the plugin changed itself (its editor), for the host to save.
    param_out: Vec<AtomicU32>,
    param_dirty: Vec<AtomicBool>,
    any_dirty: AtomicBool,
    // Requests from the plugin, handled in `idle()`.
    callback_requested: AtomicBool,
    restart_requested: AtomicBool,
    latency_changed: AtomicBool,
    pub(crate) needs_rebuild: AtomicBool,
    // Processing state (audio thread) and suspension for restarts.
    started: AtomicBool,
    suspend: AtomicBool,
    suspended_ack: AtomicBool,
    run_loop: RunLoop,
    gui: Mutex<Option<Gui>>,
    pending_resize: Mutex<Option<(u32, u32)>>,
    gui_closed_by_plugin: AtomicBool,
    _lib: Arc<ClapLibrary>,
}

// The plugin pointer is used from the main thread (main-thread functions)
// and from exactly one audio thread at a time (`process`), as CLAP intends.
unsafe impl Send for ClapShared {}
unsafe impl Sync for ClapShared {}

unsafe fn shared<'a>(host: *const clap_host) -> Option<&'a ClapShared> {
    unsafe {
        let p = (*host).host_data as *const ClapShared;
        if p.is_null() { None } else { Some(&*p) }
    }
}

unsafe fn ext<T>(plugin: *const clap_plugin, id: &CStr) -> Option<&'static T> {
    unsafe {
        let p = (*plugin).get_extension.map(|f| f(plugin, id.as_ptr())).unwrap_or(ptr::null());
        if p.is_null() { None } else { Some(&*(p as *const T)) }
    }
}

impl ClapShared {
    pub fn new(lib: Arc<ClapLibrary>, id: &str, sample_rate: f64, max_block: u32, state: Option<&str>) -> Result<Arc<Self>, String> {
        let info = lib.plugins.iter().find(|p| p.id == id).cloned().ok_or_else(|| format!("{id} not found"))?;
        let host = Box::new(clap_host {
            clap_version: CLAP_VERSION,
            host_data: ptr::null_mut(),
            name: HOST_NAME.as_ptr(),
            vendor: HOST_VENDOR.as_ptr(),
            url: HOST_URL.as_ptr(),
            version: HOST_VERSION.as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_request_restart),
            request_process: Some(host_noop),
            request_callback: Some(host_request_callback),
        });
        let mut me = Arc::new(Self {
            plugin: AtomicPtr::new(ptr::null_mut()),
            host,
            name: info.name.clone(),
            is_instrument: info.is_instrument,
            params: &[],
            param_ids: Vec::new(),
            id_index: Vec::new(),
            latency: AtomicUsize::new(0),
            sample_rate,
            max_block,
            main_thread: std::thread::current().id(),
            param_out: Vec::new(),
            param_dirty: Vec::new(),
            any_dirty: AtomicBool::new(false),
            callback_requested: AtomicBool::new(false),
            restart_requested: AtomicBool::new(false),
            latency_changed: AtomicBool::new(false),
            needs_rebuild: AtomicBool::new(false),
            started: AtomicBool::new(false),
            suspend: AtomicBool::new(false),
            suspended_ack: AtomicBool::new(false),
            run_loop: RunLoop::default(),
            gui: Mutex::new(None),
            pending_resize: Mutex::new(None),
            gui_closed_by_plugin: AtomicBool::new(false),
            _lib: lib.clone(),
        });
        let cid = CString::new(id).map_err(|e| e.to_string())?;
        unsafe {
            // Point the host struct at the shared state before the plugin sees it.
            let m = Arc::get_mut(&mut me).expect("unique");
            m.host.host_data = m as *mut ClapShared as *mut c_void;
            let create = (*lib.factory).create_plugin.ok_or("factory cannot create plugins")?;
            let plugin = create(lib.factory, &*m.host, cid.as_ptr());
            if plugin.is_null() {
                return Err(format!("failed to create {id}"));
            }
            if !(*plugin).init.map(|f| f(plugin)).unwrap_or(false) {
                if let Some(f) = (*plugin).destroy {
                    f(plugin)
                }
                return Err(format!("{id} failed to initialize"));
            }
            m.plugin.store(plugin as *mut _, SeqCst);
            if let Some(s) = state.and_then(b64_decode) {
                m.load_state(&s);
            }
            let (params, ids) = query_params(plugin, id);
            m.params = params;
            m.id_index = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
            m.id_index.sort_unstable();
            m.param_out = (0..ids.len()).map(|_| AtomicU32::new(0)).collect();
            m.param_dirty = (0..ids.len()).map(|_| AtomicBool::new(false)).collect();
            m.param_ids = ids;
            if !(*plugin).activate.map(|f| f(plugin, sample_rate, 1, max_block)).unwrap_or(false) {
                return Err(format!("{id} failed to activate"));
            }
            m.latency.store(query_latency(plugin), SeqCst);
        }
        // `host_data` points into the Arc's allocation, which never moves.
        Ok(me)
    }

    fn plugin(&self) -> *const clap_plugin {
        self.plugin.load(Acquire)
    }

    pub fn params(&self) -> &'static [ParamInfo] {
        self.params
    }

    /// Save the plugin's state (state extension), base64-encoded.
    pub fn save_state(&self) -> Option<String> {
        unsafe {
            let st: &clap_plugin_state = ext(self.plugin(), CLAP_EXT_STATE)?;
            let save = st.save?;
            let mut buf: Vec<u8> = Vec::new();
            let os = clap_ostream { ctx: &mut buf as *mut Vec<u8> as *mut c_void, write: Some(ostream_write) };
            save(self.plugin(), &os).then(|| b64_encode(&buf))
        }
    }

    fn load_state(&self, data: &[u8]) -> bool {
        unsafe {
            let Some(st) = ext::<clap_plugin_state>(self.plugin(), CLAP_EXT_STATE) else { return false };
            let Some(load) = st.load else { return false };
            let mut cur = (data, 0usize);
            let is = clap_istream { ctx: &mut cur as *mut (&[u8], usize) as *mut c_void, read: Some(istream_read) };
            load(self.plugin(), &is)
        }
    }

    pub fn has_editor(&self) -> bool {
        unsafe { ext::<clap_plugin_gui>(self.plugin(), CLAP_EXT_GUI).is_some() }
    }

    pub fn editor_open(&self) -> bool {
        self.gui.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// Open the plugin's editor: embedded in a host window where possible
    /// (X11), else the plugin's own floating window.
    pub fn open_editor(&self) -> Result<(), String> {
        if self.editor_open() {
            return Ok(());
        }
        let p = self.plugin();
        let gui: &clap_plugin_gui = unsafe { ext(p, CLAP_EXT_GUI) }.ok_or("this plugin has no editor")?;
        self.gui_closed_by_plugin.store(false, SeqCst);
        unsafe {
            let supported = |api: &CStr, floating: bool| gui.is_api_supported.is_some_and(|f| f(p, api.as_ptr(), floating));
            // Embedded first.
            if supported(CLAP_WINDOW_API_X11, false) && gui.create.is_some_and(|f| f(p, CLAP_WINDOW_API_X11.as_ptr(), false)) {
                let (mut w, mut h) = (640u32, 480u32);
                if let Some(gs) = gui.get_size {
                    gs(p, &mut w, &mut h);
                }
                match EditorWindow::new(w, h, &self.name) {
                    Ok(win) => {
                        let window = clap_window { api: CLAP_WINDOW_API_X11.as_ptr(), specific: clap_window_handle { x11: win.handle() as _ } };
                        if gui.set_parent.is_some_and(|f| f(p, &window)) {
                            if let Some(show) = gui.show {
                                show(p);
                            }
                            *self.gui.lock().map_err(|_| "gui lock")? = Some(Gui::Embedded(win));
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        if let Some(d) = gui.destroy {
                            d(p);
                        }
                        // Fall through to floating.
                        if !(supported(CLAP_WINDOW_API_X11, true)) {
                            return Err(e);
                        }
                    }
                }
                if let Some(d) = gui.destroy {
                    d(p);
                }
            }
            // Floating: the plugin makes its own window.
            let mut api: *const c_char = ptr::null();
            let mut floating = true;
            let have_pref = gui.get_preferred_api.is_some_and(|f| f(p, &mut api, &mut floating));
            let api = if have_pref && !api.is_null() { api } else { CLAP_WINDOW_API_X11.as_ptr() };
            if gui.is_api_supported.is_some_and(|f| f(p, api, true)) && gui.create.is_some_and(|f| f(p, api, true)) {
                let title = CString::new(self.name.clone()).unwrap_or_default();
                if let Some(t) = gui.suggest_title {
                    t(p, title.as_ptr());
                }
                if let Some(show) = gui.show {
                    show(p);
                }
                *self.gui.lock().map_err(|_| "gui lock")? = Some(Gui::Floating);
                return Ok(());
            }
        }
        Err("the plugin's editor doesn't support this platform's windowing".into())
    }

    pub fn close_editor(&self) {
        let Ok(mut g) = self.gui.lock() else { return };
        let Some(old) = g.take() else { return };
        let p = self.plugin();
        unsafe {
            if let Some(gui) = ext::<clap_plugin_gui>(p, CLAP_EXT_GUI) {
                if let Some(h) = gui.hide {
                    h(p);
                }
                if let Some(d) = gui.destroy {
                    d(p);
                }
            }
        }
        drop(old); // destroys the host window after the plugin let go of it
    }

    /// Main-thread housekeeping, called by the host every UI frame.
    pub fn idle(&self) {
        let p = self.plugin();
        unsafe {
            if self.callback_requested.swap(false, AcqRel)
                && let Some(f) = (*p).on_main_thread
            {
                f(p);
            }
            if let Some(ts) = ext::<clap_plugin_timer_support>(p, CLAP_EXT_TIMER_SUPPORT)
                && let Some(on_timer) = ts.on_timer
            {
                for id in self.run_loop.due_timers() {
                    on_timer(p, id as clap_id);
                }
            }
            if let Some(fs) = ext::<clap_plugin_posix_fd_support>(p, CLAP_EXT_POSIX_FD_SUPPORT)
                && let Some(on_fd) = fs.on_fd
            {
                for (fd, flags) in self.run_loop.ready_fds() {
                    on_fd(p, fd, flags as clap_posix_fd_flags);
                }
            }
        }
        // Editor window.
        let closed = {
            let g = self.gui.lock();
            match g.as_deref() {
                Ok(Some(Gui::Embedded(w))) => {
                    if let Ok(mut r) = self.pending_resize.lock()
                        && let Some((rw, rh)) = r.take()
                    {
                        w.resize(rw, rh);
                    }
                    w.poll_closed()
                }
                _ => false,
            }
        };
        if closed || self.gui_closed_by_plugin.swap(false, AcqRel) {
            self.close_editor();
        }
        if self.restart_requested.swap(false, AcqRel) || self.latency_changed.swap(false, AcqRel) {
            self.restart();
        }
    }

    /// Deactivate and reactivate (the plugin asked, e.g. its latency
    /// changed). The audio side outputs silence meanwhile.
    fn restart(&self) {
        self.suspend.store(true, SeqCst);
        let t0 = std::time::Instant::now();
        while !self.suspended_ack.load(SeqCst) && t0.elapsed().as_millis() < 250 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let p = self.plugin();
        unsafe {
            if self.started.swap(false, SeqCst)
                && let Some(f) = (*p).stop_processing
            {
                f(p);
            }
            if let Some(f) = (*p).deactivate {
                f(p);
            }
            if let Some(f) = (*p).activate {
                f(p, self.sample_rate, 1, self.max_block);
            }
            let new = query_latency(p);
            if new != self.latency.swap(new, SeqCst) {
                self.needs_rebuild.store(true, SeqCst);
            }
        }
        self.suspended_ack.store(false, SeqCst);
        self.suspend.store(false, SeqCst);
    }

    /// Parameter values the plugin changed on its own since the last call.
    pub fn take_param_changes(&self) -> Vec<(usize, f32)> {
        if !self.any_dirty.swap(false, AcqRel) {
            return Vec::new();
        }
        self.param_dirty.iter().enumerate().filter(|(_, d)| d.swap(false, AcqRel)).map(|(i, _)| (i, f32::from_bits(self.param_out[i].load(Acquire)))).collect()
    }

    fn note_param_from_plugin(&self, id: clap_id, value: f64) {
        if let Ok(k) = self.id_index.binary_search_by_key(&id, |x| x.0) {
            let i = self.id_index[k].1;
            self.param_out[i].store((value as f32).to_bits(), Release);
            self.param_dirty[i].store(true, Release);
            self.any_dirty.store(true, Release);
        }
    }
}

impl Drop for ClapShared {
    fn drop(&mut self) {
        self.close_editor();
        self.run_loop.clear();
        let p = self.plugin();
        if p.is_null() {
            return;
        }
        unsafe {
            if self.started.load(SeqCst)
                && let Some(f) = (*p).stop_processing
            {
                f(p)
            }
            if let Some(f) = (*p).deactivate {
                f(p)
            }
            if let Some(f) = (*p).destroy {
                f(p)
            }
        }
    }
}

// --- host callbacks -------------------------------------------------------

unsafe extern "C" fn host_noop(_: *const clap_host) {}

unsafe extern "C" fn host_request_callback(h: *const clap_host) {
    if let Some(s) = unsafe { shared(h) } {
        s.callback_requested.store(true, Release);
    }
}

unsafe extern "C" fn host_request_restart(h: *const clap_host) {
    if let Some(s) = unsafe { shared(h) } {
        s.restart_requested.store(true, Release);
    }
}

unsafe extern "C" fn gui_resize_hints_changed(_: *const clap_host) {}
unsafe extern "C" fn gui_request_resize(h: *const clap_host, w: u32, hgt: u32) -> bool {
    let Some(s) = (unsafe { shared(h) }) else { return false };
    if let Ok(mut r) = s.pending_resize.lock() {
        *r = Some((w, hgt));
    }
    true
}
unsafe extern "C" fn gui_request_show(_: *const clap_host) -> bool {
    true
}
unsafe extern "C" fn gui_request_hide(_: *const clap_host) -> bool {
    true
}
unsafe extern "C" fn gui_closed(h: *const clap_host, _was_destroyed: bool) {
    if let Some(s) = unsafe { shared(h) } {
        s.gui_closed_by_plugin.store(true, Release);
    }
}
static HOST_GUI: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(gui_resize_hints_changed),
    request_resize: Some(gui_request_resize),
    request_show: Some(gui_request_show),
    request_hide: Some(gui_request_hide),
    closed: Some(gui_closed),
};

unsafe extern "C" fn timer_register(h: *const clap_host, period_ms: u32, id: *mut clap_id) -> bool {
    let Some(s) = (unsafe { shared(h) }) else { return false };
    let t = s.run_loop.add_timer(period_ms);
    unsafe { *id = t as clap_id };
    true
}
unsafe extern "C" fn timer_unregister(h: *const clap_host, id: clap_id) -> bool {
    unsafe { shared(h) }.is_some_and(|s| s.run_loop.remove_timer(id as u64))
}
static HOST_TIMER: clap_host_timer_support = clap_host_timer_support { register_timer: Some(timer_register), unregister_timer: Some(timer_unregister) };

unsafe extern "C" fn fd_register(h: *const clap_host, fd: i32, flags: clap_posix_fd_flags) -> bool {
    let Some(s) = (unsafe { shared(h) }) else { return false };
    s.run_loop.set_fd(fd, flags);
    true
}
unsafe extern "C" fn fd_unregister(h: *const clap_host, fd: i32) -> bool {
    unsafe { shared(h) }.is_some_and(|s| s.run_loop.remove_fd(fd))
}
static HOST_FD: clap_host_posix_fd_support =
    clap_host_posix_fd_support { register_fd: Some(fd_register), modify_fd: Some(fd_register), unregister_fd: Some(fd_unregister) };

unsafe extern "C" fn params_rescan(_: *const clap_host, _: clap_param_rescan_flags) {}
unsafe extern "C" fn params_clear(_: *const clap_host, _: clap_id, _: clap_param_clear_flags) {}
static HOST_PARAMS: clap_host_params = clap_host_params { rescan: Some(params_rescan), clear: Some(params_clear), request_flush: Some(host_noop) };

unsafe extern "C" fn latency_changed(h: *const clap_host) {
    if let Some(s) = unsafe { shared(h) } {
        s.latency_changed.store(true, Release);
    }
}
static HOST_LATENCY: clap_host_latency = clap_host_latency { changed: Some(latency_changed) };

static HOST_STATE: clap_host_state = clap_host_state { mark_dirty: Some(host_noop) };

unsafe extern "C" fn is_main_thread(h: *const clap_host) -> bool {
    unsafe { shared(h) }.is_some_and(|s| s.main_thread == std::thread::current().id())
}
unsafe extern "C" fn is_audio_thread(_: *const clap_host) -> bool {
    AUDIO_THREAD.with(|c| c.get())
}
static HOST_THREAD_CHECK: clap_host_thread_check = clap_host_thread_check { is_main_thread: Some(is_main_thread), is_audio_thread: Some(is_audio_thread) };

unsafe extern "C" fn host_log(_: *const clap_host, severity: clap_log_severity, msg: *const c_char) {
    if severity >= 2 {
        eprintln!("[plugin] {}", unsafe { cstr(msg) });
    }
}
static HOST_LOG: clap_host_log = clap_host_log { log: Some(host_log) };

unsafe extern "C" fn host_get_extension(_: *const clap_host, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return ptr::null();
    }
    let id = unsafe { CStr::from_ptr(id) };
    let p: *const c_void = if id == CLAP_EXT_GUI {
        &HOST_GUI as *const _ as _
    } else if id == CLAP_EXT_TIMER_SUPPORT {
        &HOST_TIMER as *const _ as _
    } else if id == CLAP_EXT_POSIX_FD_SUPPORT {
        &HOST_FD as *const _ as _
    } else if id == CLAP_EXT_PARAMS {
        &HOST_PARAMS as *const _ as _
    } else if id == CLAP_EXT_LATENCY {
        &HOST_LATENCY as *const _ as _
    } else if id == CLAP_EXT_STATE {
        &HOST_STATE as *const _ as _
    } else if id == CLAP_EXT_THREAD_CHECK {
        &HOST_THREAD_CHECK as *const _ as _
    } else if id == CLAP_EXT_LOG {
        &HOST_LOG as *const _ as _
    } else {
        ptr::null()
    };
    p
}

unsafe extern "C" fn ostream_write(s: *const clap_ostream, buf: *const c_void, size: u64) -> i64 {
    unsafe {
        let v = &mut *((*s).ctx as *mut Vec<u8>);
        v.extend_from_slice(std::slice::from_raw_parts(buf as *const u8, size as usize));
    }
    size as i64
}

unsafe extern "C" fn istream_read(s: *const clap_istream, buf: *mut c_void, size: u64) -> i64 {
    unsafe {
        let (data, pos) = &mut *((*s).ctx as *mut (&[u8], usize));
        let n = (size as usize).min(data.len() - *pos);
        ptr::copy_nonoverlapping(data.as_ptr().add(*pos), buf as *mut u8, n);
        *pos += n;
        n as i64
    }
}

/// Read the plugin's parameter list (params extension), hiding read-only
/// and hidden parameters, and register it for the UI.
unsafe fn query_params(plugin: *const clap_plugin, id: &str) -> (&'static [ParamInfo], Vec<u32>) {
    unsafe {
        let Some(ext) = ext::<clap_plugin_params>(plugin, CLAP_EXT_PARAMS) else { return (&[], Vec::new()) };
        let n = ext.count.map(|f| f(plugin)).unwrap_or(0);
        let mut list = Vec::new();
        let mut ids = Vec::new();
        for i in 0..n {
            let mut info: clap_param_info = std::mem::zeroed();
            if !ext.get_info.map(|f| f(plugin, i, &mut info)).unwrap_or(false) || info.flags & (CLAP_PARAM_IS_HIDDEN | CLAP_PARAM_IS_READONLY) != 0 {
                continue;
            }
            let name = CStr::from_ptr(info.name.as_ptr()).to_string_lossy().into_owned();
            list.push(PluginParam { name, min: info.min_value as f32, max: info.max_value as f32, default: info.default_value as f32 });
            ids.push(info.id);
        }
        (register(id, list), ids)
    }
}

unsafe fn query_latency(plugin: *const clap_plugin) -> usize {
    unsafe { ext::<clap_plugin_latency>(plugin, CLAP_EXT_LATENCY).and_then(|e| e.get).map(|f| f(plugin) as usize).unwrap_or(0) }
}

// ---------------------------------------------------------------------------
// The audio-thread half
// ---------------------------------------------------------------------------

const MAX_EVENTS: usize = 1024;

/// Note, parameter and transport events share one list; all start with a header.
#[repr(C)]
#[derive(Clone, Copy)]
union Ev {
    note: clap_event_note,
    param: clap_event_param_value,
}

struct InEvents {
    list: Vec<Ev>,
}

unsafe extern "C" fn in_size(list: *const clap_input_events) -> u32 {
    unsafe { (*((*list).ctx as *const InEvents)).list.len() as u32 }
}

unsafe extern "C" fn in_get(list: *const clap_input_events, i: u32) -> *const clap_event_header {
    unsafe {
        let ev = &*((*list).ctx as *const InEvents);
        ev.list.get(i as usize).map_or(ptr::null(), |e| e as *const Ev as *const clap_event_header)
    }
}

unsafe extern "C" fn out_try_push(list: *const clap_output_events, ev: *const clap_event_header) -> bool {
    unsafe {
        // Parameter changes the plugin makes itself (its editor) go back to the host.
        if !ev.is_null() && (*ev).space_id == CLAP_CORE_EVENT_SPACE_ID && (*ev).type_ == CLAP_EVENT_PARAM_VALUE {
            let s = &*((*list).ctx as *const ClapShared);
            let pv = &*(ev as *const clap_event_param_value);
            s.note_param_from_plugin(pv.param_id, pv.value);
        }
    }
    true
}

/// One running plugin as the engine sees it. Implements both [`Effect`] and
/// [`Instrument`]; the factory picks the role from the plugin's features.
pub struct ClapDevice {
    shared: Arc<ClapShared>,
    events: Box<InEvents>,
    in_list: Box<clap_input_events>,
    out_list: Box<clap_output_events>,
    transport: Box<clap_event_transport>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    steady: i64,
}

unsafe impl Send for ClapDevice {}

impl ClapDevice {
    pub fn new(shared: Arc<ClapShared>, params: &[f32]) -> Self {
        let n = shared.max_block as usize;
        let mut events = Box::new(InEvents { list: Vec::with_capacity(MAX_EVENTS) });
        let ctx = &mut *events as *mut InEvents as *mut c_void;
        let out_ctx = Arc::as_ptr(&shared) as *mut c_void;
        let mut d = Self {
            in_list: Box::new(clap_input_events { ctx, size: Some(in_size), get: Some(in_get) }),
            out_list: Box::new(clap_output_events { ctx: out_ctx, try_push: Some(out_try_push) }),
            transport: Box::new(unsafe { std::mem::zeroed() }),
            events,
            in_l: vec![0.0; n],
            in_r: vec![0.0; n],
            out_l: vec![0.0; n],
            out_r: vec![0.0; n],
            steady: 0,
            shared,
        };
        // Saved values reach the plugin with its first block.
        for (i, v) in params.iter().enumerate() {
            d.push_param(i, *v);
        }
        d
    }

    pub fn shared(&self) -> &Arc<ClapShared> {
        &self.shared
    }

    fn push_param(&mut self, index: usize, value: f32) {
        let Some(&id) = self.shared.param_ids.get(index) else { return };
        if self.events.list.len() >= MAX_EVENTS {
            return;
        }
        let header = clap_event_header {
            size: std::mem::size_of::<clap_event_param_value>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_PARAM_VALUE,
            flags: 0,
        };
        let param =
            clap_event_param_value { header, param_id: id, cookie: ptr::null_mut(), note_id: -1, port_index: -1, channel: -1, key: -1, value: value as f64 };
        self.events.list.push(Ev { param });
    }

    fn push_note(&mut self, on: bool, key: u8, velocity: u8) {
        if self.events.list.len() >= MAX_EVENTS {
            return;
        }
        let header = clap_event_header {
            size: std::mem::size_of::<clap_event_note>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: if on { CLAP_EVENT_NOTE_ON } else { CLAP_EVENT_NOTE_OFF },
            flags: 0,
        };
        let note = clap_event_note { header, note_id: -1, port_index: 0, channel: 0, key: key as i16, velocity: velocity as f64 / 127.0 };
        self.events.list.push(Ev { note });
    }

    /// Run the plugin over `n` frames using the internal in/out buffers.
    fn run(&mut self, ctx: &ProcessContext, n: usize) -> bool {
        let n = n.min(self.out_l.len());
        let s = &*self.shared;
        AUDIO_THREAD.with(|c| c.set(true));
        if s.suspend.load(Acquire) {
            s.suspended_ack.store(true, Release);
            self.events.list.clear();
            return false;
        }
        let p = s.plugin();
        unsafe {
            if !s.started.load(Acquire) {
                let ok = (*p).start_processing.map(|f| f(p)).unwrap_or(true);
                s.started.store(ok, Release);
            }
            let t = &mut *self.transport;
            t.header = clap_event_header {
                size: std::mem::size_of::<clap_event_transport>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_TRANSPORT,
                flags: 0,
            };
            t.flags = CLAP_TRANSPORT_HAS_TEMPO | CLAP_TRANSPORT_HAS_BEATS_TIMELINE | if ctx.playing { CLAP_TRANSPORT_IS_PLAYING } else { 0 };
            t.tempo = ctx.tempo;
            t.song_pos_beats = (ctx.beat * CLAP_BEATTIME_FACTOR as f64) as i64;
            let mut in_ptrs = [self.in_l.as_mut_ptr(), self.in_r.as_mut_ptr()];
            let mut out_ptrs = [self.out_l.as_mut_ptr(), self.out_r.as_mut_ptr()];
            let inputs = clap_audio_buffer { data32: in_ptrs.as_mut_ptr(), data64: ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
            let mut outputs = clap_audio_buffer { data32: out_ptrs.as_mut_ptr(), data64: ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
            let proc = clap_process {
                steady_time: self.steady,
                frames_count: n as u32,
                transport: &*self.transport,
                audio_inputs: &inputs,
                audio_outputs: &mut outputs,
                audio_inputs_count: 1,
                audio_outputs_count: 1,
                in_events: &*self.in_list,
                out_events: &*self.out_list,
            };
            let status = (*p).process.map(|f| f(p, &proc)).unwrap_or(CLAP_PROCESS_ERROR);
            self.steady += n as i64;
            self.events.list.clear();
            status != CLAP_PROCESS_ERROR
        }
    }
}

impl Effect for ClapDevice {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let mut done = 0;
        while done < left.len() {
            let n = (left.len() - done).min(self.in_l.len());
            self.in_l[..n].copy_from_slice(&left[done..done + n]);
            self.in_r[..n].copy_from_slice(&right[done..done + n]);
            if self.run(ctx, n) {
                left[done..done + n].copy_from_slice(&self.out_l[..n]);
                right[done..done + n].copy_from_slice(&self.out_r[..n]);
            } else {
                left[done..done + n].fill(0.0);
                right[done..done + n].fill(0.0);
            }
            done += n;
        }
    }

    fn params(&self) -> &'static [ParamInfo] {
        self.shared.params
    }

    fn set_param(&mut self, index: usize, value: f32) {
        self.push_param(index, value);
    }

    fn latency(&self) -> usize {
        self.shared.latency.load(Relaxed)
    }

    fn name(&self) -> &str {
        &self.shared.name
    }
}

impl Instrument for ClapDevice {
    fn note_on(&mut self, note: u8, velocity: u8) {
        self.push_note(true, note, velocity);
    }

    fn note_off(&mut self, note: u8) {
        self.push_note(false, note, 0);
    }

    fn all_notes_off(&mut self) {
        for k in 0..128 {
            self.push_note(false, k, 0);
        }
    }

    fn render(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let mut done = 0;
        while done < left.len() {
            let n = (left.len() - done).min(self.in_l.len());
            self.in_l[..n].fill(0.0);
            self.in_r[..n].fill(0.0);
            if self.run(ctx, n) {
                for i in 0..n {
                    left[done + i] += self.out_l[i];
                    right[done + i] += self.out_r[i];
                }
            }
            done += n;
        }
    }

    fn params(&self) -> &'static [ParamInfo] {
        self.shared.params
    }

    fn set_param(&mut self, index: usize, value: f32) {
        self.push_param(index, value);
    }

    fn name(&self) -> &str {
        &self.shared.name
    }
}
