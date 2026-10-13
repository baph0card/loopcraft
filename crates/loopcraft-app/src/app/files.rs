//! Project files, audio export, samples on disk.

use super::{App, ClipRef, Dialog, DialogKind};
use loopcraft_core::*;
use loopcraft_engine::Command;
use loopcraft_io::{BitDepth, FileLoader};
use loopcraft_plugin::HostFactory;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;

/// (label, extension, bit depth)
pub const EXPORT_FORMATS: [(&str, &str, BitDepth); 6] = [
    ("WAV 24-bit", "wav", BitDepth::Int24),
    ("WAV 16-bit", "wav", BitDepth::Int16),
    ("WAV 32-bit float", "wav", BitDepth::Float32),
    ("FLAC 24-bit", "flac", BitDepth::Int24),
    ("FLAC 16-bit", "flac", BitDepth::Int16),
    ("AIFF 24-bit", "aiff", BitDepth::Int24),
];

pub const EXPORT_RATES: [u32; 4] = [44_100, 48_000, 88_200, 96_000];

impl App {
    pub fn new_project(&mut self) {
        self.checkpoint();
        let mut p = Project::default();
        for i in 1..=8 {
            p.add_scene(format!("{i}"));
        }
        p.add_return(Track::new("A Reverb", TrackKind::Return).with_effect(DeviceSpec::Reverb(ReverbParams { dry_wet: 1.0, ..Default::default() })));
        p.add_return(Track::new("B Delay", TrackKind::Return).with_effect(DeviceSpec::Delay(DelayParams { dry_wet: 1.0, ..Default::default() })));
        p.add_track(Track::midi("1 Drums", DeviceSpec::DrumRack(Default::default())).with_color(Self::PALETTE[0]));
        p.add_track(Track::midi("2 Synth", DeviceSpec::PolySynth(Default::default())).with_color(Self::PALETTE[2]));
        p.add_track(Track::audio("3 Audio").with_color(Self::PALETTE[4]));
        p.add_track(Track::audio("4 Audio").with_color(Self::PALETTE[5]));
        p.grooves.push(Groove::swing("Swing 16ths 58%", 0.58));
        p.count_in_bars = self.settings.count_in_bars;
        self.send(Command::Stop);
        self.project = p;
        self.path = None;
        self.selected_track = Some(0);
        self.selected_tracks.clear();
        self.selected_clip = None;
        self.automation_view.clear();
        self.loader = Arc::new(FileLoader::new(self.default_dir(), self.sample_rate));
        self.rebuild();
        self.sync_transport();
        self.send(Command::Seek(0.0));
        self.dirty = false;
    }

    /// Push project-level transport settings to the engine.
    pub(crate) fn sync_transport(&mut self) {
        self.send(Command::SetTempo(self.project.tempo));
        self.send(Command::SetQuantization(self.project.quantization));
        self.send(Command::SetLoop(self.project.loop_region));
        self.send(Command::SetMetronome(self.project.metronome));
        self.send(Command::SetCrossfader(self.project.crossfader));
        self.send(Command::SetCueVolume(self.project.cue_volume_db));
        self.send(Command::SetCueMode(self.cue_mode));
    }

    pub fn open(&mut self, path: &Path) {
        match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|s| Project::from_json(&s).map_err(|e| e.to_string())) {
            Ok(p) => {
                self.send(Command::Stop);
                self.undo.clear();
                self.redo.clear();
                self.project = p;
                self.path = Some(path.to_path_buf());
                let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
                self.loader = Arc::new(FileLoader::new(base, self.sample_rate));
                self.selected_track = if self.project.tracks.is_empty() { None } else { Some(0) };
                self.selected_tracks.clear();
                self.selected_clip = None;
                self.automation_view.clear();
                self.rebuild();
                self.sync_transport();
                self.send(Command::Seek(0.0));
                self.dirty = false;
                self.status = format!("Opened {}", path.display());
                self.settings.add_recent(path.to_path_buf());
            }
            Err(e) => self.status = format!("Couldn't open {}: {e}", path.display()),
        }
    }

    /// Every audio file path stored in the project (clips and samplers).
    fn for_each_file(p: &mut Project, f: &mut dyn FnMut(&mut String)) {
        fn spec(s: &mut DeviceSpec, f: &mut dyn FnMut(&mut String)) {
            match s {
                DeviceSpec::Sampler(p) => {
                    if !p.file.is_empty() {
                        f(&mut p.file)
                    }
                }
                DeviceSpec::DrumRack(d) => {
                    for pad in d.pads.iter_mut() {
                        if let PadSource::Sample(file) = &mut pad.source
                            && !file.is_empty()
                        {
                            f(file)
                        }
                    }
                }
                DeviceSpec::MultiSampler(m) => {
                    for z in m.zones.iter_mut() {
                        f(&mut z.file)
                    }
                }
                DeviceSpec::AudioRack(r) | DeviceSpec::InstrumentRack(r) => {
                    for c in r.chains.iter_mut() {
                        for d in c.devices.iter_mut() {
                            spec(d, f)
                        }
                    }
                }
                _ => {}
            }
        }
        for t in p.tracks.iter_mut() {
            for c in t.slots.iter_mut().flatten().chain(t.arrangement.iter_mut().map(|a| &mut a.clip)) {
                if let ClipContent::Audio(a) = &mut c.content {
                    f(&mut a.file);
                }
            }
            if let Some(i) = t.instrument.as_mut() {
                spec(i, f);
            }
        }
    }

    pub fn save_to(&mut self, path: &Path) {
        self.sync_plugin_states();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let new_base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        if new_base != self.default_dir() {
            // Relative paths were relative to the old folder: make them absolute.
            let loader = self.loader.clone();
            Self::for_each_file(&mut self.project, &mut |f| {
                if Path::new(f.as_str()).is_relative() {
                    *f = loader.resolve(f).display().to_string();
                }
            });
        }
        match std::fs::write(path, self.project.to_json()) {
            Ok(()) => {
                self.path = Some(path.to_path_buf());
                self.dirty = false;
                self.status = format!("Saved {}", path.display());
                if self.loader_base_changed(&new_base) {
                    self.loader = Arc::new(FileLoader::new(new_base, self.sample_rate));
                    self.rebuild();
                }
                self.settings.add_recent(path.to_path_buf());
            }
            Err(e) => self.status = format!("Couldn't save: {e}"),
        }
    }

    fn loader_base_changed(&self, base: &Path) -> bool {
        self.loader.resolve("") != base.join("")
    }

    pub fn save(&mut self) {
        match self.path.clone() {
            Some(p) => self.save_to(&p),
            None => self.open_dialog(DialogKind::SaveAs),
        }
    }

    /// Copy every sample from outside the project folder into
    /// `Samples/Imported` and point the project at the copies, then save.
    pub fn collect_all_and_save(&mut self) {
        let Some(path) = self.path.clone() else {
            self.status = "Save the project first".into();
            self.open_dialog(DialogKind::SaveAs);
            return;
        };
        let base = self.default_dir();
        let dest = base.join("Samples").join("Imported");
        let loader = self.loader.clone();
        let mut copied = 0;
        let mut failed = Vec::new();
        self.checkpoint();
        Self::for_each_file(&mut self.project, &mut |f| {
            let src = loader.resolve(f);
            if src.starts_with(&base) {
                return;
            }
            let Some(name) = src.file_name() else { return };
            let _ = std::fs::create_dir_all(&dest);
            let target = dest.join(name);
            if target.exists() || std::fs::copy(&src, &target).is_ok() {
                *f = Path::new("Samples").join("Imported").join(name).display().to_string();
                copied += 1;
            } else {
                failed.push(src.display().to_string());
            }
        });
        self.save_to(&path);
        self.rebuild();
        self.status = if failed.is_empty() {
            format!("Collected {copied} file(s) and saved")
        } else {
            format!("Collected {copied}; couldn't copy {}", failed.join(", "))
        };
    }

    pub fn default_dir(&self) -> PathBuf {
        self.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| self.settings.library())
    }

    pub fn open_dialog(&mut self, kind: DialogKind) {
        let dir = self.default_dir();
        let stem = self.project.name.replace(['/', '\\'], "-");
        let path = match kind {
            DialogKind::Open | DialogKind::SaveAs => self.path.clone().unwrap_or_else(|| dir.join(format!("{stem}.loopcraft"))),
            DialogKind::Export => dir.join(format!("{stem}.wav")),
            DialogKind::SavePreset => PathBuf::from("My Preset"),
        };
        self.dialog = Some(Dialog {
            kind,
            path: path.display().to_string(),
            export_arrangement: self.project.arrangement_length() > 0.0,
            export_stems: false,
            export_format: 0,
            export_rate: 48_000,
            preset_spec: None,
        });
    }

    pub fn open_preset_dialog(&mut self, spec: DeviceSpec) {
        self.open_dialog(DialogKind::SavePreset);
        if let Some(d) = self.dialog.as_mut() {
            d.path = format!("My {}", spec.name());
            d.preset_spec = Some(spec);
        }
    }

    /// Render on a background thread. With `stems`, `path` names a folder
    /// (or a file whose stem names one) and each top-level track gets a file.
    pub fn export(&mut self, path: PathBuf, arrangement: bool, stems: bool, format: usize, rate: u32) {
        self.sync_plugin_states();
        let project = self.project.clone();
        let base = self.default_dir();
        let (tx, rx) = channel();
        self.export_rx = Some(rx);
        self.status = "Exporting…".into();
        let (_, ext, depth) = EXPORT_FORMATS[format.min(EXPORT_FORMATS.len() - 1)];
        let path = path.with_extension(ext);
        std::thread::spawn(move || {
            let bar = project.time_signature.beats_per_bar();
            let mode = if arrangement {
                loopcraft_io::RenderMode::Arrangement { length: None }
            } else {
                loopcraft_io::RenderMode::Scenes((0..project.scenes.len()).map(|s| (s, 8.0 * bar)).collect())
            };
            let opts = loopcraft_io::RenderOptions { sample_rate: rate, mode, tail_seconds: 2.0 };
            let host = HostFactory::new();
            let msg = if stems {
                let dir = path.with_extension("");
                let res = std::fs::create_dir_all(&dir)
                    .map_err(anyhow::Error::from)
                    .and_then(|_| loopcraft_io::render_stems(&project, &base, &opts, &host))
                    .and_then(|stems| {
                        let n = stems.len();
                        for (i, (name, r)) in stems.iter().enumerate() {
                            let safe: String = name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
                            let f = dir.join(format!("{:02} {safe}.{ext}", i + 1));
                            loopcraft_io::write_audio(&f, &r.left, &r.right, rate, depth)?;
                        }
                        Ok(n)
                    });
                match res {
                    Ok(n) => format!("Exported {n} stems to {}", dir.display()),
                    Err(e) => format!("Export failed: {e:#}"),
                }
            } else {
                match loopcraft_io::render(&project, &base, &opts, &host).and_then(|r| {
                    let peak = r.peak();
                    loopcraft_io::write_audio(&path, &r.left, &r.right, rate, depth).map(|_| (r.seconds(), peak))
                }) {
                    Ok((secs, peak)) => {
                        let clip = if peak > 1.0 {
                            format!(" — peak {:+.1} dBFS, clipped", gain_to_db(peak))
                        } else {
                            format!(" — peak {:+.1} dBFS", gain_to_db(peak))
                        };
                        format!("Exported {secs:.1}s to {}{clip}", path.display())
                    }
                    Err(e) => format!("Export failed: {e:#}"),
                }
            };
            let _ = tx.send(msg);
        });
    }

    /// Add an audio file as a warped clip on the selected (or a new) audio track.
    pub fn add_sample_clip(&mut self, file: &str) {
        let t = match self.selected_track.filter(|&t| self.project.tracks.get(t).is_some_and(|t| t.kind == TrackKind::Audio)) {
            Some(t) => t,
            None => self.add_audio_track(),
        };
        let slot = self.project.tracks[t].slots.iter().position(Option::is_none);
        self.add_sample_to(t, slot, file);
    }

    /// Load a file into a specific slot (or the first free one).
    pub fn add_sample_to(&mut self, t: usize, slot: Option<usize>, file: &str) {
        if self.project.tracks.get(t).is_none_or(|tr| tr.kind != TrackKind::Audio) {
            self.status = "Samples go on audio tracks".into();
            return;
        }
        let Some(buf) = loopcraft_dsp::SampleLoader::load(self.loader.as_ref(), file) else {
            self.status = format!("Couldn't load {file}");
            return;
        };
        let secs = buf.len() as f64 / buf.sample_rate as f64;
        let (len, bpm, warp) = guess_tempo(secs, self.project.tempo, self.project.time_signature.beats_per_bar());
        self.checkpoint();
        let slot = slot.unwrap_or_else(|| {
            self.project.add_scene(format!("{}", self.project.scenes.len() + 1));
            self.project.scenes.len() - 1
        });
        let name = Path::new(file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let audio = AudioClip { file: file.to_string(), warp, original_bpm: bpm, ..Default::default() };
        let looping = warp != WarpMode::Off;
        self.project.tracks[t].slots[slot] = Some(Clip { color: self.project.tracks[t].color, looping, ..Clip::audio(name, len, audio) });
        self.rebuild();
        self.selected_clip = Some(ClipRef::Session { track: t, slot });
        self.status = if looping { format!("Added {file} ({len} beats, detected {bpm:.1} bpm)") } else { format!("Added {file} as a one-shot ({secs:.2}s)") };
    }

    /// Place an audio file on the timeline at `beat`.
    pub fn add_sample_to_arrangement(&mut self, t: usize, beat: Beats, file: &str) {
        if self.project.tracks.get(t).is_none_or(|tr| tr.kind != TrackKind::Audio) {
            return;
        }
        let Some(buf) = loopcraft_dsp::SampleLoader::load(self.loader.as_ref(), file) else {
            self.status = format!("Couldn't load {file}");
            return;
        };
        let secs = buf.len() as f64 / buf.sample_rate as f64;
        let (len, bpm, warp) = guess_tempo(secs, self.project.tempo, self.project.time_signature.beats_per_bar());
        self.checkpoint();
        let name = Path::new(file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let audio = AudioClip { file: file.to_string(), warp, original_bpm: bpm, ..Default::default() };
        let clip = Clip { color: self.project.tracks[t].color, looping: false, ..Clip::audio(name, len, audio) };
        self.clear_range(t, beat, beat + len);
        self.project.tracks[t].arrangement.push(ArrangementClip::new(beat, clip));
        self.sort_arrangement(t);
        self.rebuild();
    }

    /// Audio files in the project folder, its `Samples` folder (recursively)
    /// and the library's `Samples` folder, as paths the loader resolves.
    pub fn sample_files(&self) -> Vec<String> {
        let base = self.default_dir();
        let mut v = Vec::new();
        let is_audio = |p: &Path| p.extension().is_some_and(|e| loopcraft_io::AUDIO_EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str()));
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() && is_audio(&p) {
                    v.push(p.file_name().unwrap_or_default().to_string_lossy().into_owned());
                }
            }
        }
        fn walk(dir: &Path, rel: &Path, depth: usize, out: &mut Vec<String>, is_audio: &dyn Fn(&Path) -> bool) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                let p = e.path();
                let r = rel.join(e.file_name());
                if p.is_dir() && depth < 4 {
                    walk(&p, &r, depth + 1, out, is_audio);
                } else if is_audio(&p) {
                    out.push(r.display().to_string());
                }
            }
        }
        walk(&base.join("Samples"), Path::new("Samples"), 0, &mut v, &is_audio);
        let lib = self.settings.library().join("Samples");
        if lib != base.join("Samples") {
            walk(&lib, &lib, 0, &mut v, &is_audio);
        }
        v.sort();
        v.dedup();
        v
    }
}

/// Guess a file's length in beats and tempo. Files that are close to a
/// whole number of bars at a plausible tempo are treated as loops (warped);
/// anything short is a one-shot.
pub fn guess_tempo(secs: f64, song_bpm: f64, bar: f64) -> (Beats, f64, WarpMode) {
    if secs < 1.0 {
        let beats = (secs * song_bpm / 60.0 * 4.0).ceil().max(1.0) / 4.0;
        return (beats, song_bpm, WarpMode::Off);
    }
    let bars = ((secs * song_bpm / 60.0) / bar).round().max(1.0);
    let bpm = bars * bar * 60.0 / secs;
    // Halve / double until it's in a sensible range.
    let (mut b, mut n) = (bpm, bars);
    while b < 70.0 {
        b *= 2.0;
        n *= 2.0;
    }
    while b > 180.0 && n > 1.0 {
        b /= 2.0;
        n /= 2.0;
    }
    (n * bar, b, WarpMode::Stretch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_guess() {
        // 2 bars at 120 bpm = 4 s.
        let (len, bpm, w) = guess_tempo(4.0, 120.0, 4.0);
        assert_eq!((len, w), (8.0, WarpMode::Stretch));
        assert!((bpm - 120.0).abs() < 1e-9);
        // 4 s loop with song at 90: one bar at 60 → doubled to 2 bars at 120.
        let (len, bpm, _) = guess_tempo(4.0, 90.0, 4.0);
        assert!((bpm - 120.0).abs() < 1e-9, "{bpm}");
        assert_eq!(len, 8.0);
        let (_, _, w) = guess_tempo(0.3, 120.0, 4.0);
        assert_eq!(w, WarpMode::Off);
    }
}
