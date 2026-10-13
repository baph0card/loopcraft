//! The project model: everything that is saved in a `.loopcraft` file.
//!
//! This is plain data. The engine builds its real-time structures from it on
//! a non-audio thread, so nothing here needs to be lock-free.

use crate::devices::*;
use crate::time::{Beats, Quantization, TimeSignature};
use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub format_version: u32,
    pub name: String,
    pub tempo: f64,
    pub time_signature: TimeSignature,
    pub quantization: Quantization,
    pub tracks: Vec<Track>,
    /// Return tracks. Every regular track has one send per return.
    pub returns: Vec<Track>,
    pub master: Track,
    pub scenes: Vec<Scene>,
    /// Arrangement loop brace (start, end) in beats.
    pub loop_region: Option<(Beats, Beats)>,
    pub metronome: bool,
    /// Crossfader position, -1 (A) .. 1 (B).
    pub crossfader: f32,
    /// Cue (pre-listen) output level in dB.
    pub cue_volume_db: f32,
    /// Bars of metronome count-in before recording starts (0 = off).
    pub count_in_bars: u32,
    /// Groove pool: named swing/timing templates clips can reference.
    pub grooves: Vec<Groove>,
    /// MIDI CC / note and computer-key mappings to parameters and actions.
    pub mappings: Vec<Mapping>,
    /// Arrangement tempo automation (beat, bpm). Empty = constant tempo.
    pub tempo_automation: Vec<(Beats, f64)>,
    /// Arrangement locators (beat, name).
    pub locators: Vec<(Beats, String)>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            name: "Untitled".into(),
            tempo: 120.0,
            time_signature: TimeSignature::default(),
            quantization: Quantization::OneBar,
            tracks: Vec::new(),
            returns: Vec::new(),
            master: Track::new("Master", TrackKind::Master),
            scenes: Vec::new(),
            loop_region: None,
            metronome: false,
            crossfader: 0.0,
            cue_volume_db: 0.0,
            count_in_bars: 0,
            grooves: Vec::new(),
            mappings: Vec::new(),
            tempo_automation: Vec::new(),
            locators: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum ProjectError {
    Json(serde_json::Error),
    Version { found: u32, supported: u32 },
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectError::Json(e) => write!(f, "invalid project file: {e}"),
            ProjectError::Version { found, supported } => write!(f, "project format version {found} is newer than this build supports ({supported})"),
        }
    }
}

impl std::error::Error for ProjectError {}

impl Project {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("project serializes")
    }

    pub fn from_json(s: &str) -> Result<Self, ProjectError> {
        let p: Project = serde_json::from_str(s).map_err(ProjectError::Json)?;
        if p.format_version > FORMAT_VERSION {
            return Err(ProjectError::Version { found: p.format_version, supported: FORMAT_VERSION });
        }
        Ok(p.normalized())
    }

    /// Repair invariants that a hand-edited file might break: every track
    /// gets exactly one send per return, and every track has one slot per scene.
    pub fn normalized(mut self) -> Self {
        let n_returns = self.returns.len();
        let n_scenes = self.scenes.len().max(self.tracks.iter().map(|t| t.slots.len()).max().unwrap_or(0));
        while self.scenes.len() < n_scenes {
            let i = self.scenes.len() + 1;
            self.scenes.push(Scene { name: format!("{i}"), ..Default::default() });
        }
        let n_tracks = self.tracks.len();
        for i in 0..n_tracks {
            // A track may only belong to an earlier group track (no cycles).
            let ok = self.tracks[i].group.is_some_and(|g| g < i && self.tracks[g].kind == TrackKind::Group);
            if !ok {
                self.tracks[i].group = None;
            }
        }
        for t in self.tracks.iter_mut() {
            t.sends.resize(n_returns, -70.0);
            t.slots.resize(n_scenes, None);
        }
        for r in self.returns.iter_mut() {
            r.sends.resize(n_returns, -70.0);
        }
        self
    }

    /// Indices of the tracks whose output feeds group track `g`, directly.
    pub fn group_children(&self, g: usize) -> impl Iterator<Item = usize> + '_ {
        self.tracks.iter().enumerate().filter(move |(_, t)| t.group == Some(g)).map(|(i, _)| i)
    }

    /// True when `track` sits (directly or indirectly) inside group `g`.
    pub fn is_inside(&self, track: usize, g: usize) -> bool {
        let mut cur = self.tracks.get(track).and_then(|t| t.group);
        while let Some(p) = cur {
            if p == g {
                return true;
            }
            cur = self.tracks.get(p).and_then(|t| t.group);
        }
        false
    }

    /// Nesting depth (0 = top level).
    pub fn depth(&self, track: usize) -> usize {
        let mut d = 0;
        let mut cur = self.tracks.get(track).and_then(|t| t.group);
        while let Some(p) = cur {
            d += 1;
            cur = self.tracks.get(p).and_then(|t| t.group);
        }
        d
    }

    /// Tempo at `beat`, following tempo automation when present.
    pub fn tempo_at(&self, beat: Beats) -> f64 {
        tempo_at(&self.tempo_automation, self.tempo, beat)
    }

    pub fn add_track(&mut self, track: Track) -> usize {
        self.tracks.push(track);
        self.normalized_in_place();
        self.tracks.len() - 1
    }

    pub fn add_return(&mut self, track: Track) -> usize {
        self.returns.push(track);
        self.normalized_in_place();
        self.returns.len() - 1
    }

    pub fn add_scene(&mut self, name: impl Into<String>) -> usize {
        self.scenes.push(Scene { name: name.into(), ..Default::default() });
        self.normalized_in_place();
        self.scenes.len() - 1
    }

    fn normalized_in_place(&mut self) {
        let p = std::mem::take(self);
        *self = p.normalized();
    }

    /// End of the last arrangement clip, in beats.
    pub fn arrangement_length(&self) -> Beats {
        self.tracks.iter().flat_map(|t| t.arrangement.iter()).map(|c| c.end()).fold(0.0, f64::max)
    }
}

/// Step-wise tempo automation lookup (each point holds until the next).
pub fn tempo_at(points: &[(Beats, f64)], default: f64, beat: Beats) -> f64 {
    let mut t = points.first().map(|p| p.1).unwrap_or(default);
    for &(b, bpm) in points {
        if b <= beat + 1e-9 {
            t = bpm;
        } else {
            break;
        }
    }
    t
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Midi,
    Audio,
    /// Sums the tracks that name it as their `group`.
    Group,
    Return,
    Master,
}

/// Where an audio track's live input comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputSource {
    None,
    /// Channel pair `n` (0 = inputs 1/2) of the audio interface.
    Stereo(u8),
    /// A single input channel, duplicated to both sides.
    Mono(u8),
    /// Another track's post-fader output (resampling).
    Track(usize),
    /// The master output (resampling).
    Master,
}

impl Default for InputSource {
    fn default() -> Self {
        InputSource::Stereo(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Monitor {
    /// Hear the input when the track is armed and not playing a clip.
    #[default]
    Auto,
    In,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CrossfadeAssign {
    #[default]
    None,
    A,
    B,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    pub name: String,
    pub kind: TrackKind,
    pub color: [u8; 3],
    /// Instrument for MIDI tracks. Ignored on audio tracks.
    pub instrument: Option<DeviceSpec>,
    /// Audio effects, processed in order after the instrument / clip.
    pub effects: Vec<DeviceSpec>,
    pub mixer: MixerSettings,
    /// Send levels in dB, one per return track.
    pub sends: Vec<f32>,
    /// Session-view clip slots, one per scene.
    pub slots: Vec<Option<Clip>>,
    /// Arrangement-view clips on the timeline.
    pub arrangement: Vec<ArrangementClip>,
    pub automation: Vec<AutomationLane>,
    /// MIDI effects ahead of the instrument (MIDI tracks only).
    pub midi_effects: Vec<DeviceSpec>,
    /// Index of the group track this track feeds, if any.
    pub group: Option<usize>,
    /// UI: group folded closed.
    pub folded: bool,
    pub input: InputSource,
    pub monitor: Monitor,
    pub armed: bool,
    pub crossfade: CrossfadeAssign,
    /// Effects (by index in `effects`) that are switched off.
    pub bypassed: Vec<usize>,
    /// Instrument switched off.
    pub instrument_bypassed: bool,
}

impl Default for Track {
    fn default() -> Self {
        Self::new("Track", TrackKind::Midi)
    }
}

impl Track {
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self {
            name: name.into(),
            kind,
            color: [0x6c, 0x9e, 0xd8],
            instrument: None,
            effects: Vec::new(),
            mixer: MixerSettings::default(),
            sends: Vec::new(),
            slots: Vec::new(),
            arrangement: Vec::new(),
            automation: Vec::new(),
            midi_effects: Vec::new(),
            group: None,
            folded: false,
            input: InputSource::default(),
            monitor: Monitor::default(),
            armed: false,
            crossfade: CrossfadeAssign::None,
            bypassed: Vec::new(),
            instrument_bypassed: false,
        }
    }

    pub fn group(name: impl Into<String>) -> Self {
        Self::new(name, TrackKind::Group)
    }

    pub fn with_midi_effect(mut self, fx: DeviceSpec) -> Self {
        self.midi_effects.push(fx);
        self
    }

    pub fn in_group(mut self, g: usize) -> Self {
        self.group = Some(g);
        self
    }

    pub fn is_effect_bypassed(&self, i: usize) -> bool {
        self.bypassed.contains(&i)
    }

    pub fn midi(name: impl Into<String>, instrument: DeviceSpec) -> Self {
        Self { instrument: Some(instrument), ..Self::new(name, TrackKind::Midi) }
    }

    pub fn audio(name: impl Into<String>) -> Self {
        Self::new(name, TrackKind::Audio)
    }

    pub fn with_color(mut self, rgb: [u8; 3]) -> Self {
        self.color = rgb;
        self
    }

    pub fn with_effect(mut self, fx: DeviceSpec) -> Self {
        self.effects.push(fx);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixerSettings {
    pub volume_db: f32,
    /// -1.0 (left) .. 1.0 (right)
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
}

impl Default for MixerSettings {
    fn default() -> Self {
        Self { volume_db: 0.0, pan: 0.0, mute: false, solo: false }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Scene {
    pub name: String,
    /// Launching the scene changes the song tempo when set.
    pub tempo: Option<f64>,
    pub time_signature: Option<TimeSignature>,
    /// Launch another scene automatically after a while.
    pub follow: Option<FollowAction>,
    pub color: Option<[u8; 3]>,
}

/// What happens when a follow action fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FollowKind {
    /// Keep playing; nothing happens.
    #[default]
    NoAction,
    Stop,
    /// Restart this clip (or scene).
    Again,
    Previous,
    Next,
    First,
    Last,
    /// Any clip in the group, including this one.
    Any,
    /// Any clip in the group except this one.
    Other,
    /// A specific slot (clips) or scene index (scenes).
    Jump(usize),
}

/// Two candidate actions chosen at random by `chance_a` after `after` beats.
/// For clips, "the group" is the run of filled slots around the clip,
/// bounded by empty slots.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FollowAction {
    pub after: Beats,
    pub action_a: FollowKind,
    pub action_b: FollowKind,
    /// Probability of `action_a` (0..1); `action_b` gets the rest.
    pub chance_a: f32,
    /// Fire after the clip's own length (and every multiple of it) instead of `after`.
    pub linked: bool,
    pub enabled: bool,
}

impl Default for FollowAction {
    fn default() -> Self {
        Self { after: 4.0, action_a: FollowKind::Next, action_b: FollowKind::NoAction, chance_a: 1.0, linked: false, enabled: true }
    }
}

impl FollowAction {
    pub fn next_after(beats: Beats) -> Self {
        Self { after: beats, ..Default::default() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LaunchMode {
    /// Starts on press; press again restarts.
    #[default]
    Trigger,
    /// Plays only while held (needs a release command).
    Gate,
    /// Press to start, press again to stop.
    Toggle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Clip {
    pub name: String,
    pub color: [u8; 3],
    /// Loop length (or one-shot length) in beats.
    pub length: Beats,
    pub looping: bool,
    pub launch_mode: LaunchMode,
    /// Per-clip launch quantization; `None` follows the global setting.
    pub quantization: Option<Quantization>,
    pub content: ClipContent,
    pub follow: Option<FollowAction>,
    /// Where the loop starts inside the clip (beats). Playback begins at 0
    /// and, when looping, wraps from `length` back to `loop_start`.
    pub loop_start: Beats,
    /// Index into the project's groove pool.
    pub groove: Option<usize>,
    /// Launch velocity sensitivity (0 = ignore launch velocity).
    pub velocity_amount: f32,
}

impl Default for Clip {
    fn default() -> Self {
        Self {
            name: "Clip".into(),
            color: [0xe0, 0xa0, 0x40],
            length: 4.0,
            looping: true,
            launch_mode: LaunchMode::Trigger,
            quantization: None,
            content: ClipContent::Midi { notes: Vec::new() },
            follow: None,
            loop_start: 0.0,
            groove: None,
            velocity_amount: 0.0,
        }
    }
}

impl Clip {
    pub fn midi(name: impl Into<String>, length: Beats, notes: Vec<Note>) -> Self {
        Self { name: name.into(), length, content: ClipContent::Midi { notes }, ..Default::default() }
    }

    pub fn audio(name: impl Into<String>, length: Beats, audio: AudioClip) -> Self {
        Self { name: name.into(), length, content: ClipContent::Audio(audio), ..Default::default() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClipContent {
    Midi { notes: Vec<Note> },
    Audio(AudioClip),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Note {
    pub pitch: u8,
    /// Start relative to the clip, in beats.
    pub start: Beats,
    pub length: Beats,
    pub velocity: u8,
    /// Chance (0..1) that the note plays on each pass.
    pub probability: f32,
}

impl Default for Note {
    fn default() -> Self {
        Self { pitch: 60, start: 0.0, length: 0.25, velocity: 100, probability: 1.0 }
    }
}

impl Note {
    pub fn new(pitch: u8, start: Beats, length: Beats, velocity: u8) -> Self {
        Self { pitch, start, length, velocity, probability: 1.0 }
    }
}

/// How an audio clip follows the song tempo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WarpMode {
    /// Plays at the file's own speed; not tempo-synced.
    Off,
    /// Speed and pitch follow tempo together, like a turntable.
    Repitch,
    /// WSOLA time-stretch: tempo changes, pitch stays. Good all-rounder.
    #[default]
    Stretch,
    /// Slices at transients and plays each slice at its own speed: tight
    /// drums and percussive loops.
    Beats,
    /// Phase-vocoder stretch for full mixes and tonal material.
    Complex,
}

/// Pins a beat position in the clip to a position (seconds) in the file.
/// Between markers the file is stretched linearly.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WarpMarker {
    pub beat: Beats,
    pub seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioClip {
    /// Path to the audio file, relative to the project file or absolute.
    pub file: String,
    pub warp: WarpMode,
    /// Tempo the material was recorded at. Warping maps it onto the song tempo.
    pub original_bpm: f64,
    /// Where in the file (in beats at `original_bpm`) the clip starts.
    pub start_offset: Beats,
    pub gain_db: f32,
    /// Pitch shift in semitones (Stretch / Beats / Complex modes; Repitch
    /// folds it into the playback rate).
    pub transpose: f32,
    /// When non-empty these define the beat-to-file mapping and override
    /// `original_bpm` / `start_offset`.
    pub warp_markers: Vec<WarpMarker>,
    pub reverse: bool,
    /// Fade in / out lengths in beats.
    pub fade_in: Beats,
    pub fade_out: Beats,
}

impl Default for AudioClip {
    fn default() -> Self {
        Self {
            file: String::new(),
            warp: WarpMode::Stretch,
            original_bpm: 120.0,
            start_offset: 0.0,
            gain_db: 0.0,
            transpose: 0.0,
            warp_markers: Vec::new(),
            reverse: false,
            fade_in: 0.0,
            fade_out: 0.0,
        }
    }
}

impl AudioClip {
    /// Map a clip-local beat to a file position in seconds.
    pub fn seconds_at(&self, beat: Beats) -> f64 {
        let m = &self.warp_markers;
        if m.len() >= 2 {
            let i = m.windows(2).position(|w| beat < w[1].beat).unwrap_or(m.len() - 2);
            let (a, b) = (m[i], m[i + 1]);
            let span = (b.beat - a.beat).max(1e-9);
            return a.seconds + (beat - a.beat) / span * (b.seconds - a.seconds);
        }
        if m.len() == 1 {
            return m[0].seconds + (beat - m[0].beat) * 60.0 / self.original_bpm.max(1.0);
        }
        (self.start_offset + beat) * 60.0 / self.original_bpm.max(1.0)
    }

    /// Local playback rate (file seconds per beat) at a beat.
    pub fn seconds_per_beat_at(&self, beat: Beats) -> f64 {
        let m = &self.warp_markers;
        if m.len() >= 2 {
            let i = m.windows(2).position(|w| beat < w[1].beat).unwrap_or(m.len() - 2);
            let (a, b) = (m[i], m[i + 1]);
            return (b.seconds - a.seconds) / (b.beat - a.beat).max(1e-9);
        }
        60.0 / self.original_bpm.max(1.0)
    }
}

/// A groove template: per-sixteenth timing (fraction of a sixteenth) and
/// velocity offsets, repeating every `steps.len()` sixteenths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Groove {
    pub name: String,
    pub timing: Vec<f32>,
    pub velocity: Vec<f32>,
    /// Global amount 0..1.3.
    pub amount: f32,
}

impl Default for Groove {
    fn default() -> Self {
        Self::swing("Swing 16ths 58%", 0.58)
    }
}

impl Groove {
    /// Classic MPC-style swing: every other 16th is late. `ratio` 0.5 = straight.
    pub fn swing(name: &str, ratio: f32) -> Self {
        let late = (ratio - 0.5) * 2.0;
        Self { name: name.into(), timing: vec![0.0, late], velocity: vec![0.0, -0.1], amount: 1.0 }
    }

    /// Offset (beats) and velocity multiplier for a note starting at `beat`.
    pub fn apply(&self, beat: Beats) -> (Beats, f32) {
        if self.timing.is_empty() {
            return (0.0, 1.0);
        }
        let step = (beat * 4.0).round();
        if (beat * 4.0 - step).abs() > 0.2 {
            return (0.0, 1.0);
        }
        let i = (step as i64).rem_euclid(self.timing.len() as i64) as usize;
        let t = self.timing[i] * self.amount * 0.25;
        let v = 1.0 + self.velocity.get(i).copied().unwrap_or(0.0) * self.amount;
        (t as f64, v.max(0.0))
    }
}

/// What a controller mapping drives.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MappingTarget {
    Volume(usize),
    Pan(usize),
    Send { track: usize, ret: usize },
    DeviceParam { track: usize, device: usize, param: usize },
    InstrumentParam { track: usize, param: usize },
    Tempo,
    Crossfader,
    LaunchClip { track: usize, slot: usize },
    LaunchScene(usize),
    StopTrack(usize),
    Play,
    Stop,
    Mute(usize),
    Solo(usize),
    Arm(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MappingSource {
    /// MIDI CC (channel 0..15, controller 0..127).
    Cc { channel: u8, cc: u8 },
    /// MIDI note (buttons / pads).
    Note { channel: u8, note: u8 },
    /// Computer key, by name (egui key name).
    Key(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    pub source: MappingSource,
    pub target: MappingTarget,
    /// Output range for continuous targets, in the target's natural units.
    pub min: f32,
    pub max: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrangementClip {
    pub start: Beats,
    /// Region length on the timeline. When longer than the clip and the clip
    /// loops, the content repeats; `None` means exactly one clip length.
    #[serde(default)]
    pub duration: Option<Beats>,
    pub clip: Clip,
}

impl ArrangementClip {
    pub fn new(start: Beats, clip: Clip) -> Self {
        Self { start, duration: None, clip }
    }

    pub fn looped(start: Beats, duration: Beats, clip: Clip) -> Self {
        Self { start, duration: Some(duration), clip }
    }

    pub fn duration(&self) -> Beats {
        self.duration.unwrap_or(self.clip.length)
    }

    pub fn end(&self) -> Beats {
        self.start + self.duration()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutomationTarget {
    Volume,
    Pan,
    Send(usize),
    Instrument { param: usize },
    Effect { index: usize, param: usize },
}

/// A breakpoint envelope in arrangement time. Values are normalized 0..1
/// except `Volume` (dB) and `Pan` (-1..1), which use their natural units.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationLane {
    pub target: AutomationTarget,
    pub points: Vec<(Beats, f32)>,
}

impl AutomationLane {
    /// Linear interpolation between breakpoints; holds the end values.
    pub fn value_at(&self, beat: Beats) -> Option<f32> {
        let pts = &self.points;
        let first = pts.first()?;
        if beat <= first.0 {
            return Some(first.1);
        }
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if beat <= b.0 {
                let span = (b.0 - a.0).max(1e-12);
                let t = ((beat - a.0) / span) as f32;
                return Some(a.1 + (b.1 - a.1) * t);
            }
        }
        pts.last().map(|p| p.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_normalize() {
        let mut p = Project::default();
        p.add_return(Track::new("A Reverb", TrackKind::Return).with_effect(DeviceSpec::Reverb(Default::default())));
        p.add_scene("Intro");
        p.add_scene("Drop");
        let t = p.add_track(Track::midi("Bass", DeviceSpec::PolySynth(Default::default())));
        p.tracks[t].slots[1] = Some(Clip::midi("riff", 4.0, vec![Note::new(36, 0.0, 1.0, 110)]));

        assert_eq!(p.tracks[t].sends.len(), 1);
        assert_eq!(p.tracks[t].slots.len(), 2);

        let json = p.to_json();
        let back = Project::from_json(&json).unwrap();
        assert_eq!(back.tracks.len(), 1);
        assert!(back.tracks[0].slots[1].is_some());
        assert_eq!(back.scenes[1].name, "Drop");
    }

    #[test]
    fn rejects_future_versions() {
        let json = r#"{"format_version": 999}"#;
        assert!(matches!(Project::from_json(json), Err(ProjectError::Version { .. })));
    }

    #[test]
    fn automation_interpolates() {
        let lane = AutomationLane { target: AutomationTarget::Volume, points: vec![(0.0, -12.0), (4.0, 0.0)] };
        assert_eq!(lane.value_at(-1.0), Some(-12.0));
        assert_eq!(lane.value_at(2.0), Some(-6.0));
        assert_eq!(lane.value_at(10.0), Some(0.0));
    }
}
