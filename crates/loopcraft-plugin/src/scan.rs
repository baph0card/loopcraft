//! Finding plugins on disk: CLAP files in the locations from the CLAP
//! specification plus `$CLAP_PATH`, and VST3 bundles in the locations from
//! the VST3 SDK documentation plus `$VST3_PATH`.

use crate::clap::{ClapLibrary, ClapPluginInfo};
use std::path::{Path, PathBuf};

pub fn default_clap_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(extra) = std::env::var_os("CLAP_PATH") {
        v.extend(std::env::split_paths(&extra));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        if let Some(h) = &home {
            v.push(h.join("Library/Audio/Plug-Ins/CLAP"));
        }
        v.push("/Library/Audio/Plug-Ins/CLAP".into());
    } else if cfg!(target_os = "windows") {
        if let Some(c) = std::env::var_os("COMMONPROGRAMFILES") {
            v.push(PathBuf::from(c).join("CLAP"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            v.push(PathBuf::from(l).join("Programs/Common/CLAP"));
        }
    } else {
        if let Some(h) = &home {
            v.push(h.join(".clap"));
        }
        v.push("/usr/lib/clap".into());
        v.push("/usr/local/lib/clap".into());
    }
    v
}

/// The loadable binary for a `.clap` entry (macOS bundles are directories).
fn binary_for(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    let macos = path.join("Contents/MacOS");
    std::fs::read_dir(macos).ok()?.flatten().map(|e| e.path()).find(|p| p.is_file())
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "clap") {
            if let Some(b) = binary_for(&p) {
                out.push(b);
            }
        } else if p.is_dir() && depth < 4 {
            walk(&p, depth + 1, out);
        }
    }
}

/// Scan directories and load each library to read its descriptors.
/// Returns (library path, plugins) and a list of failures.
pub fn scan_clap(dirs: &[PathBuf]) -> (Vec<(PathBuf, Vec<ClapPluginInfo>)>, Vec<String>) {
    let mut files = Vec::new();
    for d in dirs {
        walk(d, 0, &mut files);
    }
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for f in files {
        match ClapLibrary::load(&f) {
            Ok(lib) => found.push((f, lib.plugins.clone())),
            Err(e) => errors.push(format!("{}: {e}", f.display())),
        }
    }
    (found, errors)
}

// ---------------------------------------------------------------------------
// VST3
// ---------------------------------------------------------------------------

/// Standard VST3 locations for this platform, plus `$VST3_PATH`.
pub fn default_vst3_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(extra) = std::env::var_os("VST3_PATH") {
        v.extend(std::env::split_paths(&extra));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        if let Some(h) = &home {
            v.push(h.join("Library/Audio/Plug-Ins/VST3"));
        }
        v.push("/Library/Audio/Plug-Ins/VST3".into());
    } else if cfg!(target_os = "windows") {
        if let Some(c) = std::env::var_os("COMMONPROGRAMFILES") {
            v.push(PathBuf::from(c).join("VST3"));
        }
    } else {
        if let Some(h) = &home {
            v.push(h.join(".vst3"));
        }
        v.push("/usr/lib/vst3".into());
        v.push("/usr/local/lib/vst3".into());
    }
    v
}

fn walk_vst3(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "vst3") {
            out.push(p);
        } else if p.is_dir() && depth < 4 {
            walk_vst3(&p, depth + 1, out);
        }
    }
}

pub fn scan_vst3(dirs: &[PathBuf]) -> (Vec<(PathBuf, Vec<crate::Vst3PluginInfo>)>, Vec<String>) {
    let mut files = Vec::new();
    for d in dirs {
        walk_vst3(d, 0, &mut files);
    }
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for f in files {
        match crate::Vst3Library::load(&f) {
            Ok(lib) => found.push((f, lib.plugins.clone())),
            Err(e) => errors.push(format!("{}: {e}", f.display())),
        }
    }
    (found, errors)
}

/// A plugin found on disk, in either format.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub format: loopcraft_core::PluginFormat,
    pub path: PathBuf,
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub is_instrument: bool,
}

/// Scan the default CLAP and VST3 locations.
pub fn scan_all() -> (Vec<PluginInfo>, Vec<String>) {
    use loopcraft_core::PluginFormat;
    let (clap, mut errors) = scan_clap(&default_clap_paths());
    let (vst3, e2) = scan_vst3(&default_vst3_paths());
    errors.extend(e2);
    let mut v: Vec<PluginInfo> = clap
        .into_iter()
        .flat_map(|(path, ps)| {
            ps.into_iter().map(move |p| PluginInfo {
                format: PluginFormat::Clap,
                path: path.clone(),
                id: p.id,
                name: p.name,
                vendor: p.vendor,
                is_instrument: p.is_instrument,
            })
        })
        .collect();
    v.extend(vst3.into_iter().flat_map(|(path, ps)| {
        ps.into_iter().map(move |p| PluginInfo {
            format: PluginFormat::Vst3,
            path: path.clone(),
            id: p.id,
            name: p.name,
            vendor: p.vendor,
            is_instrument: p.is_instrument,
        })
    }));
    v.sort_by_key(|a| a.name.to_lowercase());
    (v, errors)
}
