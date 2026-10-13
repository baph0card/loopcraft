//! Per-user preferences, saved to the platform config directory.

use crate::audio::AudioSettings;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub audio: AudioSettings,
    /// MIDI input port names to listen to; empty = all.
    pub midi_inputs: Vec<String>,
    pub recent: Vec<PathBuf>,
    pub count_in_bars: u32,
    /// Where presets, recordings and new projects go by default.
    pub library_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            audio: AudioSettings { input_enabled: true, ..Default::default() },
            midi_inputs: Vec::new(),
            recent: Vec::new(),
            count_in_bars: 0,
            library_dir: None,
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|b| b.join("loopcraft").join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        config_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = config_path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
        }
    }

    pub fn library(&self) -> PathBuf {
        self.library_dir.clone().unwrap_or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("LoopCraft")).unwrap_or_else(|| PathBuf::from(".")))
    }

    pub fn add_recent(&mut self, p: PathBuf) {
        self.recent.retain(|r| r != &p);
        self.recent.insert(0, p);
        self.recent.truncate(10);
        self.save();
    }
}
