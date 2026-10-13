//! Application state and every action that changes the project.
//!
//! Rule of thumb: the `Project` is the source of truth. Small changes
//! (fader moves, parameter tweaks, note edits) update the project *and* send
//! a targeted command to the engine. Structural changes (adding tracks or
//! devices) update the project and rebuild the engine graph, which is
//! swapped in without stopping playback.

mod clips;
mod devices;
mod files;
mod midi_in;
mod notes;
mod plugins;
mod record;
mod settings;
#[cfg(test)]
mod tests;
mod tracks;

pub use devices::nested_mut;
pub use files::{EXPORT_FORMATS, EXPORT_RATES};
pub use midi_in::{MidiIn, is_button};
pub use notes::{PianoDrag, PianoDragMode};
pub use record::PendingAudio;
pub use settings::Settings;

use crate::audio::{self, DeviceInfo, Output};
use loopcraft_core::*;
use loopcraft_engine::{Command, Core, EngineHandle, RtClip, TrackRef, create};
use loopcraft_io::FileLoader;
use loopcraft_plugin::{HostFactory, PluginInfo};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;

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
    SavePreset,
}

pub struct Dialog {
    pub kind: DialogKind,
    pub path: String,
    pub export_arrangement: bool,
    pub export_stems: bool,
    pub export_format: usize,
    pub export_rate: u32,
    /// For presets: which device.
    pub preset_spec: Option<DeviceSpec>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rename {
    Track(usize),
    Scene(usize),
    Return(usize),
}

/// Controller-mapping modes (MIDI learn / key map).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapMode {
    Midi,
    Key,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArrDragMode {
    Move,
    /// Right edge.
    Resize,
    /// Left edge.
    Trim,
}

/// An arrangement region being dragged.
#[derive(Clone, Copy, Debug)]
pub struct ArrDrag {
    pub track: usize,
    pub index: usize,
    pub orig_start: f64,
    pub orig_dur: f64,
    pub orig_offset: f64,
    pub press_beat: f64,
    pub mode: ArrDragMode,
}

/// A macro mapping being made: click "Map" on a macro, then a parameter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MacroLearn {
    pub channel: TrackRef,
    pub device: loopcraft_engine::DeviceRef,
    pub macro_index: usize,
}

pub struct App {
    pub project: Project,
    pub path: Option<PathBuf>,
    pub engine: EngineHandle,
    pub sample_rate: f32,
    pub device: DeviceInfo,
    pub device_label: String,
    output: Option<Output>,
    pub loader: Arc<FileLoader>,
    pub plugins: HostFactory,
    pub plugin_list: Vec<PluginInfo>,
    pub settings: Settings,
    pub midi: MidiIn,

    pub view: View,
    pub selected_track: Option<usize>,
    /// Extra selected tracks (Ctrl/Shift-click), for grouping.
    pub selected_tracks: Vec<usize>,
    pub selected_scene: usize,
    pub selected_clip: Option<ClipRef>,
    pub detail_tab: DetailTab,
    pub status: String,
    pub dialog: Option<Dialog>,
    pub show_settings: bool,
    pub show_grooves: bool,
    pub dirty: bool,
    undo: Vec<Project>,
    redo: Vec<Project>,

    pub computer_keys: bool,
    pub octave: i32,
    pub held_notes: Vec<(eframe::egui::Key, u8)>,

    pub arr_pixels_per_beat: f32,
    pub arr_follow: bool,
    /// Which automation lane each track shows in the Arrangement.
    pub automation_view: BTreeMap<usize, AutomationTarget>,
    pub show_tempo_lane: bool,
    pub piano_grid: Beats,
    /// Selected notes (indices into the edited clip's notes).
    pub piano_sel: std::collections::BTreeSet<usize>,
    pub piano_sel_owner: Option<ClipRef>,
    pub note_clipboard: Vec<Note>,
    /// Ctrl+V pastes notes (true) or a clip (false): whichever was copied last.
    pub notes_copied_last: bool,
    /// Last clicked position in the piano roll (beats), where pastes land.
    pub piano_cursor: f64,
    /// Show only pitches that have notes.
    pub piano_fold: bool,
    /// Highlight a scale in the piano roll: (root 0..11, scale).
    pub piano_scale: Option<(u8, ScaleKind)>,
    pub export_rx: Option<Receiver<String>>,
    /// Channel whose device chain the Devices tab shows.
    pub device_target: TrackRef,
    pub piano_drag: Option<PianoDrag>,
    pub arr_drag: Option<ArrDrag>,
    pub renaming: Option<(Rename, String)>,
    /// Lowest visible pitch in the piano roll, per clip, so the view doesn't
    /// jump while notes are added. Scroll to move it.
    pub piano_view: Option<(ClipRef, i32)>,
    pub browser_search: String,
    pub map_mode: Option<MapMode>,
    pub map_target: Option<MappingTarget>,
    pub macro_learn: Option<MacroLearn>,
    pub tap_times: Vec<f64>,
    pub pending_audio: Vec<PendingAudio>,
    pub cue_mode: bool,
    pub arrangement_recording: bool,
    /// Device boxes collapsed in the chain view, by (channel, device index).
    pub collapsed_devices: Vec<(TrackRef, loopcraft_engine::DeviceRef)>,
    /// Rack chain shown expanded, per rack device.
    pub open_chain: BTreeMap<String, usize>,
    pub time_started: std::time::Instant,
    pub arr_rec_started: std::time::Instant,
    /// A spec-only edit (e.g. inside a rack) needs a rebuild once the mouse is released.
    pub rebuild_pending: bool,
    /// Copied session clip.
    pub clipboard: Option<Clip>,
    /// A Gate-mode clip held down with the mouse.
    pub gate_held: Option<(usize, usize)>,
    /// Velocity for computer-keyboard notes.
    pub key_velocity: u8,
    pub show_mappings: bool,
    /// Loop brace to restore when Loop is switched back on.
    pub last_loop: Option<(Beats, Beats)>,
    pub auto_drag: Option<(usize, AutomationTarget, usize)>,
    pub tempo_drag: Option<usize>,
    pub warp_drag: Option<usize>,
    pub arr_scroll_x: f32,
    pub loop_drag: Option<Beats>,
    /// (outputs, inputs) for the Settings window.
    pub device_list: Option<(Vec<String>, Vec<String>)>,
    /// Audio settings being edited in the Settings window.
    pub audio_edit: crate::audio::AudioSettings,
    /// Browser clicks add into this rack chain: (channel, rack device, chain).
    pub rack_insert: Option<(TrackRef, loopcraft_engine::DeviceRef, usize)>,
    /// Session clips being recorded into the Arrangement: track -> (slot, clip offset, start beat).
    pub session_rec: BTreeMap<usize, (usize, f64, f64)>,
    /// Comping drag on a take lane: (track, lane, start beat).
    pub comp_drag: Option<(usize, usize, f64)>,
}

impl App {
    pub fn new(project: Project, path: Option<PathBuf>) -> Self {
        let settings = Settings::load();
        let midi = MidiIn::open(&settings.midi_inputs);
        Self::with(project, path, settings, midi, true)
    }

    /// No audio device, no MIDI, default settings: for tests.
    #[cfg(test)]
    pub fn headless(project: Project) -> Self {
        let mut s = Settings { persist: false, ..Default::default() };
        s.library_dir = Some(std::env::temp_dir().join(format!("loopcraft-test-{}", std::process::id())));
        Self::with(project, None, s, MidiIn::none(), false)
    }

    fn with(project: Project, path: Option<PathBuf>, settings: Settings, midi: MidiIn, use_device: bool) -> Self {
        let (device, out_dev, in_dev) = if use_device { audio::probe(&settings.audio) } else { audio::probe_none() };
        let sr = device.sample_rate;
        let base = path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| settings.library());
        let loader = Arc::new(FileLoader::new(base, sr));
        let plugins = HostFactory::new();
        let mut project = project;
        project.assign_plugin_instances();
        let core = Core::from_project(&project, sr, loader.as_ref(), &plugins);
        let (engine, handle) = create(core);
        let (output, label) = audio::start(&device, out_dev, in_dev, engine);
        let mut app = Self {
            project,
            path,
            engine: handle,
            sample_rate: sr,
            device,
            device_label: label,
            output: Some(output),
            loader,
            plugins,
            plugin_list: Vec::new(),
            settings,
            midi,
            view: View::Session,
            selected_track: None,
            selected_tracks: Vec::new(),
            selected_scene: 0,
            selected_clip: None,
            detail_tab: DetailTab::Clip,
            status: String::new(),
            dialog: None,
            show_settings: false,
            show_grooves: false,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            computer_keys: true,
            octave: 0,
            held_notes: Vec::new(),
            arr_pixels_per_beat: 14.0,
            arr_follow: true,
            automation_view: BTreeMap::new(),
            show_tempo_lane: false,
            piano_grid: 0.25,
            piano_sel: Default::default(),
            piano_sel_owner: None,
            note_clipboard: Vec::new(),
            notes_copied_last: false,
            piano_cursor: 0.0,
            piano_fold: false,
            piano_scale: None,
            export_rx: None,
            device_target: TrackRef::Track(0),
            piano_drag: None,
            arr_drag: None,
            renaming: None,
            piano_view: None,
            browser_search: String::new(),
            map_mode: None,
            map_target: None,
            macro_learn: None,
            tap_times: Vec::new(),
            pending_audio: Vec::new(),
            cue_mode: false,
            arrangement_recording: false,
            collapsed_devices: Vec::new(),
            open_chain: BTreeMap::new(),
            time_started: std::time::Instant::now(),
            arr_rec_started: std::time::Instant::now(),
            rebuild_pending: false,
            clipboard: None,
            gate_held: None,
            key_velocity: 100,
            show_mappings: false,
            last_loop: None,
            auto_drag: None,
            tempo_drag: None,
            warp_drag: None,
            arr_scroll_x: 0.0,
            loop_drag: None,
            device_list: None,
            audio_edit: Default::default(),
            rack_insert: None,
            session_rec: BTreeMap::new(),
            comp_drag: None,
        };
        app.audio_edit = app.settings.audio.clone();
        app.report_load_errors();
        app.sync_transport();
        if !app.project.tracks.is_empty() {
            app.selected_track = Some(0);
        }
        app
    }

    /// Play, with the count-in when recording is armed to start.
    pub fn play(&mut self) {
        self.send(Command::Play);
    }

    pub fn open_settings(&mut self) {
        self.show_settings = true;
        self.audio_edit = self.settings.audio.clone();
        if self.device_list.is_none() {
            let d = audio::list_devices();
            self.device_list = Some((d.outputs, d.inputs));
        }
    }

    /// Call once per frame after the UI ran: apply deferred rebuilds.
    pub fn end_frame(&mut self, pointer_down: bool) {
        if self.rebuild_pending && !pointer_down {
            self.rebuild_pending = false;
            self.rebuild();
        }
    }

    /// Restart audio with the current settings (device, rate, buffer).
    pub fn restart_audio(&mut self) {
        self.send(Command::Stop);
        // Drop the old stream first so the device is free.
        self.output = None;
        let (device, out_dev, in_dev) = audio::probe(&self.settings.audio);
        self.sample_rate = device.sample_rate;
        self.loader = Arc::new(FileLoader::new(self.default_dir(), self.sample_rate));
        self.project.assign_plugin_instances();
        let core = Core::from_project(&self.project, self.sample_rate, self.loader.as_ref(), &self.plugins);
        let (engine, handle) = create(core);
        let (output, label) = audio::start(&device, out_dev, in_dev, engine);
        self.engine = handle;
        self.output = Some(output);
        self.device = device;
        self.device_label = label;
        self.send(Command::SetCueMode(self.cue_mode));
        self.settings.save();
        self.status = format!("Audio: {}", self.device_label);
    }

    pub(crate) fn report_load_errors(&mut self) {
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

    pub fn playing(&self) -> bool {
        self.engine.state.playing.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Call once per frame: collect recordings, MIDI input, export results.
    pub fn poll(&mut self) {
        self.poll_recordings();
        self.poll_midi();
        self.poll_plugins();
        if let Some(rx) = &self.export_rx
            && let Ok(msg) = rx.try_recv()
        {
            self.status = msg;
            self.export_rx = None;
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
        self.selected_tracks.retain(|&t| t < n);
        self.selected_clip = self.selected_clip.filter(|c| self.clip(*c).is_some());
        self.rebuild();
        self.send(Command::SetTempo(self.project.tempo));
    }

    /// Rebuild the engine graph from the project and swap it in.
    pub fn rebuild(&mut self) {
        self.project.assign_plugin_instances();
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
            .map(|c| Box::new(RtClip::from_clip_with(c, self.loader.as_ref(), &self.project.grooves)));
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

    pub fn mixer_mut(&mut self, ch: TrackRef) -> Option<&mut MixerSettings> {
        match ch {
            TrackRef::Track(i) => self.project.tracks.get_mut(i).map(|t| &mut t.mixer),
            TrackRef::Return(i) => self.project.returns.get_mut(i).map(|t| &mut t.mixer),
            TrackRef::Master => Some(&mut self.project.master.mixer),
        }
    }

    pub fn track_ref_name(&self, ch: TrackRef) -> String {
        match ch {
            TrackRef::Track(t) => self.project.tracks.get(t).map(|t| t.name.clone()).unwrap_or_default(),
            TrackRef::Return(r) => self.project.returns.get(r).map(|t| t.name.clone()).unwrap_or_default(),
            TrackRef::Master => "Master".into(),
        }
    }

    /// Tap tempo: average of the last few taps.
    pub fn tap_tempo(&mut self) {
        let now = self.time_started.elapsed().as_secs_f64();
        if self.tap_times.last().is_some_and(|&t| now - t > 2.0) {
            self.tap_times.clear();
        }
        self.tap_times.push(now);
        if self.tap_times.len() > 8 {
            self.tap_times.remove(0);
        }
        if self.tap_times.len() >= 2 {
            let d: Vec<f64> = self.tap_times.windows(2).map(|w| w[1] - w[0]).collect();
            let avg = d.iter().sum::<f64>() / d.len() as f64;
            let bpm = (60.0 / avg).clamp(20.0, 999.0);
            self.project.tempo = (bpm * 100.0).round() / 100.0;
            self.send(Command::SetTempo(self.project.tempo));
            self.status = format!("Tap tempo: {:.1} bpm", self.project.tempo);
        }
    }

    pub const PALETTE: [[u8; 3]; 8] = [
        [0xf0, 0x80, 0x50],
        [0x90, 0xe0, 0x70],
        [0x70, 0xa0, 0xf0],
        [0xe0, 0x60, 0xc0],
        [0xf0, 0xd0, 0x50],
        [0x60, 0xd0, 0xd0],
        [0xb0, 0x90, 0xf0],
        [0xf0, 0x70, 0x70],
    ];
}
