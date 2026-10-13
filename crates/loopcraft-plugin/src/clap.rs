//! Minimal CLAP host: load a library, list its plugins, instantiate one,
//! process stereo audio and note events, set parameters (params extension)
//! and report latency (latency extension).

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, CLAP_PARAM_IS_HIDDEN, CLAP_PARAM_IS_READONLY, clap_param_info, clap_plugin_params};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::plugin_features::CLAP_PLUGIN_FEATURE_INSTRUMENT;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use clap_sys::version::{CLAP_VERSION, clap_version_is_compatible};
use loopcraft_dsp::plugin_params::{PluginParam, register};
use loopcraft_dsp::{Effect, Instrument, ParamInfo, ProcessContext};
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::ptr;
use std::sync::Arc;

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

// --- host callbacks -------------------------------------------------------

unsafe extern "C" fn host_get_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    ptr::null()
}
unsafe extern "C" fn host_noop(_: *const clap_host) {}

const HOST_NAME: &CStr = c"LoopCraft";
const HOST_VENDOR: &CStr = c"LoopCraft contributors";
const HOST_URL: &CStr = c"https://github.com/loopcraft/loopcraft";
const HOST_VERSION: &CStr = c"0.1.0";

// --- event lists -----------------------------------------------------------

const MAX_EVENTS: usize = 512;

/// Note and parameter events share one list; both start with a header.
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

unsafe extern "C" fn out_try_push(_: *const clap_output_events, _: *const clap_event_header) -> bool {
    // Plugin output events (note-end, param changes) are not routed yet.
    true
}

/// One running plugin instance. Implements both [`Effect`] and
/// [`Instrument`]; the factory picks the role from the plugin's features.
pub struct ClapInstance {
    plugin: *const clap_plugin,
    host: Box<clap_host>,
    events: Box<InEvents>,
    in_list: Box<clap_input_events>,
    out_list: Box<clap_output_events>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    started: bool,
    steady: i64,
    name: String,
    params: &'static [ParamInfo],
    param_ids: Vec<u32>,
    latency: usize,
    _lib: Arc<ClapLibrary>,
}

// Instances are created and destroyed on the control thread and processed
// on the audio thread, matching CLAP's threading contract.
unsafe impl Send for ClapInstance {}

impl ClapInstance {
    pub fn new(lib: Arc<ClapLibrary>, id: &str, sample_rate: f64, max_block: u32) -> Result<Self, String> {
        let host = Box::new(clap_host {
            clap_version: CLAP_VERSION,
            host_data: ptr::null_mut(),
            name: HOST_NAME.as_ptr(),
            vendor: HOST_VENDOR.as_ptr(),
            url: HOST_URL.as_ptr(),
            version: HOST_VERSION.as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_noop),
            request_process: Some(host_noop),
            request_callback: Some(host_noop),
        });
        let cid = CString::new(id).map_err(|e| e.to_string())?;
        unsafe {
            let create = (*lib.factory).create_plugin.ok_or("factory cannot create plugins")?;
            let plugin = create(lib.factory, &*host, cid.as_ptr());
            if plugin.is_null() {
                return Err(format!("failed to create {id}"));
            }
            if !(*plugin).init.map(|f| f(plugin)).unwrap_or(false) {
                if let Some(f) = (*plugin).destroy {
                    f(plugin)
                }
                return Err(format!("{id} failed to initialize"));
            }
            if !(*plugin).activate.map(|f| f(plugin, sample_rate, 1, max_block)).unwrap_or(false) {
                if let Some(f) = (*plugin).destroy {
                    f(plugin)
                }
                return Err(format!("{id} failed to activate"));
            }
            let (params, param_ids) = query_params(plugin, id);
            let latency = query_latency(plugin);
            let mut events = Box::new(InEvents { list: Vec::with_capacity(MAX_EVENTS) });
            let ctx = &mut *events as *mut InEvents as *mut c_void;
            let n = max_block as usize;
            Ok(Self {
                plugin,
                host,
                in_list: Box::new(clap_input_events { ctx, size: Some(in_size), get: Some(in_get) }),
                out_list: Box::new(clap_output_events { ctx: ptr::null_mut(), try_push: Some(out_try_push) }),
                events,
                in_l: vec![0.0; n],
                in_r: vec![0.0; n],
                out_l: vec![0.0; n],
                out_r: vec![0.0; n],
                started: false,
                steady: 0,
                name: lib.plugins.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_else(|| id.to_string()),
                params,
                param_ids,
                latency,
                _lib: lib,
            })
        }
    }

    /// The plugin's parameters, in the order LoopCraft indexes them.
    pub fn param_table(&self) -> &'static [ParamInfo] {
        self.params
    }

    fn push_param(&mut self, index: usize, value: f32) {
        let Some(&id) = self.param_ids.get(index) else { return };
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
    fn run(&mut self, n: usize) -> bool {
        let n = n.min(self.out_l.len());
        unsafe {
            if !self.started {
                self.started = (*self.plugin).start_processing.map(|f| f(self.plugin)).unwrap_or(true);
            }
            let mut in_ptrs = [self.in_l.as_mut_ptr(), self.in_r.as_mut_ptr()];
            let mut out_ptrs = [self.out_l.as_mut_ptr(), self.out_r.as_mut_ptr()];
            let inputs = clap_audio_buffer { data32: in_ptrs.as_mut_ptr(), data64: ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
            let mut outputs = clap_audio_buffer { data32: out_ptrs.as_mut_ptr(), data64: ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
            let proc = clap_process {
                steady_time: self.steady,
                frames_count: n as u32,
                transport: ptr::null(),
                audio_inputs: &inputs,
                audio_outputs: &mut outputs,
                audio_inputs_count: 1,
                audio_outputs_count: 1,
                in_events: &*self.in_list,
                out_events: &*self.out_list,
            };
            let status = (*self.plugin).process.map(|f| f(self.plugin, &proc)).unwrap_or(CLAP_PROCESS_ERROR);
            self.steady += n as i64;
            self.events.list.clear();
            status != CLAP_PROCESS_ERROR
        }
    }
}

/// Read the plugin's parameter list (params extension), hiding read-only
/// and hidden parameters, and register it for the UI.
unsafe fn query_params(plugin: *const clap_plugin, id: &str) -> (&'static [ParamInfo], Vec<u32>) {
    unsafe {
        let ext = (*plugin).get_extension.map(|f| f(plugin, CLAP_EXT_PARAMS.as_ptr())).unwrap_or(ptr::null());
        if ext.is_null() {
            return (&[], Vec::new());
        }
        let ext = &*(ext as *const clap_plugin_params);
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
    unsafe {
        let ext = (*plugin).get_extension.map(|f| f(plugin, CLAP_EXT_LATENCY.as_ptr())).unwrap_or(ptr::null());
        if ext.is_null() {
            return 0;
        }
        let ext = &*(ext as *const clap_plugin_latency);
        ext.get.map(|f| f(plugin)).unwrap_or(0) as usize
    }
}

impl Drop for ClapInstance {
    fn drop(&mut self) {
        unsafe {
            if self.started
                && let Some(f) = (*self.plugin).stop_processing
            {
                f(self.plugin)
            }
            if let Some(f) = (*self.plugin).deactivate {
                f(self.plugin)
            }
            if let Some(f) = (*self.plugin).destroy {
                f(self.plugin)
            }
        }
        let _ = &self.host;
    }
}

impl Effect for ClapInstance {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let mut done = 0;
        while done < left.len() {
            let n = (left.len() - done).min(self.in_l.len());
            self.in_l[..n].copy_from_slice(&left[done..done + n]);
            self.in_r[..n].copy_from_slice(&right[done..done + n]);
            if self.run(n) {
                left[done..done + n].copy_from_slice(&self.out_l[..n]);
                right[done..done + n].copy_from_slice(&self.out_r[..n]);
            }
            done += n;
        }
    }

    fn params(&self) -> &'static [ParamInfo] {
        self.params
    }

    fn set_param(&mut self, index: usize, value: f32) {
        self.push_param(index, value);
    }

    fn latency(&self) -> usize {
        self.latency
    }

    fn name(&self) -> &str {
        &self.name
    }
}

impl Instrument for ClapInstance {
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

    fn render(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let mut done = 0;
        while done < left.len() {
            let n = (left.len() - done).min(self.in_l.len());
            self.in_l[..n].fill(0.0);
            self.in_r[..n].fill(0.0);
            if self.run(n) {
                for i in 0..n {
                    left[done + i] += self.out_l[i];
                    right[done + i] += self.out_r[i];
                }
            }
            done += n;
        }
    }

    fn params(&self) -> &'static [ParamInfo] {
        self.params
    }

    fn set_param(&mut self, index: usize, value: f32) {
        self.push_param(index, value);
    }

    fn name(&self) -> &str {
        &self.name
    }
}
