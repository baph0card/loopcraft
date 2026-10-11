//! Application state and every action that changes the project.
//!
//! Rule of thumb: the `Project` is the source of truth. Small changes
//! (fader moves, parameter tweaks, note edits) update the project *and* send
//! a targeted command to the engine. Structural changes (adding tracks or
//! devices) update the project and rebuild the engine graph, which is
//! swapped in without stopping playback.

use crate::audio::{self, DeviceInfo, Output};
use loopcraft_core::*;
use loopcraft_engine::{Command, Core, DeviceRef, EngineHandle, RecordBuffer, RtClip, TrackRef, create};
use loopcraft_io::FileLoader;
use loopcraft_plugin::{ClapPluginInfo, HostFactory};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Session,
    Arrangement,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClipRef {
    Session { track: usize, slot: usize },
    Arrangement { track: usize, index: usize },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Clip,
    Devices,
}

#[derive(Clone, PartialEq, Eq)]
pub enum DialogKind {
    Open,
    SaveAs,
    Export,
}

pub struct Dialog {
    pub kind: DialogKind,
    pub path: String,
    pub export_arrangement: bool,
}

pub struct App {
    pub project: Project,
    pub path: Option<PathBuf>,
    pub engine: EngineHandle,
    pub sample_rate: f32,
    #[allow(dead_code)] // kept for a future audio-settings panel
    pub device: DeviceInfo,
    pub device_label: String,
    _output: Output,
    pub loader: Arc<FileLoader>,
    pub plugins: HostFactory,
    pub plugin_list: Vec<(PathBuf, ClapPluginInfo)>,

    pub view: View,
    pub selected_track: Option<usize>,
    pub selected_clip: Option<ClipRef>,
    pub detail_tab: DetailTab,
    pub status: String,
    pub dialog: Option<Dialog>,
    pub dirty: bool,
    undo: Vec<Project>,
    redo: Vec<Project>,

    pub computer_keys: bool,
    pub octave: i32,
    pub held_notes: Vec<(eframe::egui::Key, u8)>,

    pub arr_pixels_per_beat: f32,
    pub piano_grid: Beats,
    pub piano_selected: Option<usize>,
    pub export_rx: Option<Receiver<String>>,
    /// Channel whose device chain the Devices tab shows.
    pub device_target: TrackRef,
    /// (note index, resizing?, note before drag, beat and pitch at press)
    pub piano_drag: Option<(usize, bool, Note, f64, i32)>,
    /// (track, arrangement index, start before drag, beat at press)
    pub arr_drag: Option<(usize, usize, f64, f64)>,
    pub renaming: Option<(Rename, String)>,
    /// Lowest visible pitch in the piano roll, per clip, so the view doesn't
    /// jump while notes are added. Scroll to move it.
    pub piano_view: Option<(ClipRef, i32)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rename {
    Track(usize),
    Scene(usize),
}

impl App {
    pub fn new(project: Project, path: Option<PathBuf>) -> Self {
        let device = audio::probe();
        let sr = device.sample_rate;
        let base = path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let loader = Arc::new(FileLoader::new(base, sr));
        let plugins = HostFactory::new();
        let core = Core::from_project(&project, sr, loader.as_ref(), &plugins);
        let (engine, handle) = create(core);
        let (output, label) = audio::start(&device, engine);
        let mut app = Self {
            project,
            path,
            engine: handle,
            sample_rate: sr,
            device,
            device_label: label,
            _output: output,
            loader,
            plugins,
            plugin_list: Vec::new(),
            view: View::Session,
            selected_track: None,
            selected_clip: None,
            detail_tab: DetailTab::Clip,
            status: String::new(),
            dialog: None,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            computer_keys: true,
            octave: 0,
            held_notes: Vec::new(),
            arr_pixels_per_beat: 14.0,
            piano_grid: 0.25,
            piano_selected: None,
            export_rx: None,
            device_target: TrackRef::Track(0),
            piano_drag: None,
            arr_drag: None,
            renaming: None,
            piano_view: None,
        };
        app.report_load_errors();
        if !app.project.tracks.is_empty() {
            app.selected_track = Some(0);
        }
        app
    }

    fn report_load_errors(&mut self) {
        let errs: Vec<String> = self.loader.errors().into_iter().chain(self.plugins.errors()).collect();
        if let Some(e) = errs.last() {
            self.status = format!("Couldn't load {e}");
        }
    }

    pub fn send(&mut self, cmd: Command) {
        if self.engine.send(cmd).is_err() {
            self.status = "Engine command queue full".into();
        }
    }

    /// Call once per frame: collect finished recordings, export results.
    pub fn poll(&mut self) {
        for rec in self.engine.poll() {
            let (t, s) = (rec.track, rec.slot);
            if t < self.project.tracks.len() && s < self.project.scenes.len() {
                self.checkpoint();
                let clip = Clip { name: "Recording".into(), color: self.project.tracks[t].color, ..Clip::midi("Recording", rec.length, rec.notes.clone()) };
                self.project.tracks[t].slots[s] = Some(clip);
                self.push_clip(t, s);
                // Loop the new clip from its natural start, seamlessly.
                let at = rec.start + rec.length;
                self.send(Command::LaunchClipAt { track: t, slot: s, at });
                self.selected_clip = Some(ClipRef::Session { track: t, slot: s });
                self.status = format!("Recorded {} notes", rec.notes.len());
            }
        }
        if let Some(rx) = &self.export_rx {
            if let Ok(msg) = rx.try_recv() {
                self.status = msg;
                self.export_rx = None;
            }
        }
    }

    // ------------------------------------------------------------------
    // Undo / structure
    // ------------------------------------------------------------------

    pub fn checkpoint(&mut self) {
        self.undo.push(self.project.clone());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    pub fn undo(&mut self) {
        if let Some(p) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.project, p));
            self.after_history_change();
            self.status = "Undo".into();
        }
    }

    pub fn redo(&mut self) {
        if let Some(p) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.project, p));
            self.after_history_change();
            self.status = "Redo".into();
        }
    }

    fn after_history_change(&mut self) {
        let n = self.project.tracks.len();
        if self.selected_track.is_some_and(|t| t >= n) {
            self.selected_track = n.checked_sub(1);
        }
        self.selected_clip = self.selected_clip.filter(|c| self.clip(*c).is_some());
        self.rebuild();
        self.send(Command::SetTempo(self.project.tempo));
    }

    /// Rebuild the engine graph from the project and swap it in.
    pub fn rebuild(&mut self) {
        let core = Core::from_project(&self.project, self.sample_rate, self.loader.as_ref(), &self.plugins);
        self.send(Command::SwapCore(Box::new(core)));
        self.report_load_errors();
    }

    /// Send one session clip to the engine without rebuilding.
    pub fn push_clip(&mut self, track: usize, slot: usize) {
        let clip = self
            .project
            .tracks
            .get(track)
            .and_then(|t| t.slots.get(slot))
            .and_then(|c| c.as_ref())
            .map(|c| Box::new(RtClip::from_clip(c, self.loader.as_ref())));
        self.send(Command::ReplaceClip { track, slot, clip });
    }

    /// Push an edited clip, wherever it lives.
    pub fn commit_clip(&mut self, r: ClipRef) {
        self.dirty = true;
        match r {
            ClipRef::Session { track, slot } => self.push_clip(track, slot),
            ClipRef::Arrangement { .. } => self.rebuild(),
        }
    }

    pub fn clip(&self, r: ClipRef) -> Option<&Clip> {
        match r {
            ClipRef::Session { track, slot } => self.project.tracks.get(track)?.slots.get(slot)?.as_ref(),
            ClipRef::Arrangement { track, index } => self.project.tracks.get(track)?.arrangement.get(index).map(|a| &a.clip),
        }
    }

    pub fn clip_mut(&mut self, r: ClipRef) -> Option<&mut Clip> {
        match r {
            ClipRef::Session { track, slot } => self.project.tracks.get_mut(track)?.slots.get_mut(slot)?.as_mut(),
            ClipRef::Arrangement { track, index } => self.project.tracks.get_mut(track)?.arrangement.get_mut(index).map(|a| &mut a.clip),
        }
    }

    const PALETTE: [[u8; 3]; 8] = [
        [0xf0, 0x80, 0x50],
        [0x90, 0xe0, 0x70],
        [0x70, 0xa0, 0xf0],
        [0xe0, 0x60, 0xc0],
        [0xf0, 0xd0, 0x50],
        [0x60, 0xd0, 0xd0],
        [0xb0, 0x90, 0xf0],
        [0xf0, 0x70, 0x70],
    ];

    pub fn add_midi_track(&mut self, instrument: DeviceSpec) -> usize {
        self.checkpoint();
        let n = self.project.tracks.len();
        let name = format!("{} {}", instrument.name(), n + 1);
        if self.project.scenes.is_empty() {
            self.project.add_scene("1");
        }
        let i = self.project.add_track(Track::midi(name, instrument).with_color(Self::PALETTE[n % 8]));
        self.selected_track = Some(i);
        self.rebuild();
        i
    }

    pub fn add_audio_track(&mut self) -> usize {
        self.checkpoint();
        let n = self.project.tracks.len();
        if self.project.scenes.is_empty() {
            self.project.add_scene("1");
        }
        let i = self.project.add_track(Track::audio(format!("Audio {}", n + 1)).with_color(Self::PALETTE[n % 8]));
        self.selected_track = Some(i);
        self.rebuild();
        i
    }

    pub fn add_return(&mut self) {
        self.checkpoint();
        let letter = (b'A' + self.project.returns.len() as u8) as char;
        self.project.add_return(
            Track::new(format!("{letter} Return"), TrackKind::Return).with_effect(DeviceSpec::Reverb(ReverbParams { dry_wet: 1.0, ..Default::default() })),
        );
        self.rebuild();
    }

    pub fn add_scene(&mut self) {
        self.checkpoint();
        let n = self.project.scenes.len() + 1;
        self.project.add_scene(format!("{n}"));
        self.rebuild();
    }

    pub fn insert_scene(&mut self, at: usize) {
        self.checkpoint();
        let at = at.min(self.project.scenes.len());
        self.project.scenes.insert(at, Scene { name: format!("{}", at + 1), ..Default::default() });
        for t in self.project.tracks.iter_mut() {
            if at <= t.slots.len() {
                t.slots.insert(at, None);
            }
        }
        self.rebuild();
    }

    pub fn delete_scene(&mut self, s: usize) {
        if s >= self.project.scenes.len() {
            return;
        }
        self.checkpoint();
        self.project.scenes.remove(s);
        for t in self.project.tracks.iter_mut() {
            if s < t.slots.len() {
                t.slots.remove(s);
            }
        }
        self.selected_clip = None;
        self.rebuild();
    }

    pub fn delete_track(&mut self, t: usize) {
        if t >= self.project.tracks.len() {
            return;
        }
        self.checkpoint();
        self.project.tracks.remove(t);
        // Fix sidechain references pointing at or past the removed track.
        for tr in self.project.tracks.iter_mut() {
            for fx in tr.effects.iter_mut() {
                if let DeviceSpec::Compressor(c) = fx {
                    c.sidechain_track = match c.sidechain_track {
                        Some(i) if i == t => None,
                        Some(i) if i > t => Some(i - 1),
                        other => other,
                    };
                }
            }
        }
        self.selected_track = None;
        self.selected_clip = None;
        self.rebuild();
    }

    pub fn duplicate_track(&mut self, t: usize) {
        let Some(track) = self.project.tracks.get(t).cloned() else { return };
        self.checkpoint();
        let mut copy = track;
        copy.name = format!("{} copy", copy.name);
        copy.mixer.solo = false;
        self.project.tracks.insert(t + 1, copy);
        self.selected_track = Some(t + 1);
        self.rebuild();
    }

    pub fn new_midi_clip(&mut self, track: usize, slot: usize) {
        if self.project.tracks.get(track).is_none_or(|t| t.kind != TrackKind::Midi) {
            return;
        }
        self.checkpoint();
        let color = self.project.tracks[track].color;
        self.project.tracks[track].slots[slot] = Some(Clip { color, ..Clip::midi("MIDI Clip", 4.0, Vec::new()) });
        self.push_clip(track, slot);
        self.selected_clip = Some(ClipRef::Session { track, slot });
        self.detail_tab = DetailTab::Clip;
    }

    pub fn delete_clip(&mut self, r: ClipRef) {
        self.checkpoint();
        match r {
            ClipRef::Session { track, slot } => {
                if let Some(s) = self.project.tracks.get_mut(track).and_then(|t| t.slots.get_mut(slot)) {
                    *s = None;
                }
                self.push_clip(track, slot);
            }
            ClipRef::Arrangement { track, index } => {
                if let Some(t) = self.project.tracks.get_mut(track) {
                    if index < t.arrangement.len() {
                        t.arrangement.remove(index);
                    }
                }
                self.rebuild();
            }
        }
        if self.selected_clip == Some(r) {
            self.selected_clip = None;
        }
    }

    pub fn duplicate_clip_down(&mut self, track: usize, slot: usize) {
        let Some(clip) = self.project.tracks.get(track).and_then(|t| t.slots.get(slot)).cloned().flatten() else { return };
        self.checkpoint();
        if slot + 1 >= self.project.scenes.len() {
            self.project.add_scene(format!("{}", self.project.scenes.len() + 1));
            self.project.tracks[track].slots[slot + 1] = Some(clip);
            self.rebuild();
        } else {
            self.project.tracks[track].slots[slot + 1] = Some(clip);
            self.push_clip(track, slot + 1);
        }
    }

    /// Copy a session clip onto the timeline at the playhead (bar-aligned).
    pub fn clip_to_arrangement(&mut self, track: usize, slot: usize) {
        let Some(clip) = self.project.tracks.get(track).and_then(|t| t.slots.get(slot)).cloned().flatten() else { return };
        self.checkpoint();
        let bar = self.project.time_signature.beats_per_bar();
        let at = (self.engine.state.beat.load() / bar).floor() * bar;
        self.project.tracks[track].arrangement.push(ArrangementClip::new(at, clip));
        self.project.tracks[track].arrangement.sort_by(|a, b| a.start.total_cmp(&b.start));
        self.rebuild();
        self.status = format!("Copied to Arrangement at bar {}", (at / bar) as i64 + 1);
    }

    pub fn effects_mut(&mut self, ch: TrackRef) -> Option<&mut Vec<DeviceSpec>> {
        match ch {
            TrackRef::Track(i) => self.project.tracks.get_mut(i).map(|t| &mut t.effects),
            TrackRef::Return(i) => self.project.returns.get_mut(i).map(|t| &mut t.effects),
            TrackRef::Master => Some(&mut self.project.master.effects),
        }
    }

    pub fn add_device(&mut self, ch: TrackRef, spec: DeviceSpec) {
        if spec.is_instrument() {
            let TrackRef::Track(t) = ch else {
                self.status = "Instruments go on MIDI tracks".into();
                return;
            };
            if self.project.tracks.get(t).is_none_or(|t| t.kind != TrackKind::Midi) {
                self.status = "Instruments go on MIDI tracks".into();
                return;
            }
            self.checkpoint();
            self.project.tracks[t].instrument = Some(spec);
        } else {
            if self.effects_mut(ch).is_none() {
                return;
            }
            self.checkpoint();
            if let Some(fx) = self.effects_mut(ch) {
                fx.push(spec);
            }
        }
        self.device_target = ch;
        self.detail_tab = DetailTab::Devices;
        self.rebuild();
    }

    pub fn remove_effect(&mut self, ch: TrackRef, index: usize) {
        if self.effects_mut(ch).is_none_or(|fx| index >= fx.len()) {
            return;
        }
        self.checkpoint();
        if let Some(fx) = self.effects_mut(ch) {
            fx.remove(index);
        }
        self.rebuild();
    }

    pub fn move_effect(&mut self, ch: TrackRef, index: usize, left: bool) {
        let Some(len) = self.effects_mut(ch).map(|f| f.len()) else { return };
        let j = if left { index.checked_sub(1) } else { Some(index + 1).filter(|&j| j < len) };
        if let Some(j) = j {
            self.checkpoint();
            if let Some(fx) = self.effects_mut(ch) {
                fx.swap(index, j);
            }
            self.rebuild();
        }
    }

    /// Change a device parameter: write it into the project and the engine.
    pub fn set_param(&mut self, channel: TrackRef, device: DeviceRef, param: usize, value: f32) {
        let spec = match (channel, device) {
            (TrackRef::Track(t), DeviceRef::Instrument) => self.project.tracks.get_mut(t).and_then(|t| t.instrument.as_mut()),
            (TrackRef::Track(t), DeviceRef::Effect(e)) => self.project.tracks.get_mut(t).and_then(|t| t.effects.get_mut(e)),
            (TrackRef::Return(r), DeviceRef::Effect(e)) => self.project.returns.get_mut(r).and_then(|t| t.effects.get_mut(e)),
            (TrackRef::Master, DeviceRef::Effect(e)) => self.project.master.effects.get_mut(e),
            _ => None,
        };
        if let Some(spec) = spec {
            loopcraft_dsp::specs::set_param(spec, param, value);
            self.dirty = true;
        }
        self.send(Command::SetDeviceParam { channel, device, param, value });
    }

    pub fn mixer_mut(&mut self, ch: TrackRef) -> Option<&mut MixerSettings> {
        match ch {
            TrackRef::Track(i) => self.project.tracks.get_mut(i).map(|t| &mut t.mixer),
            TrackRef::Return(i) => self.project.returns.get_mut(i).map(|t| &mut t.mixer),
            TrackRef::Master => Some(&mut self.project.master.mixer),
        }
    }

    pub fn record_midi(&mut self) {
        let Some(t) = self.selected_track else {
            self.status = "Select a MIDI track to record into".into();
            return;
        };
        if self.project.tracks.get(t).is_none_or(|tr| tr.kind != TrackKind::Midi) {
            self.status = "Select a MIDI track to record into".into();
            return;
        }
        let slot = match self.selected_clip {
            Some(ClipRef::Session { track, slot }) if track == t && self.project.tracks[t].slots[slot].is_none() => Some(slot),
            _ => self.project.tracks[t].slots.iter().position(Option::is_none),
        };
        let slot = match slot {
            Some(s) => s,
            None => {
                self.add_scene();
                self.project.scenes.len() - 1
            }
        };
        let bars = 4.0 * self.project.time_signature.beats_per_bar();
        self.send(Command::RecordMidi(RecordBuffer::new(t, slot, bars)));
        self.status = format!("Recording 4 bars into {} slot {} from the next bar — play the keys A–K", self.project.tracks[t].name, slot + 1);
    }

    // ------------------------------------------------------------------
    // Files
    // ------------------------------------------------------------------

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
        self.send(Command::Stop);
        self.project = p;
        self.path = None;
        self.selected_track = Some(0);
        self.selected_clip = None;
        self.rebuild();
        self.send(Command::SetTempo(self.project.tempo));
        self.send(Command::Seek(0.0));
        self.dirty = false;
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
                self.selected_clip = None;
                self.rebuild();
                self.send(Command::SetTempo(self.project.tempo));
                self.send(Command::SetQuantization(self.project.quantization));
                self.send(Command::SetLoop(self.project.loop_region));
                self.send(Command::Seek(0.0));
                self.dirty = false;
                self.status = format!("Opened {}", path.display());
            }
            Err(e) => self.status = format!("Couldn't open {}: {e}", path.display()),
        }
    }

    pub fn save_to(&mut self, path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(path, self.project.to_json()) {
            Ok(()) => {
                self.path = Some(path.to_path_buf());
                self.dirty = false;
                self.status = format!("Saved {}", path.display());
            }
            Err(e) => self.status = format!("Couldn't save: {e}"),
        }
    }

    pub fn save(&mut self) {
        match self.path.clone() {
            Some(p) => self.save_to(&p),
            None => self.open_dialog(DialogKind::SaveAs),
        }
    }

    pub fn default_dir(&self) -> PathBuf {
        self.path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("LoopCraft")).unwrap_or_else(|| PathBuf::from(".")))
    }

    pub fn open_dialog(&mut self, kind: DialogKind) {
        let dir = self.default_dir();
        let stem = self.project.name.replace(['/', '\\'], "-");
        let path = match kind {
            DialogKind::Open => self.path.clone().unwrap_or_else(|| dir.join(format!("{stem}.loopcraft"))),
            DialogKind::SaveAs => self.path.clone().unwrap_or_else(|| dir.join(format!("{stem}.loopcraft"))),
            DialogKind::Export => dir.join(format!("{stem}.wav")),
        };
        self.dialog = Some(Dialog { kind, path: path.display().to_string(), export_arrangement: self.project.arrangement_length() > 0.0 });
    }

    /// Render to WAV on a background thread.
    pub fn export(&mut self, path: PathBuf, arrangement: bool) {
        let project = self.project.clone();
        let base = self.default_dir();
        let (tx, rx) = channel();
        self.export_rx = Some(rx);
        self.status = "Exporting…".into();
        std::thread::spawn(move || {
            let bar = project.time_signature.beats_per_bar();
            let mode = if arrangement {
                loopcraft_io::RenderMode::Arrangement { length: None }
            } else {
                loopcraft_io::RenderMode::Scenes((0..project.scenes.len()).map(|s| (s, 8.0 * bar)).collect())
            };
            let opts = loopcraft_io::RenderOptions { sample_rate: 48_000, mode, tail_seconds: 2.0 };
            let msg = match loopcraft_io::render(&project, &base, &opts, &HostFactory::new())
                .and_then(|r| loopcraft_io::write_wav(&path, &r.left, &r.right, 48_000, loopcraft_io::BitDepth::Int24).map(|_| r.seconds()))
            {
                Ok(secs) => format!("Exported {secs:.1}s to {}", path.display()),
                Err(e) => format!("Export failed: {e:#}"),
            };
            let _ = tx.send(msg);
        });
    }

    /// Add a WAV file as a warped audio clip on the selected (or a new) audio track.
    pub fn add_sample_clip(&mut self, file: &str) {
        let t = match self.selected_track.filter(|&t| self.project.tracks.get(t).is_some_and(|t| t.kind == TrackKind::Audio)) {
            Some(t) => t,
            None => self.add_audio_track(),
        };
        let Some(buf) = loopcraft_dsp::SampleLoader::load(self.loader.as_ref(), file) else {
            self.status = format!("Couldn't load {file}");
            return;
        };
        let secs = buf.len() as f64 / buf.sample_rate as f64;
        // Guess the loop's tempo: the bpm nearest the song tempo that makes
        // the file a whole number of bars.
        let bar = self.project.time_signature.beats_per_bar();
        let bars = ((secs * self.project.tempo / 60.0) / bar).round().max(1.0);
        let bpm = bars * bar * 60.0 / secs;
        let slot = self.project.tracks[t].slots.iter().position(Option::is_none);
        self.checkpoint();
        let slot = slot.unwrap_or_else(|| {
            self.project.add_scene(format!("{}", self.project.scenes.len() + 1));
            self.project.scenes.len() - 1
        });
        let name = Path::new(file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let audio = AudioClip { file: file.to_string(), warp: WarpMode::Stretch, original_bpm: bpm, ..Default::default() };
        self.project.tracks[t].slots[slot] = Some(Clip { color: self.project.tracks[t].color, ..Clip::audio(name, bars * bar, audio) });
        self.rebuild();
        self.selected_clip = Some(ClipRef::Session { track: t, slot });
        self.status = format!("Added {file} ({bars} bars, detected {bpm:.1} bpm)");
    }

    pub fn sample_files(&self) -> Vec<String> {
        let dir = self.default_dir();
        let mut v: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let p = e.path();
                        let ext = p.extension()?.to_string_lossy().to_lowercase();
                        (ext == "wav").then(|| p.file_name().unwrap().to_string_lossy().into_owned())
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
}
