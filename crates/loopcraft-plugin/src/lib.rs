//! Third-party plugin hosting for LoopCraft: CLAP and VST3.
//!
//! Both formats are open: CLAP is MIT-licensed, and the VST3 SDK has been
//! MIT-licensed since version 3.8, so both fit an open-source DAW.
//!
//! What works for both: scanning, instantiation, stereo audio, notes for
//! instruments, parameters (shown in the device view, automatable,
//! mappable, saved with the project), reported latency (compensated by the
//! engine), saving and restoring the plugin's own state, and the plugin's
//! editor window (embedded in a host window on X11 / Linux, or the plugin's
//! floating window where it offers one).
//!
//! Instances are kept in a pool keyed by `PluginRef::instance`, so a running
//! plugin, its editor and its internal state survive the engine rebuilding
//! its graph after an edit.
//!
//! Not yet: sidechain and multi-bus ports, MIDI output from plugins,
//! embedded editors on Windows and macOS, Audio Units.

mod clap;
mod host_util;
mod scan;
mod vst3host;

pub use crate::clap::{ClapDevice, ClapLibrary, ClapPluginInfo, ClapShared};
pub use host_util::{b64_decode, b64_encode};
pub use scan::{PluginInfo, default_clap_paths, default_vst3_paths, scan_all, scan_clap, scan_vst3};
pub use vst3host::{Vst3Device, Vst3Library, Vst3PluginInfo, Vst3Shared};

use loopcraft_core::{PluginFormat, PluginRef, Project};
use loopcraft_dsp::Device;
use loopcraft_engine::PluginFactory;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

/// A live instance of either format.
#[derive(Clone)]
pub enum Instance {
    Clap(Arc<ClapShared>),
    Vst3(Arc<Vst3Shared>),
}

impl Instance {
    fn device(&self, is_instrument: bool, params: &[f32]) -> Device {
        match self {
            Instance::Clap(s) => {
                let d = ClapDevice::new(s.clone(), params);
                if is_instrument { Device::Instrument(Box::new(d)) } else { Device::Effect(Box::new(d)) }
            }
            Instance::Vst3(s) => {
                let d = vst3host::Vst3Device::new(s.clone(), params);
                if is_instrument { Device::Instrument(Box::new(d)) } else { Device::Effect(Box::new(d)) }
            }
        }
    }

    pub fn save_state(&self) -> Option<String> {
        match self {
            Instance::Clap(s) => s.save_state(),
            Instance::Vst3(s) => s.save_state(),
        }
    }

    pub fn open_editor(&self) -> Result<(), String> {
        match self {
            Instance::Clap(s) => s.open_editor(),
            Instance::Vst3(s) => s.open_editor(),
        }
    }

    pub fn close_editor(&self) {
        match self {
            Instance::Clap(s) => s.close_editor(),
            Instance::Vst3(s) => s.close_editor(),
        }
    }

    pub fn editor_open(&self) -> bool {
        match self {
            Instance::Clap(s) => s.editor_open(),
            Instance::Vst3(s) => s.editor_open(),
        }
    }

    pub fn has_editor(&self) -> bool {
        match self {
            Instance::Clap(s) => s.has_editor(),
            Instance::Vst3(s) => s.has_editor(),
        }
    }

    fn idle(&self) {
        match self {
            Instance::Clap(s) => s.idle(),
            Instance::Vst3(s) => s.idle(),
        }
    }

    fn take_param_changes(&self) -> Vec<(usize, f32)> {
        match self {
            Instance::Clap(s) => s.take_param_changes(),
            Instance::Vst3(s) => s.take_param_changes(),
        }
    }

    fn needs_rebuild(&self) -> bool {
        match self {
            Instance::Clap(s) => s.needs_rebuild.swap(false, Ordering::AcqRel),
            Instance::Vst3(s) => s.needs_rebuild.swap(false, Ordering::AcqRel),
        }
    }
}

struct Entry {
    inst: Instance,
    key: (PluginFormat, String, String),
    sample_rate: f32,
    max_block: usize,
    used: bool,
}

/// Loads plugins on demand and keeps running instances across graph
/// rebuilds. Main-thread object: call [`HostFactory::idle`] every UI frame.
#[derive(Default)]
pub struct HostFactory {
    clap_libs: Mutex<HashMap<PathBuf, Arc<ClapLibrary>>>,
    vst3_libs: Mutex<HashMap<PathBuf, Arc<Vst3Library>>>,
    pool: Mutex<HashMap<u64, Entry>>,
    /// States of instances that left the pool (deleted devices), so undo
    /// can bring them back as they were.
    saved: Mutex<HashMap<u64, String>>,
    errors: Mutex<Vec<String>>,
}

impl HostFactory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().map(|e| e.clone()).unwrap_or_default()
    }

    fn clap_library(&self, path: &str) -> Result<Arc<ClapLibrary>, String> {
        let key = PathBuf::from(path);
        let mut libs = self.clap_libs.lock().map_err(|_| "plugin cache poisoned".to_string())?;
        if let Some(l) = libs.get(&key) {
            return Ok(l.clone());
        }
        let lib = Arc::new(ClapLibrary::load(&key)?);
        libs.insert(key, lib.clone());
        Ok(lib)
    }

    fn vst3_library(&self, path: &str) -> Result<Arc<Vst3Library>, String> {
        let key = PathBuf::from(path);
        let mut libs = self.vst3_libs.lock().map_err(|_| "plugin cache poisoned".to_string())?;
        if let Some(l) = libs.get(&key) {
            return Ok(l.clone());
        }
        let lib = Arc::new(Vst3Library::load(&key)?);
        libs.insert(key, lib.clone());
        Ok(lib)
    }

    fn instantiate(&self, plugin: &PluginRef, state: Option<&str>, sample_rate: f32, max_block: usize) -> Result<(Instance, bool), String> {
        match plugin.format {
            PluginFormat::Clap => {
                let lib = self.clap_library(&plugin.path)?;
                let s = ClapShared::new(lib, &plugin.id, sample_rate as f64, max_block as u32, state)?;
                let inst = s.is_instrument;
                Ok((Instance::Clap(s), inst))
            }
            PluginFormat::Vst3 => {
                let lib = self.vst3_library(&plugin.path)?;
                let s = Vst3Shared::new(lib, &plugin.id, sample_rate as f64, max_block as u32, state)?;
                let inst = s.is_instrument;
                Ok((Instance::Vst3(s), inst))
            }
            PluginFormat::AudioUnit => Err("Audio Units are macOS-only and not supported yet".into()),
        }
    }

    /// The running instance for a project's plugin, if any.
    pub fn instance(&self, instance: u64) -> Option<Instance> {
        self.pool.lock().ok()?.get(&instance).map(|e| e.inst.clone())
    }

    pub fn open_editor(&self, instance: u64) -> Result<(), String> {
        self.instance(instance).ok_or("the plugin isn't running")?.open_editor()
    }

    pub fn close_editor(&self, instance: u64) {
        if let Some(i) = self.instance(instance) {
            i.close_editor();
        }
    }

    pub fn editor_open(&self, instance: u64) -> bool {
        self.instance(instance).is_some_and(|i| i.editor_open())
    }

    pub fn has_editor(&self, instance: u64) -> bool {
        self.instance(instance).is_some_and(|i| i.has_editor())
    }

    /// Main-thread housekeeping: plugin timers, fds, callbacks, editor
    /// windows, restarts. Call every UI frame. Returns true if a plugin's
    /// latency changed, so the host should rebuild its graph.
    pub fn idle(&self) -> bool {
        let insts: Vec<Instance> = self.pool.lock().map(|p| p.values().map(|e| e.inst.clone()).collect()).unwrap_or_default();
        let mut rebuild = false;
        for i in insts {
            i.idle();
            rebuild |= i.needs_rebuild();
        }
        rebuild
    }

    /// Parameter values plugins changed themselves (in their editors):
    /// (instance, parameter index, value).
    pub fn take_param_changes(&self) -> Vec<(u64, usize, f32)> {
        let insts: Vec<(u64, Instance)> = self.pool.lock().map(|p| p.iter().map(|(k, e)| (*k, e.inst.clone())).collect()).unwrap_or_default();
        let mut out = Vec::new();
        for (k, i) in insts {
            out.extend(i.take_param_changes().into_iter().map(|(p, v)| (k, p, v)));
        }
        out
    }

    /// Write every running plugin's state into the project (before saving
    /// or exporting it).
    pub fn save_states(&self, project: &mut Project) {
        let Ok(pool) = self.pool.lock() else { return };
        project.for_each_plugin_mut(&mut |p| {
            if let Some(e) = pool.get(&p.instance)
                && let Some(s) = e.inst.save_state()
            {
                p.state = Some(s);
            }
        });
    }
}

impl PluginFactory for HostFactory {
    fn begin_build(&self) {
        if let Ok(mut pool) = self.pool.lock() {
            for e in pool.values_mut() {
                e.used = false;
            }
        }
    }

    fn end_build(&self) {
        // Instances the new graph doesn't use leave the pool (the old graph
        // may still hold them until it's dropped). Keep their state for undo.
        let Ok(mut pool) = self.pool.lock() else { return };
        let gone: Vec<u64> = pool.iter().filter(|(_, e)| !e.used).map(|(k, _)| *k).collect();
        for k in gone {
            if let Some(e) = pool.remove(&k) {
                e.inst.close_editor();
                if let (Some(s), Ok(mut saved)) = (e.inst.save_state(), self.saved.lock()) {
                    saved.insert(k, s);
                }
            }
        }
    }

    fn create(&self, plugin: &PluginRef, sample_rate: f32, max_block: usize) -> Option<Device> {
        let key = (plugin.format, plugin.path.clone(), plugin.id.clone());
        let id = plugin.instance;
        // Reuse the running instance when nothing about it changed.
        if id != 0
            && let Ok(mut pool) = self.pool.lock()
            && let Some(e) = pool.get_mut(&id)
            && e.key == key
            && e.sample_rate == sample_rate
            && e.max_block == max_block
            && !e.used
        {
            e.used = true;
            return Some(e.inst.device(plugin.is_instrument, &plugin.params));
        }
        // A new instance: restore from the project, or from the state it had
        // when it last left the pool.
        let carried = (id != 0).then(|| self.pool.lock().ok().and_then(|p| p.get(&id).and_then(|e| e.inst.save_state()))).flatten();
        let saved = (id != 0).then(|| self.saved.lock().ok().and_then(|mut s| s.remove(&id))).flatten();
        let state = carried.or(saved).or_else(|| plugin.state.clone());
        match self.instantiate(plugin, state.as_deref(), sample_rate, max_block) {
            Ok((inst, is_instrument)) => {
                let dev = inst.device(is_instrument || plugin.is_instrument, &plugin.params);
                if id != 0
                    && let Ok(mut pool) = self.pool.lock()
                    && let Some(old) = pool.insert(id, Entry { inst, key, sample_rate, max_block, used: true })
                {
                    old.inst.close_editor();
                }
                Some(dev)
            }
            Err(e) => {
                if let Ok(mut errs) = self.errors.lock() {
                    errs.push(format!("{}: {e}", plugin.name));
                }
                None
            }
        }
    }
}
