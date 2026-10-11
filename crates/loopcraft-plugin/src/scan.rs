//! Finding CLAP plugins on disk, using the standard search locations from
//! the CLAP specification plus `$CLAP_PATH`.

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
