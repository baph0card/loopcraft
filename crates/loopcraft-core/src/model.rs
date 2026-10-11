//! The project model: everything that is saved in a `.loopcraft` file.
//!
//! This is plain data. The engine builds its real-time structures from it on
//! a non-audio thread, so nothing here needs to be lock-free.

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
            ProjectError::Version { found, supported } => write!(
                f,
                "project format version {found} is newer than this build supports ({supported})"
            ),
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
        for t in self.tracks.iter_mut() {
            t.sends.resize(n_returns, -70.0);
            t.slots.resize(n_scenes, None);
        }
        self
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
        self.tracks
            .iter()
            .flat_map(|t| t.arrangement.iter())
            .map(|c| c.end())
            .fold(0.0, f64::max)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Midi,
    Audio,
    Return,
    Master,
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
        }
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
    /// Granular time-stretch: tempo changes, pitch stays.
    #[default]
    Stretch,
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
}

impl Default for AudioClip {
    fn default() -> Self {
        Self { file: String::new(), warp: WarpMode::Stretch, original_bpm: 120.0, start_offset: 0.0, gain_db: 0.0 }
    }
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

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeviceSpec {
    // Instruments
    PolySynth(SynthParams),
    Sampler(SamplerParams),
    DrumRack(DrumRackParams),
    // Audio effects
    Eq(EqParams),
    Compressor(CompressorParams),
    Delay(DelayParams),
    Reverb(ReverbParams),
    Saturator(SaturatorParams),
    Utility(UtilityParams),
    AutoFilter(AutoFilterParams),
    // Third-party
    Plugin(PluginRef),
}

impl DeviceSpec {
    pub fn name(&self) -> &str {
        match self {
            DeviceSpec::PolySynth(_) => "Poly Synth",
            DeviceSpec::Sampler(_) => "Sampler",
            DeviceSpec::DrumRack(_) => "Drum Rack",
            DeviceSpec::Eq(_) => "EQ",
            DeviceSpec::Compressor(_) => "Compressor",
            DeviceSpec::Delay(_) => "Delay",
            DeviceSpec::Reverb(_) => "Reverb",
            DeviceSpec::Saturator(_) => "Saturator",
            DeviceSpec::Utility(_) => "Utility",
            DeviceSpec::AutoFilter(_) => "Auto Filter",
            DeviceSpec::Plugin(p) => &p.name,
        }
    }

    pub fn is_instrument(&self) -> bool {
        matches!(self, DeviceSpec::PolySynth(_) | DeviceSpec::Sampler(_) | DeviceSpec::DrumRack(_))
            || matches!(self, DeviceSpec::Plugin(p) if p.is_instrument)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Waveform {
    Sine,
    Triangle,
    #[default]
    Saw,
    Square,
    Noise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FilterMode {
    #[default]
    LowPass,
    HighPass,
    BandPass,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Adsr {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
}

impl Default for Adsr {
    fn default() -> Self {
        Self { attack: 0.005, decay: 0.2, sustain: 0.7, release: 0.25 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SynthParams {
    pub osc1: Waveform,
    pub osc2: Waveform,
    /// Osc 2 detune in semitones.
    pub osc2_semitones: f32,
    pub osc2_detune_cents: f32,
    /// 0 = only osc1, 1 = only osc2.
    pub osc_mix: f32,
    pub sub_level: f32,
    pub filter_mode: FilterMode,
    pub cutoff_hz: f32,
    pub resonance: f32,
    /// Filter envelope depth in octaves.
    pub filter_env_amount: f32,
    pub amp_env: Adsr,
    pub filter_env: Adsr,
    pub voices: usize,
    pub glide: f32,
    pub gain_db: f32,
}

impl Default for SynthParams {
    fn default() -> Self {
        Self {
            osc1: Waveform::Saw,
            osc2: Waveform::Square,
            osc2_semitones: 0.0,
            osc2_detune_cents: 7.0,
            osc_mix: 0.4,
            sub_level: 0.0,
            filter_mode: FilterMode::LowPass,
            cutoff_hz: 2_000.0,
            resonance: 0.2,
            filter_env_amount: 2.0,
            amp_env: Adsr::default(),
            filter_env: Adsr { attack: 0.002, decay: 0.3, sustain: 0.2, release: 0.3 },
            voices: 16,
            glide: 0.0,
            gain_db: -6.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SamplerParams {
    pub file: String,
    pub root_key: u8,
    pub amp_env: Adsr,
    pub gain_db: f32,
    pub loop_sample: bool,
}

impl Default for SamplerParams {
    fn default() -> Self {
        Self {
            file: String::new(),
            root_key: 60,
            amp_env: Adsr { attack: 0.001, decay: 0.1, sustain: 1.0, release: 0.1 },
            gain_db: 0.0,
            loop_sample: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DrumVoice {
    Kick,
    Snare,
    ClosedHat,
    OpenHat,
    Clap,
    Tom,
    Rim,
    Cowbell,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PadSource {
    /// Built-in synthesized drum voice; works with no sample files.
    Synth(DrumVoice),
    Sample(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DrumPad {
    pub note: u8,
    pub source: PadSource,
    pub gain_db: f32,
    pub pan: f32,
    /// Pads in the same choke group cut each other off (e.g. open/closed hat).
    pub choke_group: Option<u8>,
    pub tune_semitones: f32,
}

impl Default for DrumPad {
    fn default() -> Self {
        Self { note: 36, source: PadSource::Synth(DrumVoice::Kick), gain_db: 0.0, pan: 0.0, choke_group: None, tune_semitones: 0.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DrumRackParams {
    pub pads: Vec<DrumPad>,
}

impl Default for DrumRackParams {
    /// A General-MIDI-style default kit.
    fn default() -> Self {
        use DrumVoice::*;
        let pad = |note, v, choke| DrumPad { note, source: PadSource::Synth(v), choke_group: choke, ..Default::default() };
        Self {
            pads: vec![
                pad(36, Kick, None),
                pad(37, Rim, None),
                pad(38, Snare, None),
                pad(39, Clap, None),
                pad(42, ClosedHat, Some(1)),
                pad(45, Tom, None),
                pad(46, OpenHat, Some(1)),
                pad(56, Cowbell, None),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EqBandKind {
    LowCut,
    LowShelf,
    Peak,
    HighShelf,
    HighCut,
    Notch,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub kind: EqBandKind,
    pub freq_hz: f32,
    pub gain_db: f32,
    pub q: f32,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EqParams {
    /// Up to eight bands.
    pub bands: Vec<EqBand>,
    pub output_db: f32,
}

impl Default for EqParams {
    fn default() -> Self {
        Self {
            bands: vec![
                EqBand { kind: EqBandKind::LowCut, freq_hz: 30.0, gain_db: 0.0, q: 0.71, enabled: true },
                EqBand { kind: EqBandKind::LowShelf, freq_hz: 120.0, gain_db: 0.0, q: 0.71, enabled: true },
                EqBand { kind: EqBandKind::Peak, freq_hz: 1_000.0, gain_db: 0.0, q: 1.0, enabled: true },
                EqBand { kind: EqBandKind::HighShelf, freq_hz: 8_000.0, gain_db: 0.0, q: 0.71, enabled: true },
            ],
            output_db: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompressorParams {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub makeup_db: f32,
    /// Index of the track whose signal keys the compressor, if any.
    pub sidechain_track: Option<usize>,
    pub dry_wet: f32,
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self { threshold_db: -18.0, ratio: 4.0, attack_ms: 5.0, release_ms: 80.0, knee_db: 6.0, makeup_db: 0.0, sidechain_track: None, dry_wet: 1.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DelayParams {
    /// When true, `time_beats` follows the song tempo; otherwise `time_ms` is used.
    pub sync: bool,
    pub time_beats: f64,
    pub time_ms: f32,
    pub feedback: f32,
    pub ping_pong: bool,
    pub filter_hz: f32,
    pub dry_wet: f32,
}

impl Default for DelayParams {
    fn default() -> Self {
        Self { sync: true, time_beats: 0.75, time_ms: 375.0, feedback: 0.4, ping_pong: true, filter_hz: 6_000.0, dry_wet: 0.3 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReverbParams {
    pub size: f32,
    pub damping: f32,
    pub width: f32,
    pub predelay_ms: f32,
    pub dry_wet: f32,
}

impl Default for ReverbParams {
    fn default() -> Self {
        Self { size: 0.7, damping: 0.5, width: 1.0, predelay_ms: 10.0, dry_wet: 0.25 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SaturatorParams {
    pub drive_db: f32,
    pub output_db: f32,
    pub dry_wet: f32,
}

impl Default for SaturatorParams {
    fn default() -> Self {
        Self { drive_db: 6.0, output_db: -3.0, dry_wet: 1.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UtilityParams {
    pub gain_db: f32,
    /// 0 = mono, 1 = unchanged, 2 = extra wide.
    pub width: f32,
    pub invert_left: bool,
    pub invert_right: bool,
    pub mono_bass_hz: Option<f32>,
}

impl Default for UtilityParams {
    fn default() -> Self {
        Self { gain_db: 0.0, width: 1.0, invert_left: false, invert_right: false, mono_bass_hz: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoFilterParams {
    pub mode: FilterMode,
    pub cutoff_hz: f32,
    pub resonance: f32,
    /// LFO rate in beats per cycle when synced.
    pub lfo_beats: f64,
    /// LFO depth in octaves.
    pub lfo_amount: f32,
}

impl Default for AutoFilterParams {
    fn default() -> Self {
        Self { mode: FilterMode::LowPass, cutoff_hz: 1_200.0, resonance: 0.3, lfo_beats: 2.0, lfo_amount: 1.5 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginFormat {
    Clap,
    Vst3,
    AudioUnit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRef {
    pub format: PluginFormat,
    /// Path to the plugin bundle / shared library.
    pub path: String,
    /// Format-specific plugin id (CLAP id, VST3 class id, AU component).
    pub id: String,
    pub name: String,
    pub is_instrument: bool,
    /// Opaque saved state, base64 or format-specific.
    pub state: Option<String>,
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
