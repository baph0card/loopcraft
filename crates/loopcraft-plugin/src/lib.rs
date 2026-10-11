//! CLAP plugin hosting for LoopCraft.
//!
//! CLAP (CLever Audio Plugin, MIT-licensed) is the first format supported
//! because its API is open and royalty-free, which suits an open-source
//! DAW. VST3 (GPLv3 / proprietary dual license) and Audio Units (macOS
//! only) are planned behind the same [`PluginFactory`] interface.
//!
//! Supported today: scanning, descriptor listing, instantiation, stereo
//! audio processing, and note events for instruments. Not yet: parameter
//! automation, state save/restore, plugin GUIs, latency compensation.

mod clap;
mod scan;

pub use clap::{ClapInstance, ClapLibrary, ClapPluginInfo};
pub use scan::{default_clap_paths, scan_clap};

use loopcraft_core::{PluginFormat, PluginRef};
use loopcraft_dsp::Device;
use loopcraft_engine::PluginFactory;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Loads plugins on demand, keeping each library loaded while any instance
/// from it is alive.
#[derive(Default)]
pub struct HostFactory {
    libraries: Mutex<HashMap<PathBuf, Arc<ClapLibrary>>>,
    errors: Mutex<Vec<String>>,
}

impl HostFactory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().map(|e| e.clone()).unwrap_or_default()
    }

    fn library(&self, path: &str) -> Result<Arc<ClapLibrary>, String> {
        let key = PathBuf::from(path);
        let mut libs = self.libraries.lock().map_err(|_| "plugin cache poisoned".to_string())?;
        if let Some(l) = libs.get(&key) {
            return Ok(l.clone());
        }
        let lib = Arc::new(ClapLibrary::load(&key)?);
        libs.insert(key, lib.clone());
        Ok(lib)
    }
}

impl PluginFactory for HostFactory {
    fn create(&self, plugin: &PluginRef, sample_rate: f32, max_block: usize) -> Option<Device> {
        let result = match plugin.format {
            PluginFormat::Clap => self.library(&plugin.path).and_then(|lib| {
                let info = lib.plugins.iter().find(|p| p.id == plugin.id).cloned().ok_or_else(|| format!("{} not found in {}", plugin.id, plugin.path))?;
                let inst = ClapInstance::new(lib, &plugin.id, sample_rate as f64, max_block as u32)?;
                Ok(if info.is_instrument { Device::Instrument(Box::new(inst)) } else { Device::Effect(Box::new(inst)) })
            }),
            PluginFormat::Vst3 => Err("VST3 hosting is not implemented yet".into()),
            PluginFormat::AudioUnit => Err("Audio Unit hosting is not implemented yet".into()),
        };
        match result {
            Ok(d) => Some(d),
            Err(e) => {
                if let Ok(mut errs) = self.errors.lock() {
                    errs.push(format!("{}: {e}", plugin.name));
                }
                None
            }
        }
    }
}
