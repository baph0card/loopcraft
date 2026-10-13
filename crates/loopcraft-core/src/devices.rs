//! Device descriptions: instruments, MIDI effects, audio effects, racks and
//! plugin references, as saved in the project file.

use crate::time::Beats;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeviceSpec {
    // Instruments
    PolySynth(SynthParams),
    Wavetable(WavetableParams),
    FmSynth(FmParams),
    Sampler(SamplerParams),
    MultiSampler(MultiSamplerParams),
    DrumRack(DrumRackParams),
    InstrumentRack(RackParams),
    // MIDI effects
    Arpeggiator(ArpParams),
    Chord(ChordParams),
    Scale(ScaleParams),
    RandomNote(RandomNoteParams),
    NoteLength(NoteLengthParams),
    Velocity(VelocityParams),
    Pitch(PitchParams),
    // Audio effects
    Eq(EqParams),
    Compressor(CompressorParams),
    GlueCompressor(GlueParams),
    Limiter(LimiterParams),
    Gate(GateParams),
    MultibandDynamics(MultibandParams),
    Delay(DelayParams),
    Reverb(ReverbParams),
    ConvolutionReverb(ConvolutionParams),
    Saturator(SaturatorParams),
    DrumBuss(DrumBussParams),
    Amp(AmpParams),
    Redux(ReduxParams),
    Chorus(ChorusParams),
    Flanger(FlangerParams),
    Phaser(PhaserParams),
    AutoPan(AutoPanParams),
    Utility(UtilityParams),
    AutoFilter(AutoFilterParams),
    BeatRepeat(BeatRepeatParams),
    GrainDelay(GrainDelayParams),
    Vocoder(VocoderParams),
    AudioRack(RackParams),
    // Third-party
    Plugin(PluginRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Instrument,
    MidiEffect,
    AudioEffect,
}

impl DeviceSpec {
    pub fn name(&self) -> &str {
        use DeviceSpec::*;
        match self {
            PolySynth(_) => "Poly Synth",
            Wavetable(_) => "Wavetable",
            FmSynth(_) => "FM Synth",
            Sampler(_) => "Sampler",
            MultiSampler(_) => "Multi Sampler",
            DrumRack(_) => "Drum Rack",
            InstrumentRack(r) | AudioRack(r) => {
                if r.name.is_empty() {
                    if matches!(self, InstrumentRack(_)) { "Instrument Rack" } else { "Audio Effect Rack" }
                } else {
                    &r.name
                }
            }
            Arpeggiator(_) => "Arpeggiator",
            Chord(_) => "Chord",
            Scale(_) => "Scale",
            RandomNote(_) => "Random",
            NoteLength(_) => "Note Length",
            Velocity(_) => "Velocity",
            Pitch(_) => "Pitch",
            Eq(_) => "EQ",
            Compressor(_) => "Compressor",
            GlueCompressor(_) => "Glue Compressor",
            Limiter(_) => "Limiter",
            Gate(_) => "Gate",
            MultibandDynamics(_) => "Multiband Dynamics",
            Delay(_) => "Delay",
            Reverb(_) => "Reverb",
            ConvolutionReverb(_) => "Convolution Reverb",
            Saturator(_) => "Saturator",
            DrumBuss(_) => "Drum Buss",
            Amp(_) => "Amp",
            Redux(_) => "Redux",
            Chorus(_) => "Chorus",
            Flanger(_) => "Flanger",
            Phaser(_) => "Phaser",
            AutoPan(_) => "Auto Pan",
            Utility(_) => "Utility",
            AutoFilter(_) => "Auto Filter",
            BeatRepeat(_) => "Beat Repeat",
            GrainDelay(_) => "Grain Delay",
            Vocoder(_) => "Vocoder",
            Plugin(p) => &p.name,
        }
    }

    pub fn kind(&self) -> DeviceKind {
        use DeviceSpec::*;
        match self {
            PolySynth(_) | Wavetable(_) | FmSynth(_) | Sampler(_) | MultiSampler(_) | DrumRack(_) | InstrumentRack(_) => DeviceKind::Instrument,
            Arpeggiator(_) | Chord(_) | Scale(_) | RandomNote(_) | NoteLength(_) | Velocity(_) | Pitch(_) => DeviceKind::MidiEffect,
            Plugin(p) if p.is_instrument => DeviceKind::Instrument,
            _ => DeviceKind::AudioEffect,
        }
    }

    pub fn is_instrument(&self) -> bool {
        self.kind() == DeviceKind::Instrument
    }

    pub fn is_midi_effect(&self) -> bool {
        self.kind() == DeviceKind::MidiEffect
    }

    pub fn is_audio_effect(&self) -> bool {
        self.kind() == DeviceKind::AudioEffect
    }

    /// Every built-in device with default settings, for browsers and tests.
    pub fn catalog() -> Vec<DeviceSpec> {
        use DeviceSpec::*;
        vec![
            PolySynth(Default::default()),
            Wavetable(Default::default()),
            FmSynth(Default::default()),
            Sampler(Default::default()),
            MultiSampler(Default::default()),
            DrumRack(Default::default()),
            InstrumentRack(RackParams::instrument_default()),
            Arpeggiator(Default::default()),
            Chord(Default::default()),
            Scale(Default::default()),
            RandomNote(Default::default()),
            NoteLength(Default::default()),
            Velocity(Default::default()),
            Pitch(Default::default()),
            Eq(Default::default()),
            Compressor(Default::default()),
            GlueCompressor(Default::default()),
            Limiter(Default::default()),
            Gate(Default::default()),
            MultibandDynamics(Default::default()),
            Delay(Default::default()),
            Reverb(Default::default()),
            ConvolutionReverb(Default::default()),
            Saturator(Default::default()),
            DrumBuss(Default::default()),
            Amp(Default::default()),
            Redux(Default::default()),
            Chorus(Default::default()),
            Flanger(Default::default()),
            Phaser(Default::default()),
            AutoPan(Default::default()),
            Utility(Default::default()),
            AutoFilter(Default::default()),
            BeatRepeat(Default::default()),
            GrainDelay(Default::default()),
            Vocoder(Default::default()),
            AudioRack(RackParams::default()),
        ]
    }
}

// ---------------------------------------------------------------------------
// Racks
// ---------------------------------------------------------------------------

/// A parallel set of device chains with eight macro knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RackParams {
    pub name: String,
    pub chains: Vec<RackChain>,
    pub macros: Vec<Macro>,
}

impl Default for RackParams {
    fn default() -> Self {
        Self {
            name: String::new(),
            chains: vec![RackChain::default()],
            macros: (1..=8).map(|i| Macro { name: format!("Macro {i}"), ..Default::default() }).collect(),
        }
    }
}

impl RackParams {
    pub fn instrument_default() -> Self {
        let mut r = Self::default();
        r.chains[0].devices = vec![DeviceSpec::PolySynth(Default::default())];
        r
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RackChain {
    pub name: String,
    /// For instrument racks the first device is the instrument; the rest are audio effects.
    pub devices: Vec<DeviceSpec>,
    pub volume_db: f32,
    pub pan: f32,
    pub mute: bool,
    /// Instrument racks: only notes inside these ranges reach the chain.
    pub key_range: (u8, u8),
    pub velocity_range: (u8, u8),
}

impl Default for RackChain {
    fn default() -> Self {
        Self { name: "Chain".into(), devices: Vec::new(), volume_db: 0.0, pan: 0.0, mute: false, key_range: (0, 127), velocity_range: (1, 127) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Macro {
    pub name: String,
    /// 0..1
    pub value: f32,
    pub mappings: Vec<MacroMapping>,
}

/// Macro 0..1 sweeps `param` of device `device` in chain `chain` from `min` to `max`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MacroMapping {
    pub chain: usize,
    pub device: usize,
    pub param: usize,
    pub min: f32,
    pub max: f32,
}

// ---------------------------------------------------------------------------
// Instruments
// ---------------------------------------------------------------------------

/// Built-in wavetables (generated, so no sample data ships with the app).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WavetableKind {
    /// Sine -> triangle -> saw -> square.
    #[default]
    Basic,
    /// Additive: adds harmonics as position increases.
    Harmonics,
    /// Pulse width sweep.
    Pulse,
    /// Vowel-like formant sweep.
    Formant,
    /// Hard-sync style sweep.
    Sync,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WavetableParams {
    pub table1: WavetableKind,
    pub table2: WavetableKind,
    /// 0..1 position within each table.
    pub position1: f32,
    pub position2: f32,
    pub osc2_semitones: f32,
    pub osc_mix: f32,
    pub unison: usize,
    pub unison_detune: f32,
    pub filter_mode: FilterMode,
    pub cutoff_hz: f32,
    pub resonance: f32,
    pub filter_env_amount: f32,
    pub amp_env: Adsr,
    pub mod_env: Adsr,
    /// Modulation envelope depth onto wavetable position.
    pub env_to_position: f32,
    pub lfo_rate_hz: f32,
    pub lfo_to_position: f32,
    pub lfo_to_cutoff: f32,
    pub voices: usize,
    pub gain_db: f32,
}

impl Default for WavetableParams {
    fn default() -> Self {
        Self {
            table1: WavetableKind::Basic,
            table2: WavetableKind::Harmonics,
            position1: 0.6,
            position2: 0.3,
            osc2_semitones: 0.0,
            osc_mix: 0.3,
            unison: 3,
            unison_detune: 0.15,
            filter_mode: FilterMode::LowPass,
            cutoff_hz: 4_000.0,
            resonance: 0.15,
            filter_env_amount: 1.0,
            amp_env: Adsr { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.35 },
            mod_env: Adsr { attack: 0.2, decay: 0.8, sustain: 0.3, release: 0.5 },
            env_to_position: 0.2,
            lfo_rate_hz: 0.3,
            lfo_to_position: 0.1,
            lfo_to_cutoff: 0.0,
            voices: 12,
            gain_db: -10.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FmOperator {
    /// Frequency ratio to the note (fixed_hz overrides when set).
    pub ratio: f32,
    pub fixed_hz: Option<f32>,
    /// Output level 0..1 (modulation index for modulators).
    pub level: f32,
    pub env: Adsr,
    pub detune_cents: f32,
}

impl Default for FmOperator {
    fn default() -> Self {
        Self { ratio: 1.0, fixed_hz: None, level: 0.5, env: Adsr { attack: 0.002, decay: 0.4, sustain: 0.4, release: 0.3 }, detune_cents: 0.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FmParams {
    /// Four operators, A..D. D is usually a carrier.
    pub ops: [FmOperator; 4],
    /// 0..10, see `FM_ALGORITHMS` in the DSP crate.
    pub algorithm: usize,
    /// Self-feedback on operator A.
    pub feedback: f32,
    pub filter_cutoff_hz: f32,
    pub voices: usize,
    pub gain_db: f32,
}

impl Default for FmParams {
    fn default() -> Self {
        let carrier = FmOperator { level: 0.8, env: Adsr { attack: 0.002, decay: 1.0, sustain: 0.5, release: 0.4 }, ..Default::default() };
        let modulator = FmOperator { ratio: 2.0, level: 0.35, env: Adsr { attack: 0.002, decay: 0.5, sustain: 0.2, release: 0.3 }, ..Default::default() };
        Self {
            ops: [modulator, FmOperator { level: 0.0, ..modulator }, FmOperator { level: 0.0, ..modulator }, carrier],
            algorithm: 4,
            feedback: 0.0,
            filter_cutoff_hz: 18_000.0,
            voices: 12,
            gain_db: -8.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SampleZone {
    pub file: String,
    pub root_key: u8,
    pub key_range: (u8, u8),
    pub velocity_range: (u8, u8),
    pub gain_db: f32,
    pub tune_cents: f32,
    /// Loop region in seconds; `None` = one shot.
    pub loop_region: Option<(f64, f64)>,
}

impl Default for SampleZone {
    fn default() -> Self {
        Self { file: String::new(), root_key: 60, key_range: (0, 127), velocity_range: (1, 127), gain_db: 0.0, tune_cents: 0.0, loop_region: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MultiSamplerParams {
    pub zones: Vec<SampleZone>,
    pub amp_env: Adsr,
    pub filter_mode: FilterMode,
    pub cutoff_hz: f32,
    pub resonance: f32,
    pub glide: f32,
    pub voices: usize,
    pub gain_db: f32,
}

impl Default for MultiSamplerParams {
    fn default() -> Self {
        Self {
            zones: Vec::new(),
            amp_env: Adsr { attack: 0.001, decay: 0.1, sustain: 1.0, release: 0.2 },
            filter_mode: FilterMode::LowPass,
            cutoff_hz: 20_000.0,
            resonance: 0.0,
            glide: 0.0,
            voices: 32,
            gain_db: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// MIDI effects
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ArpStyle {
    #[default]
    Up,
    Down,
    UpDown,
    DownUp,
    AsPlayed,
    Random,
    /// All held notes together on every step.
    Chord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArpParams {
    pub style: ArpStyle,
    /// Step length in beats (0.25 = sixteenths).
    pub rate: Beats,
    /// Note length as a fraction of the step.
    pub gate: f32,
    pub octaves: u8,
    /// Hold notes after the keys are released, until new notes are played.
    pub hold: bool,
    /// Fixed velocity, or `None` to use the played velocity.
    pub velocity: Option<u8>,
}

impl Default for ArpParams {
    fn default() -> Self {
        Self { style: ArpStyle::Up, rate: 0.25, gate: 0.6, octaves: 1, hold: false, velocity: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChordParams {
    /// Semitone offsets added to each played note (0 is the played note itself).
    pub intervals: Vec<i8>,
    /// Velocity scale for the added notes.
    pub velocity: f32,
}

impl Default for ChordParams {
    fn default() -> Self {
        Self { intervals: vec![4, 7], velocity: 1.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ScaleKind {
    #[default]
    Major,
    Minor,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Locrian,
    HarmonicMinor,
    MelodicMinor,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
    WholeTone,
    Chromatic,
}

impl ScaleKind {
    pub const ALL: [ScaleKind; 14] = [
        ScaleKind::Major,
        ScaleKind::Minor,
        ScaleKind::Dorian,
        ScaleKind::Phrygian,
        ScaleKind::Lydian,
        ScaleKind::Mixolydian,
        ScaleKind::Locrian,
        ScaleKind::HarmonicMinor,
        ScaleKind::MelodicMinor,
        ScaleKind::MajorPentatonic,
        ScaleKind::MinorPentatonic,
        ScaleKind::Blues,
        ScaleKind::WholeTone,
        ScaleKind::Chromatic,
    ];

    /// Semitone degrees within an octave.
    pub fn degrees(&self) -> &'static [u8] {
        match self {
            ScaleKind::Major => &[0, 2, 4, 5, 7, 9, 11],
            ScaleKind::Minor => &[0, 2, 3, 5, 7, 8, 10],
            ScaleKind::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            ScaleKind::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            ScaleKind::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            ScaleKind::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            ScaleKind::Locrian => &[0, 1, 3, 5, 6, 8, 10],
            ScaleKind::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            ScaleKind::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
            ScaleKind::MajorPentatonic => &[0, 2, 4, 7, 9],
            ScaleKind::MinorPentatonic => &[0, 3, 5, 7, 10],
            ScaleKind::Blues => &[0, 3, 5, 6, 7, 10],
            ScaleKind::WholeTone => &[0, 2, 4, 6, 8, 10],
            ScaleKind::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        }
    }

    /// Snap a pitch to the nearest scale note at or below it.
    pub fn snap(&self, root: u8, pitch: u8) -> u8 {
        let rel = (pitch as i32 - root as i32).rem_euclid(12) as u8;
        let deg = self.degrees();
        let below = deg.iter().rev().find(|&&d| d <= rel).copied().unwrap_or(0);
        (pitch as i32 - (rel - below) as i32).clamp(0, 127) as u8
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScaleParams {
    pub root: u8,
    pub scale: ScaleKind,
    pub transpose: i8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RandomNoteParams {
    /// Chance (0..1) that a note is changed.
    pub chance: f32,
    /// Number of possible offsets.
    pub choices: u8,
    /// Semitones between choices.
    pub interval: u8,
    /// Snap results to a scale (Chromatic = no snapping).
    pub scale: ScaleKind,
    pub root: u8,
}

impl Default for RandomNoteParams {
    fn default() -> Self {
        Self { chance: 0.5, choices: 3, interval: 12, scale: ScaleKind::Chromatic, root: 0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NoteLengthParams {
    /// Fixed note length in beats.
    pub length: Beats,
    /// Trigger on note-off instead of note-on (release notes).
    pub on_release: bool,
}

impl Default for NoteLengthParams {
    fn default() -> Self {
        Self { length: 0.25, on_release: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VelocityParams {
    /// Output range the 1..127 input is scaled into.
    pub out_low: u8,
    pub out_high: u8,
    /// Random +- amount added after scaling.
    pub random: u8,
    /// Curve: <1 boosts soft notes, >1 favours loud ones.
    pub curve: f32,
}

impl Default for VelocityParams {
    fn default() -> Self {
        Self { out_low: 1, out_high: 127, random: 0, curve: 1.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PitchParams {
    pub semitones: i8,
    /// Notes outside this range are dropped.
    pub range: Option<(u8, u8)>,
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
        Self { file: String::new(), root_key: 60, amp_env: Adsr { attack: 0.001, decay: 0.1, sustain: 1.0, release: 0.1 }, gain_db: 0.0, loop_sample: false }
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
    pub gain_db: f32,
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
            gain_db: 0.0,
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

// ---------------------------------------------------------------------------
// More audio effects
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GlueParams {
    pub threshold_db: f32,
    /// 2, 4 or 10 in classic bus compressors; any value accepted.
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    /// Program-dependent release.
    pub auto_release: bool,
    pub makeup_db: f32,
    /// Maximum gain reduction.
    pub range_db: f32,
    pub soft_clip: bool,
    pub sidechain_track: Option<usize>,
    pub dry_wet: f32,
}

impl Default for GlueParams {
    fn default() -> Self {
        Self {
            threshold_db: -12.0,
            ratio: 4.0,
            attack_ms: 3.0,
            release_ms: 200.0,
            auto_release: true,
            makeup_db: 0.0,
            range_db: 30.0,
            soft_clip: false,
            sidechain_track: None,
            dry_wet: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LimiterParams {
    pub gain_db: f32,
    pub ceiling_db: f32,
    pub release_ms: f32,
    /// Lookahead (adds this much latency; compensated by the engine).
    pub lookahead_ms: f32,
}

impl Default for LimiterParams {
    fn default() -> Self {
        Self { gain_db: 0.0, ceiling_db: -0.3, release_ms: 80.0, lookahead_ms: 1.5 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GateParams {
    pub threshold_db: f32,
    /// Hysteresis: closes this many dB below the threshold.
    pub return_db: f32,
    pub attack_ms: f32,
    pub hold_ms: f32,
    pub release_ms: f32,
    /// Attenuation when closed.
    pub floor_db: f32,
    pub sidechain_track: Option<usize>,
}

impl Default for GateParams {
    fn default() -> Self {
        Self { threshold_db: -40.0, return_db: 3.0, attack_ms: 0.5, hold_ms: 10.0, release_ms: 60.0, floor_db: -80.0, sidechain_track: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MultibandParams {
    pub low_crossover_hz: f32,
    pub high_crossover_hz: f32,
    /// Per band (low, mid, high): threshold dB, ratio (>1 compress, <1 expand), gain dB.
    pub thresholds_db: [f32; 3],
    pub ratios: [f32; 3],
    pub gains_db: [f32; 3],
    pub attack_ms: f32,
    pub release_ms: f32,
    pub amount: f32,
}

impl Default for MultibandParams {
    fn default() -> Self {
        Self {
            low_crossover_hz: 120.0,
            high_crossover_hz: 2_500.0,
            thresholds_db: [-20.0, -20.0, -20.0],
            ratios: [3.0, 2.0, 3.0],
            gains_db: [0.0; 3],
            attack_ms: 10.0,
            release_ms: 120.0,
            amount: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvolutionParams {
    /// Impulse response file. Empty = a built-in synthesized hall.
    pub ir_file: String,
    /// Built-in IR decay time in seconds (used when `ir_file` is empty).
    pub decay_s: f32,
    pub predelay_ms: f32,
    pub low_cut_hz: f32,
    pub high_cut_hz: f32,
    pub dry_wet: f32,
}

impl Default for ConvolutionParams {
    fn default() -> Self {
        Self { ir_file: String::new(), decay_s: 2.2, predelay_ms: 15.0, low_cut_hz: 120.0, high_cut_hz: 9_000.0, dry_wet: 0.3 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DrumBussParams {
    pub drive: f32,
    pub crunch: f32,
    /// Low resonator ("boom") amount and tuning.
    pub boom: f32,
    pub boom_hz: f32,
    pub boom_decay: f32,
    /// Transient shaping, -1 (soften) .. 1 (sharpen).
    pub transients: f32,
    pub damping_hz: f32,
    pub output_db: f32,
    pub dry_wet: f32,
}

impl Default for DrumBussParams {
    fn default() -> Self {
        Self { drive: 0.2, crunch: 0.0, boom: 0.0, boom_hz: 55.0, boom_decay: 0.5, transients: 0.0, damping_hz: 18_000.0, output_db: 0.0, dry_wet: 1.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AmpModel {
    #[default]
    Clean,
    Crunch,
    Lead,
    Bass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AmpParams {
    pub model: AmpModel,
    pub gain: f32,
    pub bass: f32,
    pub middle: f32,
    pub treble: f32,
    pub presence: f32,
    /// Speaker cabinet simulation on/off.
    pub cabinet: bool,
    pub output_db: f32,
    pub dry_wet: f32,
}

impl Default for AmpParams {
    fn default() -> Self {
        Self { model: AmpModel::Crunch, gain: 0.5, bass: 0.5, middle: 0.5, treble: 0.5, presence: 0.5, cabinet: true, output_db: -6.0, dry_wet: 1.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReduxParams {
    /// Bit depth 1..24.
    pub bits: f32,
    /// Sample-rate reduction factor (1 = off).
    pub downsample: f32,
    pub dry_wet: f32,
}

impl Default for ReduxParams {
    fn default() -> Self {
        Self { bits: 8.0, downsample: 4.0, dry_wet: 1.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChorusParams {
    pub rate_hz: f32,
    pub depth_ms: f32,
    pub delay_ms: f32,
    pub feedback: f32,
    pub width: f32,
    pub voices: usize,
    pub dry_wet: f32,
}

impl Default for ChorusParams {
    fn default() -> Self {
        Self { rate_hz: 0.6, depth_ms: 3.0, delay_ms: 12.0, feedback: 0.0, width: 1.0, voices: 2, dry_wet: 0.5 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FlangerParams {
    pub rate_hz: f32,
    /// Sync the LFO to beats instead of Hz.
    pub sync_beats: Option<f64>,
    pub depth_ms: f32,
    pub delay_ms: f32,
    pub feedback: f32,
    pub dry_wet: f32,
}

impl Default for FlangerParams {
    fn default() -> Self {
        Self { rate_hz: 0.25, sync_beats: None, depth_ms: 2.0, delay_ms: 1.5, feedback: 0.6, dry_wet: 0.5 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PhaserParams {
    pub stages: usize,
    pub rate_hz: f32,
    pub sync_beats: Option<f64>,
    pub center_hz: f32,
    /// LFO sweep in octaves.
    pub depth: f32,
    pub feedback: f32,
    pub dry_wet: f32,
}

impl Default for PhaserParams {
    fn default() -> Self {
        Self { stages: 6, rate_hz: 0.4, sync_beats: None, center_hz: 900.0, depth: 2.0, feedback: 0.5, dry_wet: 0.5 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LfoShape {
    #[default]
    Sine,
    Triangle,
    Square,
    SawUp,
    SawDown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoPanParams {
    /// LFO period in beats.
    pub rate_beats: f64,
    pub amount: f32,
    pub shape: LfoShape,
    /// Right-channel phase offset in degrees; 180 = autopan, 0 = tremolo.
    pub phase_deg: f32,
}

impl Default for AutoPanParams {
    fn default() -> Self {
        Self { rate_beats: 1.0, amount: 0.6, shape: LfoShape::Sine, phase_deg: 180.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BeatRepeatParams {
    /// How often a repeat may start (beats).
    pub interval: Beats,
    /// Offset into the interval where repeats start (beats).
    pub offset: Beats,
    /// Repeat length (beats).
    pub grid: Beats,
    /// How long repeating lasts once triggered (beats).
    pub gate: Beats,
    /// Chance per interval.
    pub chance: f32,
    /// Each repeat this much quieter (0..1).
    pub decay: f32,
    /// Mix: insert (replace) vs. mix with the dry signal.
    pub insert: bool,
    pub volume_db: f32,
}

impl Default for BeatRepeatParams {
    fn default() -> Self {
        Self { interval: 4.0, offset: 2.0, grid: 0.25, gate: 2.0, chance: 1.0, decay: 0.0, insert: true, volume_db: 0.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GrainDelayParams {
    pub time_beats: f64,
    pub feedback: f32,
    pub pitch_semitones: f32,
    /// Random pitch spray in semitones.
    pub spray: f32,
    pub grain_ms: f32,
    pub dry_wet: f32,
}

impl Default for GrainDelayParams {
    fn default() -> Self {
        Self { time_beats: 0.5, feedback: 0.3, pitch_semitones: 12.0, spray: 0.0, grain_ms: 60.0, dry_wet: 0.4 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VocoderCarrier {
    /// Built-in sawtooth chord at `carrier_hz`.
    #[default]
    Saw,
    Noise,
    /// The track named by `sidechain_track` provides the carrier.
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VocoderParams {
    pub bands: usize,
    pub carrier: VocoderCarrier,
    pub carrier_hz: f32,
    pub sidechain_track: Option<usize>,
    pub low_hz: f32,
    pub high_hz: f32,
    pub release_ms: f32,
    pub dry_wet: f32,
}

impl Default for VocoderParams {
    fn default() -> Self {
        Self {
            bands: 16,
            carrier: VocoderCarrier::Saw,
            carrier_hz: 110.0,
            sidechain_track: None,
            low_hz: 80.0,
            high_hz: 10_000.0,
            release_ms: 40.0,
            dry_wet: 1.0,
        }
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
    /// Parameter values by the plugin's parameter index (empty = defaults).
    #[serde(default)]
    pub params: Vec<f32>,
}
