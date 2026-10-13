//! VST3 host. Same split as the CLAP host: [`Vst3Shared`] holds the
//! component, controller and editor (main thread) and stays alive across
//! graph rebuilds; [`Vst3Device`] is the audio-thread side.
//!
//! Supported: scanning `.vst3` bundles, instruments and effects with a main
//! stereo bus, note events, parameters (normalized 0..1) in both directions,
//! component + controller state, latency (with restarts when it changes),
//! IConnectionPoint messaging between processor and controller, and editors
//! embedded on X11 (with the Linux run loop VST3 editors need).

use crate::host_util::{EditorWindow, b64_decode, b64_encode};
use loopcraft_dsp::plugin_params::{PluginParam, register};
use loopcraft_dsp::{Effect, Instrument, ParamInfo, ProcessContext};
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vst3::Steinberg::Linux::{IEventHandler, IEventHandlerTrait, IRunLoop, IRunLoopTrait, ITimerHandler, ITimerHandlerTrait};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper, Interface};

#[derive(Debug, Clone)]
pub struct Vst3PluginInfo {
    /// Class id as 32 hex digits.
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub is_instrument: bool,
}

fn cid_hex(cid: &TUID) -> String {
    cid.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

fn parse_cid(s: &str) -> Option<TUID> {
    if s.len() != 32 {
        return None;
    }
    let mut t: TUID = [0; 16];
    for (i, b) in t.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()? as c_char;
    }
    Some(t)
}

fn fixed_str(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn utf16_str(chars: &[TChar]) -> String {
    let n = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..n])
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

type ExitFn = unsafe extern "system" fn() -> bool;

pub struct Vst3Library {
    factory: std::mem::ManuallyDrop<ComPtr<IPluginFactory>>,
    pub plugins: Vec<Vst3PluginInfo>,
    exit: Option<ExitFn>,
    _lib: libloading::Library,
}

unsafe impl Send for Vst3Library {}
unsafe impl Sync for Vst3Library {}

/// The shared library inside a `.vst3` bundle (or the path itself if it's a file).
fn bundle_binary(path: &Path) -> PathBuf {
    if path.is_file() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let c = path.join("Contents");
    let candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![c.join("MacOS").join(&stem)]
    } else if cfg!(target_os = "windows") {
        vec![c.join("x86_64-win").join(format!("{stem}.vst3"))]
    } else {
        let arch = if cfg!(target_arch = "aarch64") { "aarch64-linux" } else { "x86_64-linux" };
        vec![c.join(arch).join(format!("{stem}.so"))]
    };
    candidates.into_iter().find(|p| p.exists()).unwrap_or_else(|| path.to_path_buf())
}

impl Vst3Library {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bin = bundle_binary(path);
        unsafe {
            let lib = libloading::Library::new(&bin).map_err(|e| format!("cannot load {}: {e}", bin.display()))?;
            #[allow(unused_assignments)]
            let mut exit: Option<ExitFn> = None;
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                if let Ok(entry) = lib.get::<unsafe extern "system" fn(*mut c_void) -> bool>(b"ModuleEntry") {
                    // The SDK uses the handle only to find its own path; dlopen
                    // of the same file returns the same handle.
                    let h = libc::dlopen(CString::new(bin.to_string_lossy().as_bytes()).unwrap_or_default().as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD);
                    if !entry(h) {
                        return Err("ModuleEntry failed".into());
                    }
                    if !h.is_null() {
                        libc::dlclose(h);
                    }
                }
                exit = lib.get::<ExitFn>(b"ModuleExit").ok().map(|s| *s);
            }
            #[cfg(target_os = "windows")]
            {
                if let Ok(init) = lib.get::<unsafe extern "system" fn() -> bool>(b"InitDll") {
                    init();
                }
                exit = lib.get::<ExitFn>(b"ExitDll").ok().map(|s| *s);
            }
            #[cfg(target_os = "macos")]
            {
                // Not tested: no macOS build machine. The SDK exports bundleEntry;
                // some plugins export BundleEntry.
                if let Ok(entry) = lib.get::<unsafe extern "system" fn(*mut c_void) -> bool>(b"bundleEntry").or_else(|_| lib.get(b"BundleEntry")) {
                    entry(ptr::null_mut());
                }
                exit = lib.get::<ExitFn>(b"bundleExit").or_else(|_| lib.get::<ExitFn>(b"BundleExit")).ok().map(|s| *s);
            }
            let get = lib.get::<unsafe extern "system" fn() -> *mut IPluginFactory>(b"GetPluginFactory").map_err(|e| format!("not a VST3 plugin: {e}"))?;
            let factory = ComPtr::from_raw(get()).ok_or("GetPluginFactory returned nothing")?;
            let factory2 = factory.cast::<IPluginFactory2>();
            let mut finfo: PFactoryInfo = std::mem::zeroed();
            factory.getFactoryInfo(&mut finfo);
            let fvendor = fixed_str(&finfo.vendor);
            let mut plugins = Vec::new();
            for i in 0..factory.countClasses() {
                let mut info: PClassInfo = std::mem::zeroed();
                if factory.getClassInfo(i, &mut info) != kResultOk || fixed_str(&info.category) != "Audio Module Class" {
                    continue;
                }
                let (mut vendor, mut instrument) = (fvendor.clone(), false);
                if let Some(f2) = &factory2 {
                    let mut i2: PClassInfo2 = std::mem::zeroed();
                    if f2.getClassInfo2(i, &mut i2) == kResultOk {
                        instrument = fixed_str(&i2.subCategories).contains("Instrument");
                        let v = fixed_str(&i2.vendor);
                        if !v.is_empty() {
                            vendor = v;
                        }
                    }
                }
                plugins.push(Vst3PluginInfo { id: cid_hex(&info.cid), name: fixed_str(&info.name), vendor, is_instrument: instrument });
            }
            Ok(Self { factory: std::mem::ManuallyDrop::new(factory), plugins, exit, _lib: lib })
        }
    }

    unsafe fn create<I: Interface>(&self, cid: &TUID) -> Option<ComPtr<I>> {
        unsafe {
            let mut obj: *mut c_void = ptr::null_mut();
            let r = self.factory.createInstance(cid.as_ptr(), I::IID.as_ptr() as FIDString, &mut obj);
            if r != kResultOk {
                return None;
            }
            ComPtr::from_raw(obj as *mut I)
        }
    }
}

impl Drop for Vst3Library {
    fn drop(&mut self) {
        // Release the factory, then let the module clean up, then unload.
        unsafe {
            std::mem::ManuallyDrop::drop(&mut self.factory);
            if let Some(exit) = self.exit {
                exit();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Host-side COM objects
// ---------------------------------------------------------------------------

/// IHostApplication: names the host and creates messages for plugins whose
/// processor and controller talk through IConnectionPoint.
struct HostApp;

impl Class for HostApp {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        for (d, s) in unsafe { (*name).iter_mut() }.zip("LoopCraft".encode_utf16().chain(std::iter::once(0))) {
            *d = s as TChar;
        }
        kResultOk
    }

    unsafe fn createInstance(&self, cid: *mut TUID, iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
        unsafe {
            let want = |g: &[u8; 16]| (*cid).iter().zip(g.iter()).all(|(a, b)| *a as u8 == *b) || (*iid).iter().zip(g.iter()).all(|(a, b)| *a as u8 == *b);
            if want(&IMessage::IID) {
                let m = ComWrapper::new(Message::default());
                if let Some(p) = m.to_com_ptr::<IMessage>() {
                    *obj = p.into_raw() as *mut c_void;
                    return kResultOk;
                }
            }
            if want(&IAttributeList::IID) {
                let a = ComWrapper::new(Attributes::default());
                if let Some(p) = a.to_com_ptr::<IAttributeList>() {
                    *obj = p.into_raw() as *mut c_void;
                    return kResultOk;
                }
            }
            *obj = ptr::null_mut();
            kNoInterface
        }
    }
}

#[derive(Default)]
struct Message {
    id: Mutex<CString>,
    attrs: Mutex<Option<ComWrapper<Attributes>>>,
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        self.id.lock().map(|s| s.as_ptr()).unwrap_or(c"".as_ptr())
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        if let Ok(mut s) = self.id.lock() {
            *s = if id.is_null() { CString::default() } else { unsafe { CStr::from_ptr(id) }.to_owned() };
        }
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        let Ok(mut a) = self.attrs.lock() else { return ptr::null_mut() };
        let w = a.get_or_insert_with(|| ComWrapper::new(Attributes::default()));
        // The message keeps its attributes alive; hand out a borrowed pointer.
        w.as_com_ref::<IAttributeList>().map_or(ptr::null_mut(), |r| r.as_ptr())
    }
}

#[derive(Clone)]
enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<TChar>),
    Bin(Vec<u8>),
}

#[derive(Default)]
struct Attributes {
    map: Mutex<HashMap<String, Attr>>,
}

impl Class for Attributes {
    type Interfaces = (IAttributeList,);
}

unsafe fn attr_key(id: IAttributeList_::AttrID) -> String {
    if id.is_null() { String::new() } else { unsafe { CStr::from_ptr(id) }.to_string_lossy().into_owned() }
}

impl IAttributeListTrait for Attributes {
    unsafe fn setInt(&self, id: IAttributeList_::AttrID, value: i64) -> tresult {
        self.map.lock().map(|mut m| m.insert(unsafe { attr_key(id) }, Attr::Int(value))).ok();
        kResultOk
    }
    unsafe fn getInt(&self, id: IAttributeList_::AttrID, value: *mut i64) -> tresult {
        match self.map.lock().ok().and_then(|m| m.get(&unsafe { attr_key(id) }).cloned()) {
            Some(Attr::Int(v)) => {
                unsafe { *value = v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setFloat(&self, id: IAttributeList_::AttrID, value: f64) -> tresult {
        self.map.lock().map(|mut m| m.insert(unsafe { attr_key(id) }, Attr::Float(value))).ok();
        kResultOk
    }
    unsafe fn getFloat(&self, id: IAttributeList_::AttrID, value: *mut f64) -> tresult {
        match self.map.lock().ok().and_then(|m| m.get(&unsafe { attr_key(id) }).cloned()) {
            Some(Attr::Float(v)) => {
                unsafe { *value = v };
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setString(&self, id: IAttributeList_::AttrID, string: *const TChar) -> tresult {
        let mut v = Vec::new();
        let mut p = string;
        unsafe {
            while !p.is_null() && *p != 0 {
                v.push(*p);
                p = p.add(1);
            }
        }
        v.push(0);
        self.map.lock().map(|mut m| m.insert(unsafe { attr_key(id) }, Attr::Str(v))).ok();
        kResultOk
    }
    unsafe fn getString(&self, id: IAttributeList_::AttrID, string: *mut TChar, size_in_bytes: u32) -> tresult {
        match self.map.lock().ok().and_then(|m| m.get(&unsafe { attr_key(id) }).cloned()) {
            Some(Attr::Str(v)) => {
                let n = v.len().min(size_in_bytes as usize / 2);
                unsafe { ptr::copy_nonoverlapping(v.as_ptr(), string, n) };
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setBinary(&self, id: IAttributeList_::AttrID, data: *const c_void, size: u32) -> tresult {
        let v = unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) }.to_vec();
        self.map.lock().map(|mut m| m.insert(unsafe { attr_key(id) }, Attr::Bin(v))).ok();
        kResultOk
    }
    unsafe fn getBinary(&self, id: IAttributeList_::AttrID, data: *mut *const c_void, size: *mut u32) -> tresult {
        // The pointer must stay valid while the message lives: point into the map's storage.
        let Ok(m) = self.map.lock() else { return kResultFalse };
        match m.get(&unsafe { attr_key(id) }) {
            Some(Attr::Bin(v)) => {
                unsafe {
                    *data = v.as_ptr() as *const c_void;
                    *size = v.len() as u32;
                }
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

/// An in-memory IBStream for state.
#[derive(Default)]
struct MemStream {
    inner: Mutex<(Vec<u8>, usize)>,
}

impl Class for MemStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemStream {
    unsafe fn read(&self, buffer: *mut c_void, n: i32, read: *mut i32) -> tresult {
        let Ok(mut g) = self.inner.lock() else { return kResultFalse };
        let (data, pos) = &mut *g;
        let k = (n.max(0) as usize).min(data.len().saturating_sub(*pos));
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr().add(*pos), buffer as *mut u8, k);
            if !read.is_null() {
                *read = k as i32;
            }
        }
        *pos += k;
        kResultOk
    }
    unsafe fn write(&self, buffer: *mut c_void, n: i32, written: *mut i32) -> tresult {
        let Ok(mut g) = self.inner.lock() else { return kResultFalse };
        let (data, pos) = &mut *g;
        let src = unsafe { std::slice::from_raw_parts(buffer as *const u8, n.max(0) as usize) };
        if *pos + src.len() > data.len() {
            data.resize(*pos + src.len(), 0);
        }
        data[*pos..*pos + src.len()].copy_from_slice(src);
        *pos += src.len();
        if !written.is_null() {
            unsafe { *written = n };
        }
        kResultOk
    }
    unsafe fn seek(&self, p: i64, mode: i32, result: *mut i64) -> tresult {
        let Ok(mut g) = self.inner.lock() else { return kResultFalse };
        let (data, pos) = &mut *g;
        let base = match mode {
            0 => 0i64,
            1 => *pos as i64,
            _ => data.len() as i64,
        };
        *pos = (base + p).clamp(0, data.len() as i64) as usize;
        if !result.is_null() {
            unsafe { *result = *pos as i64 };
        }
        kResultOk
    }
    unsafe fn tell(&self, p: *mut i64) -> tresult {
        if let Ok(g) = self.inner.lock()
            && !p.is_null()
        {
            unsafe { *p = g.1 as i64 };
        }
        kResultOk
    }
}

impl MemStream {
    fn with(data: Vec<u8>) -> ComWrapper<MemStream> {
        ComWrapper::new(MemStream { inner: Mutex::new((data, 0)) })
    }
    fn take(w: &ComWrapper<MemStream>) -> Vec<u8> {
        w.inner.lock().map(|g| g.0.clone()).unwrap_or_default()
    }
}

/// Parameter traffic between the editor (main thread), the processor
/// (audio thread) and the saved project.
struct ParamSync {
    ids: Vec<ParamID>,
    id_index: Vec<(ParamID, usize)>,
    /// Editor edits waiting to reach the processor.
    to_proc: Vec<AtomicU64>,
    to_proc_dirty: Vec<AtomicBool>,
    any_to_proc: AtomicBool,
    /// Editor edits waiting to be saved into the project.
    to_host: Vec<AtomicU32>,
    to_host_dirty: Vec<AtomicBool>,
    any_to_host: AtomicBool,
    /// Host (automation, device view) edits waiting to reach the controller.
    to_ctrl: Vec<AtomicU64>,
    to_ctrl_dirty: Vec<AtomicBool>,
    any_to_ctrl: AtomicBool,
    restart: AtomicI32,
}

impl ParamSync {
    fn new(ids: Vec<ParamID>) -> Self {
        let n = ids.len();
        let mut id_index: Vec<(ParamID, usize)> = ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        id_index.sort_unstable();
        let a64 = || (0..n).map(|_| AtomicU64::new(0)).collect::<Vec<_>>();
        let a32 = || (0..n).map(|_| AtomicU32::new(0)).collect::<Vec<_>>();
        let ab = || (0..n).map(|_| AtomicBool::new(false)).collect::<Vec<_>>();
        Self {
            ids,
            id_index,
            to_proc: a64(),
            to_proc_dirty: ab(),
            any_to_proc: AtomicBool::new(false),
            to_host: a32(),
            to_host_dirty: ab(),
            any_to_host: AtomicBool::new(false),
            to_ctrl: a64(),
            to_ctrl_dirty: ab(),
            any_to_ctrl: AtomicBool::new(false),
            restart: AtomicI32::new(0),
        }
    }

    fn index(&self, id: ParamID) -> Option<usize> {
        self.id_index.binary_search_by_key(&id, |x| x.0).ok().map(|k| self.id_index[k].1)
    }
}

/// IComponentHandler: the controller (editor) reports edits here.
struct Handler {
    sync: Arc<ParamSync>,
}

impl Class for Handler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for Handler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        if let Some(i) = self.sync.index(id) {
            self.sync.to_proc[i].store(value.to_bits(), Release);
            self.sync.to_proc_dirty[i].store(true, Release);
            self.sync.any_to_proc.store(true, Release);
            self.sync.to_host[i].store((value as f32).to_bits(), Release);
            self.sync.to_host_dirty[i].store(true, Release);
            self.sync.any_to_host.store(true, Release);
        }
        kResultOk
    }
    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn restartComponent(&self, flags: i32) -> tresult {
        self.sync.restart.fetch_or(flags, AcqRel);
        kResultOk
    }
}

/// The Linux run loop VST3 editors need: they register fds (their X11
/// connection) and timers, and the host calls them back on the main thread.
#[derive(Default)]
struct RunLoopState {
    handlers: Mutex<Vec<(ComPtr<IEventHandler>, i32)>>,
    timers: Mutex<Vec<(ComPtr<ITimerHandler>, Duration, Instant)>>,
}

unsafe impl Send for RunLoopState {}
unsafe impl Sync for RunLoopState {}

/// IPlugFrame (+ Linux IRunLoop) handed to the editor.
struct Frame {
    run_loop: Arc<RunLoopState>,
    resize: Arc<Mutex<Option<(u32, u32)>>>,
}

impl Class for Frame {
    type Interfaces = (IPlugFrame, IRunLoop);
}

impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, view: *mut IPlugView, size: *mut ViewRect) -> tresult {
        unsafe {
            if size.is_null() {
                return kInvalidArgument;
            }
            let r = *size;
            if let Ok(mut p) = self.resize.lock() {
                *p = Some(((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32));
            }
            if let Some(v) = ComPtr::from_raw(view) {
                v.onSize(size);
                // from_raw took a reference the plugin didn't give us.
                let _ = v.into_raw();
            }
        }
        kResultOk
    }
}

impl IRunLoopTrait for Frame {
    unsafe fn registerEventHandler(&self, handler: *mut IEventHandler, fd: i32) -> tresult {
        unsafe {
            let Some(h) = vst3::ComRef::from_raw(handler) else { return kInvalidArgument };
            if let Ok(mut v) = self.run_loop.handlers.lock() {
                v.push((h.to_com_ptr(), fd));
            }
        }
        kResultOk
    }
    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        if let Ok(mut v) = self.run_loop.handlers.lock() {
            v.retain(|(h, _)| h.as_ptr() != handler);
        }
        kResultOk
    }
    unsafe fn registerTimer(&self, handler: *mut ITimerHandler, ms: u64) -> tresult {
        unsafe {
            let Some(h) = vst3::ComRef::from_raw(handler) else { return kInvalidArgument };
            let period = Duration::from_millis(ms.max(1));
            if let Ok(mut v) = self.run_loop.timers.lock() {
                v.push((h.to_com_ptr(), period, Instant::now() + period));
            }
        }
        kResultOk
    }
    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        if let Ok(mut v) = self.run_loop.timers.lock() {
            v.retain(|(h, _, _)| h.as_ptr() != handler);
        }
        kResultOk
    }
}

impl RunLoopState {
    fn run(&self) {
        // Collect first, call without holding the locks (handlers may re-register).
        let now = Instant::now();
        let due: Vec<ComPtr<ITimerHandler>> = self
            .timers
            .lock()
            .map(|mut v| {
                v.iter_mut()
                    .filter_map(|(h, p, next)| {
                        if *next <= now {
                            *next = now + *p;
                            Some(h.clone())
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        for h in due {
            unsafe { h.onTimer() };
        }
        #[cfg(unix)]
        {
            let fds: Vec<(ComPtr<IEventHandler>, i32)> = self.handlers.lock().map(|v| v.clone()).unwrap_or_default();
            if fds.is_empty() {
                return;
            }
            let mut p: Vec<libc::pollfd> = fds.iter().map(|(_, fd)| libc::pollfd { fd: *fd, events: libc::POLLIN, revents: 0 }).collect();
            // SAFETY: valid pollfd array; no timeout.
            if unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, 0) } > 0 {
                for (k, pf) in p.iter().enumerate() {
                    if pf.revents != 0 {
                        unsafe { fds[k].0.onFDIsSet(pf.fd) };
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shared instance
// ---------------------------------------------------------------------------

struct Editor {
    view: ComPtr<IPlugView>,
    window: EditorWindow,
    _frame: ComWrapper<Frame>,
}

pub struct Vst3Shared {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    separate_controller: bool,
    connections: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    _host: ComWrapper<HostApp>,
    _handler: ComWrapper<Handler>,
    sync: Arc<ParamSync>,
    pub name: String,
    pub is_instrument: bool,
    params: &'static [ParamInfo],
    pub(crate) n_in_buses: usize,
    pub(crate) n_out_buses: usize,
    latency: AtomicUsize,
    sample_rate: f64,
    max_block: u32,
    pub(crate) needs_rebuild: AtomicBool,
    started: AtomicBool,
    suspend: AtomicBool,
    suspended_ack: AtomicBool,
    run_loop: Arc<RunLoopState>,
    editor: Mutex<Option<Editor>>,
    pending_resize: Arc<Mutex<Option<(u32, u32)>>>,
    _lib: Arc<Vst3Library>,
}

unsafe impl Send for Vst3Shared {}
unsafe impl Sync for Vst3Shared {}

/// Saved state: [component length u32 LE][component bytes][controller bytes].
fn pack_state(comp: &[u8], ctrl: &[u8]) -> Vec<u8> {
    let mut v = (comp.len() as u32).to_le_bytes().to_vec();
    v.extend_from_slice(comp);
    v.extend_from_slice(ctrl);
    v
}

fn unpack_state(v: &[u8]) -> Option<(&[u8], &[u8])> {
    let n = u32::from_le_bytes(v.get(..4)?.try_into().ok()?) as usize;
    Some((v.get(4..4 + n)?, v.get(4 + n..)?))
}

impl Vst3Shared {
    pub fn new(lib: Arc<Vst3Library>, id: &str, sample_rate: f64, max_block: u32, state: Option<&str>) -> Result<Arc<Self>, String> {
        let info = lib.plugins.iter().find(|p| p.id == id).cloned().ok_or_else(|| format!("{id} not found"))?;
        let cid = parse_cid(id).ok_or("bad VST3 class id")?;
        unsafe {
            let host = ComWrapper::new(HostApp);
            let host_ptr = host.as_com_ref::<IHostApplication>().ok_or("host")?.as_ptr() as *mut FUnknown;
            let component: ComPtr<IComponent> = lib.create(&cid).ok_or("couldn't create the plugin's component")?;
            if component.initialize(host_ptr) != kResultOk {
                return Err("the plugin failed to initialize".into());
            }
            // Controller: the component itself, or a separate class.
            let mut controller = component.cast::<IEditController>();
            let mut separate = false;
            if controller.is_none() {
                let mut ccid: TUID = [0; 16];
                if component.getControllerClassId(&mut ccid) == kResultOk
                    && let Some(c) = lib.create::<IEditController>(&ccid)
                    && c.initialize(host_ptr) == kResultOk
                {
                    controller = Some(c);
                    separate = true;
                }
            }
            let processor = component.cast::<IAudioProcessor>().ok_or("the plugin has no audio processor")?;
            let connections = if separate {
                match (component.cast::<IConnectionPoint>(), controller.as_ref().and_then(|c| c.cast::<IConnectionPoint>())) {
                    (Some(a), Some(b)) => {
                        a.connect(b.as_ptr());
                        b.connect(a.as_ptr());
                        Some((a, b))
                    }
                    _ => None,
                }
            } else {
                None
            };

            // Buses: main stereo in (effects) and out; first event input for instruments.
            let n_in = component.getBusCount(MediaTypes_::kAudio as MediaType, BusDirections_::kInput as BusDirection).max(0) as usize;
            let n_out = component.getBusCount(MediaTypes_::kAudio as MediaType, BusDirections_::kOutput as BusDirection).max(0) as usize;
            let n_ev = component.getBusCount(MediaTypes_::kEvent as MediaType, BusDirections_::kInput as BusDirection).max(0) as usize;
            let mut ins = vec![SpeakerArr::kStereo; n_in];
            let mut outs = vec![SpeakerArr::kStereo; n_out];
            processor.setBusArrangements(ins.as_mut_ptr(), n_in as i32, outs.as_mut_ptr(), n_out as i32);
            for (kind, dir, n) in [
                (MediaTypes_::kAudio, BusDirections_::kInput, n_in),
                (MediaTypes_::kAudio, BusDirections_::kOutput, n_out),
                (MediaTypes_::kEvent, BusDirections_::kInput, n_ev),
            ] {
                if n > 0 {
                    component.activateBus(kind as MediaType, dir as BusDirection, 0, 1);
                }
            }
            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: max_block as i32,
                sampleRate: sample_rate,
            };
            if processor.setupProcessing(&mut setup) != kResultOk {
                return Err("the plugin rejected the sample rate / block size".into());
            }

            // State: restore if we have one, else sync the controller to the component.
            let restored = state.and_then(b64_decode);
            match restored.as_deref().and_then(unpack_state) {
                Some((comp, ctrl)) => {
                    let s = MemStream::with(comp.to_vec());
                    component.setState(s.as_com_ref::<IBStream>().map_or(ptr::null_mut(), |r| r.as_ptr()));
                    if let Some(c) = &controller {
                        let s = MemStream::with(comp.to_vec());
                        c.setComponentState(s.as_com_ref::<IBStream>().map_or(ptr::null_mut(), |r| r.as_ptr()));
                        let s = MemStream::with(ctrl.to_vec());
                        c.setState(s.as_com_ref::<IBStream>().map_or(ptr::null_mut(), |r| r.as_ptr()));
                    }
                }
                None => {
                    if let Some(c) = &controller {
                        let s = MemStream::with(Vec::new());
                        if component.getState(s.as_com_ref::<IBStream>().map_or(ptr::null_mut(), |r| r.as_ptr())) == kResultOk {
                            let data = MemStream::take(&s);
                            let s = MemStream::with(data);
                            c.setComponentState(s.as_com_ref::<IBStream>().map_or(ptr::null_mut(), |r| r.as_ptr()));
                        }
                    }
                }
            }

            // Parameters (normalized 0..1), from the controller.
            let mut list = Vec::new();
            let mut ids = Vec::new();
            if let Some(c) = &controller {
                for i in 0..c.getParameterCount() {
                    let mut pi: ParameterInfo = std::mem::zeroed();
                    if c.getParameterInfo(i, &mut pi) != kResultOk {
                        continue;
                    }
                    let f = pi.flags;
                    if f & (ParameterInfo_::ParameterFlags_::kIsReadOnly | ParameterInfo_::ParameterFlags_::kIsHidden) != 0 {
                        continue;
                    }
                    list.push(PluginParam { name: utf16_str(&pi.title), min: 0.0, max: 1.0, default: pi.defaultNormalizedValue as f32 });
                    ids.push(pi.id);
                }
            }
            let params = register(id, list);
            let sync = Arc::new(ParamSync::new(ids));
            let handler = ComWrapper::new(Handler { sync: sync.clone() });
            if let Some(c) = &controller {
                c.setComponentHandler(handler.as_com_ref::<IComponentHandler>().map_or(ptr::null_mut(), |r| r.as_ptr()));
            }

            if component.setActive(1) != kResultOk {
                return Err("the plugin failed to activate".into());
            }
            let latency = processor.getLatencySamples() as usize;
            Ok(Arc::new(Self {
                component,
                processor,
                controller,
                separate_controller: separate,
                connections,
                _host: host,
                _handler: handler,
                sync,
                name: info.name.clone(),
                is_instrument: info.is_instrument,
                params,
                n_in_buses: n_in,
                n_out_buses: n_out,
                latency: AtomicUsize::new(latency),
                sample_rate,
                max_block,
                needs_rebuild: AtomicBool::new(false),
                started: AtomicBool::new(false),
                suspend: AtomicBool::new(false),
                suspended_ack: AtomicBool::new(false),
                run_loop: Arc::new(RunLoopState::default()),
                editor: Mutex::new(None),
                pending_resize: Arc::new(Mutex::new(None)),
                _lib: lib,
            }))
        }
    }

    pub fn params(&self) -> &'static [ParamInfo] {
        self.params
    }

    pub fn save_state(&self) -> Option<String> {
        unsafe {
            let s = MemStream::with(Vec::new());
            if self.component.getState(s.as_com_ref::<IBStream>()?.as_ptr()) != kResultOk {
                return None;
            }
            let comp = MemStream::take(&s);
            let mut ctrl = Vec::new();
            if let Some(c) = &self.controller {
                let s = MemStream::with(Vec::new());
                if c.getState(s.as_com_ref::<IBStream>()?.as_ptr()) == kResultOk {
                    ctrl = MemStream::take(&s);
                }
            }
            Some(b64_encode(&pack_state(&comp, &ctrl)))
        }
    }

    pub fn has_editor(&self) -> bool {
        self.controller.is_some()
    }

    pub fn editor_open(&self) -> bool {
        self.editor.lock().map(|e| e.is_some()).unwrap_or(false)
    }

    pub fn open_editor(&self) -> Result<(), String> {
        if self.editor_open() {
            return Ok(());
        }
        let c = self.controller.as_ref().ok_or("this plugin has no editor")?;
        unsafe {
            let view = ComPtr::from_raw(c.createView(c"editor".as_ptr())).ok_or("this plugin has no editor")?;
            if view.isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) != kResultTrue {
                return Err("the plugin's editor doesn't support X11 embedding".into());
            }
            let mut r = ViewRect { left: 0, top: 0, right: 640, bottom: 480 };
            view.getSize(&mut r);
            let (w, h) = ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32);
            let window = EditorWindow::new(w, h, &self.name)?;
            let frame = ComWrapper::new(Frame { run_loop: self.run_loop.clone(), resize: self.pending_resize.clone() });
            view.setFrame(frame.as_com_ref::<IPlugFrame>().map_or(ptr::null_mut(), |f| f.as_ptr()));
            if view.attached(window.handle() as usize as *mut c_void, kPlatformTypeX11EmbedWindowID) != kResultOk {
                view.setFrame(ptr::null_mut());
                return Err("the plugin's editor failed to open".into());
            }
            *self.editor.lock().map_err(|_| "editor lock")? = Some(Editor { view, window, _frame: frame });
        }
        Ok(())
    }

    pub fn close_editor(&self) {
        let Ok(mut e) = self.editor.lock() else { return };
        if let Some(ed) = e.take() {
            unsafe {
                ed.view.removed();
                ed.view.setFrame(ptr::null_mut());
            }
            drop(ed);
        }
        if let Ok(mut h) = self.run_loop.handlers.lock() {
            h.clear();
        }
        if let Ok(mut t) = self.run_loop.timers.lock() {
            t.clear();
        }
    }

    pub fn idle(&self) {
        self.run_loop.run();
        // Host-side edits mirrored into the controller.
        if self.sync.any_to_ctrl.swap(false, AcqRel)
            && let Some(c) = &self.controller
        {
            for i in 0..self.sync.ids.len() {
                if self.sync.to_ctrl_dirty[i].swap(false, AcqRel) {
                    unsafe { c.setParamNormalized(self.sync.ids[i], f64::from_bits(self.sync.to_ctrl[i].load(Acquire))) };
                }
            }
        }
        let closed = match self.editor.lock().as_deref() {
            Ok(Some(ed)) => {
                if let Ok(mut r) = self.pending_resize.lock()
                    && let Some((w, h)) = r.take()
                {
                    ed.window.resize(w, h);
                }
                ed.window.poll_closed()
            }
            _ => false,
        };
        if closed {
            self.close_editor();
        }
        let flags = self.sync.restart.swap(0, AcqRel);
        if flags & (RestartFlags_::kLatencyChanged | RestartFlags_::kReloadComponent | RestartFlags_::kIoChanged) != 0 {
            self.restart();
        }
    }

    fn restart(&self) {
        self.suspend.store(true, SeqCst);
        let t0 = Instant::now();
        while !self.suspended_ack.load(SeqCst) && t0.elapsed().as_millis() < 250 {
            std::thread::sleep(Duration::from_millis(1));
        }
        unsafe {
            if self.started.swap(false, SeqCst) {
                self.processor.setProcessing(0);
            }
            self.component.setActive(0);
            self.component.setActive(1);
            let new = self.processor.getLatencySamples() as usize;
            if new != self.latency.swap(new, SeqCst) {
                self.needs_rebuild.store(true, SeqCst);
            }
        }
        self.suspended_ack.store(false, SeqCst);
        self.suspend.store(false, SeqCst);
    }

    pub fn take_param_changes(&self) -> Vec<(usize, f32)> {
        if !self.sync.any_to_host.swap(false, AcqRel) {
            return Vec::new();
        }
        (0..self.sync.ids.len())
            .filter(|&i| self.sync.to_host_dirty[i].swap(false, AcqRel))
            .map(|i| (i, f32::from_bits(self.sync.to_host[i].load(Acquire))))
            .collect()
    }
}

impl Drop for Vst3Shared {
    fn drop(&mut self) {
        self.close_editor();
        unsafe {
            if self.started.load(SeqCst) {
                self.processor.setProcessing(0);
            }
            self.component.setActive(0);
            if let Some((a, b)) = self.connections.take() {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = &self.controller {
                c.setComponentHandler(ptr::null_mut());
                if self.separate_controller {
                    c.terminate();
                }
            }
            self.component.terminate();
        }
    }
}

// ---------------------------------------------------------------------------
// Audio-thread objects
// ---------------------------------------------------------------------------

const MAX_EVENTS: usize = 512;
const MAX_POINTS: usize = 16;

/// IEventList backed by a preallocated vector; only the audio thread touches it.
struct EventList {
    events: UnsafeCell<Vec<Event>>,
}

unsafe impl Send for EventList {}
unsafe impl Sync for EventList {}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> i32 {
        unsafe { (*self.events.get()).len() as i32 }
    }
    unsafe fn getEvent(&self, index: i32, e: *mut Event) -> tresult {
        unsafe {
            match (&*self.events.get()).get(index as usize) {
                Some(ev) => {
                    *e = *ev;
                    kResultOk
                }
                None => kInvalidArgument,
            }
        }
    }
    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        unsafe {
            let v = &mut *self.events.get();
            if v.len() < v.capacity() {
                v.push(*e);
            }
        }
        kResultOk
    }
}

struct ParamQueue {
    id: ParamID,
    points: UnsafeCell<Vec<(i32, f64)>>,
}

unsafe impl Send for ParamQueue {}
unsafe impl Sync for ParamQueue {}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id
    }
    unsafe fn getPointCount(&self) -> i32 {
        unsafe { (*self.points.get()).len() as i32 }
    }
    unsafe fn getPoint(&self, index: i32, offset: *mut i32, value: *mut ParamValue) -> tresult {
        unsafe {
            match (&*self.points.get()).get(index as usize) {
                Some(&(o, v)) => {
                    *offset = o;
                    *value = v;
                    kResultOk
                }
                None => kInvalidArgument,
            }
        }
    }
    unsafe fn addPoint(&self, offset: i32, value: ParamValue, index: *mut i32) -> tresult {
        unsafe {
            let v = &mut *self.points.get();
            if v.len() >= v.capacity() {
                return kResultFalse;
            }
            v.push((offset, value));
            if !index.is_null() {
                *index = v.len() as i32 - 1;
            }
        }
        kResultOk
    }
}

/// IParameterChanges over one queue per parameter (all preallocated).
struct ParamChanges {
    queues: Vec<ComWrapper<ParamQueue>>,
    ptrs: Vec<*mut IParamValueQueue>,
    active: UnsafeCell<Vec<usize>>,
    sync: Arc<ParamSync>,
}

unsafe impl Send for ParamChanges {}
unsafe impl Sync for ParamChanges {}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> i32 {
        unsafe { (*self.active.get()).len() as i32 }
    }
    unsafe fn getParameterData(&self, index: i32) -> *mut IParamValueQueue {
        unsafe { (&*self.active.get()).get(index as usize).map_or(ptr::null_mut(), |&q| self.ptrs[q]) }
    }
    unsafe fn addParameterData(&self, id: *const ParamID, index: *mut i32) -> *mut IParamValueQueue {
        unsafe {
            let Some(q) = self.sync.index(*id) else { return ptr::null_mut() };
            let act = &mut *self.active.get();
            let pos = match act.iter().position(|&a| a == q) {
                Some(p) => p,
                None => {
                    act.push(q);
                    act.len() - 1
                }
            };
            if !index.is_null() {
                *index = pos as i32;
            }
            self.ptrs[q]
        }
    }
}

impl ParamChanges {
    fn new(sync: Arc<ParamSync>) -> ComWrapper<ParamChanges> {
        let queues: Vec<ComWrapper<ParamQueue>> =
            sync.ids.iter().map(|&id| ComWrapper::new(ParamQueue { id, points: UnsafeCell::new(Vec::with_capacity(MAX_POINTS)) })).collect();
        let ptrs = queues.iter().map(|q| q.as_com_ref::<IParamValueQueue>().map_or(ptr::null_mut(), |r| r.as_ptr())).collect();
        let n = queues.len();
        ComWrapper::new(ParamChanges { queues, ptrs, active: UnsafeCell::new(Vec::with_capacity(n)), sync })
    }

    /// Set parameter `q` at the start of the next block.
    fn set(&self, q: usize, value: f64) {
        unsafe {
            let act = &mut *self.active.get();
            if !act.contains(&q) {
                act.push(q);
            }
            let pts = &mut *self.queues[q].points.get();
            pts.clear();
            pts.push((0, value));
        }
    }

    fn clear(&self) {
        unsafe {
            for &q in (*self.active.get()).iter() {
                (*self.queues[q].points.get()).clear();
            }
            (*self.active.get()).clear();
        }
    }
}

/// The engine's view of a VST3 instance.
pub struct Vst3Device {
    shared: Arc<Vst3Shared>,
    events: ComWrapper<EventList>,
    changes: ComWrapper<ParamChanges>,
    context: Box<vst3::Steinberg::Vst::ProcessContext>,
    in_bufs: Vec<Vec<f32>>,
    out_bufs: Vec<Vec<f32>>,
    in_ptrs: Vec<*mut f32>,
    out_ptrs: Vec<*mut f32>,
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    samples: i64,
}

unsafe impl Send for Vst3Device {}

impl Vst3Device {
    pub fn new(shared: Arc<Vst3Shared>, params: &[f32]) -> Self {
        let n = shared.max_block as usize;
        // Two channels per bus; bus 0 carries the signal, others stay silent.
        let ni = shared.n_in_buses.max(1);
        let no = shared.n_out_buses.max(1);
        let mut in_bufs: Vec<Vec<f32>> = (0..ni * 2).map(|_| vec![0.0; n]).collect();
        let mut out_bufs: Vec<Vec<f32>> = (0..no * 2).map(|_| vec![0.0; n]).collect();
        let in_ptrs: Vec<*mut f32> = in_bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();
        let out_ptrs: Vec<*mut f32> = out_bufs.iter_mut().map(|b| b.as_mut_ptr()).collect();
        let d = Self {
            events: ComWrapper::new(EventList { events: UnsafeCell::new(Vec::with_capacity(MAX_EVENTS)) }),
            changes: ParamChanges::new(shared.sync.clone()),
            context: Box::new(unsafe { std::mem::zeroed() }),
            in_bufs,
            out_bufs,
            in_ptrs,
            out_ptrs,
            in_buses: Vec::new(),
            out_buses: Vec::new(),
            samples: 0,
            shared,
        };
        let mut d = d;
        let mk = |ptrs: &mut Vec<*mut f32>, count: usize| -> Vec<AudioBusBuffers> {
            (0..count)
                .map(|b| AudioBusBuffers {
                    numChannels: 2,
                    silenceFlags: 0,
                    __field0: AudioBusBuffers__type0 { channelBuffers32: unsafe { ptrs.as_mut_ptr().add(b * 2) } },
                })
                .collect()
        };
        d.in_buses = mk(&mut d.in_ptrs, d.shared.n_in_buses);
        d.out_buses = mk(&mut d.out_ptrs, d.shared.n_out_buses);
        for (i, v) in params.iter().enumerate() {
            d.changes.set(i, *v as f64);
        }
        d
    }

    fn push_note(&mut self, on: bool, key: u8, velocity: u8) {
        let v = unsafe { &mut *self.events.events.get() };
        if v.len() >= v.capacity() {
            return;
        }
        let mut e: Event = unsafe { std::mem::zeroed() };
        if on {
            e.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
            e.__field0.noteOn = NoteOnEvent { channel: 0, pitch: key as i16, tuning: 0.0, velocity: velocity as f32 / 127.0, length: 0, noteId: -1 };
        } else {
            e.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
            e.__field0.noteOff = NoteOffEvent { channel: 0, pitch: key as i16, velocity: 0.0, noteId: -1, tuning: 0.0 };
        }
        v.push(e);
    }

    fn set_param_now(&mut self, index: usize, value: f32) {
        if index < self.shared.sync.ids.len() {
            self.changes.set(index, value as f64);
            // Mirror to the controller (the editor) on the main thread.
            let s = &self.shared.sync;
            s.to_ctrl[index].store((value as f64).to_bits(), Release);
            s.to_ctrl_dirty[index].store(true, Release);
            s.any_to_ctrl.store(true, Release);
        }
    }

    fn run(&mut self, ctx: &ProcessContext, n: usize) -> bool {
        let n = n.min(self.shared.max_block as usize);
        let s = &*self.shared;
        if s.suspend.load(Acquire) {
            s.suspended_ack.store(true, Release);
            unsafe { (*self.events.events.get()).clear() };
            return false;
        }
        // Editor edits reach the processor.
        if s.sync.any_to_proc.swap(false, AcqRel) {
            for i in 0..s.sync.ids.len() {
                if s.sync.to_proc_dirty[i].swap(false, AcqRel) {
                    self.changes.set(i, f64::from_bits(s.sync.to_proc[i].load(Acquire)));
                }
            }
        }
        unsafe {
            if !s.started.load(Acquire) {
                s.processor.setProcessing(1);
                s.started.store(true, Release);
            }
            let c = &mut *self.context;
            c.state = ProcessContext_::StatesAndFlags_::kTempoValid
                | ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid
                | if ctx.playing { ProcessContext_::StatesAndFlags_::kPlaying } else { 0 };
            c.sampleRate = s.sample_rate;
            c.tempo = ctx.tempo;
            c.projectTimeMusic = ctx.beat;
            c.projectTimeSamples = self.samples;
            c.continousTimeSamples = self.samples;
            c.timeSigNumerator = 4;
            c.timeSigDenominator = 4;
            for b in self.in_buses.iter_mut().chain(self.out_buses.iter_mut()) {
                b.silenceFlags = 0;
            }
            let mut data = ProcessData {
                processMode: ProcessModes_::kRealtime as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                numSamples: n as i32,
                numInputs: self.in_buses.len() as i32,
                numOutputs: self.out_buses.len() as i32,
                inputs: if self.in_buses.is_empty() { ptr::null_mut() } else { self.in_buses.as_mut_ptr() },
                outputs: if self.out_buses.is_empty() { ptr::null_mut() } else { self.out_buses.as_mut_ptr() },
                inputParameterChanges: self.changes.as_com_ref::<IParameterChanges>().map_or(ptr::null_mut(), |r| r.as_ptr()),
                outputParameterChanges: ptr::null_mut(),
                inputEvents: self.events.as_com_ref::<IEventList>().map_or(ptr::null_mut(), |r| r.as_ptr()),
                outputEvents: ptr::null_mut(),
                processContext: &mut *self.context,
            };
            let r = s.processor.process(&mut data);
            (*self.events.events.get()).clear();
            self.changes.clear();
            self.samples += n as i64;
            r == kResultOk
        }
    }
}

impl Effect for Vst3Device {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let mut done = 0;
        let block = self.shared.max_block as usize;
        while done < left.len() {
            let n = (left.len() - done).min(block);
            self.in_bufs[0][..n].copy_from_slice(&left[done..done + n]);
            self.in_bufs[1][..n].copy_from_slice(&right[done..done + n]);
            if self.run(ctx, n) && self.shared.n_out_buses > 0 {
                left[done..done + n].copy_from_slice(&self.out_bufs[0][..n]);
                right[done..done + n].copy_from_slice(&self.out_bufs[1][..n]);
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
        self.set_param_now(index, value);
    }

    fn latency(&self) -> usize {
        self.shared.latency.load(Relaxed)
    }

    fn name(&self) -> &str {
        &self.shared.name
    }
}

impl Instrument for Vst3Device {
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
        let block = self.shared.max_block as usize;
        while done < left.len() {
            let n = (left.len() - done).min(block);
            for b in self.in_bufs.iter_mut() {
                b[..n].fill(0.0);
            }
            if self.run(ctx, n) && self.shared.n_out_buses > 0 {
                for i in 0..n {
                    left[done + i] += self.out_bufs[0][i];
                    right[done + i] += self.out_bufs[1][i];
                }
            }
            done += n;
        }
    }

    fn params(&self) -> &'static [ParamInfo] {
        self.shared.params
    }

    fn set_param(&mut self, index: usize, value: f32) {
        self.set_param_now(index, value);
    }

    fn name(&self) -> &str {
        &self.shared.name
    }
}
