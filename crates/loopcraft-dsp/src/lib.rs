//! LoopCraft DSP: built-in instruments, audio effects and time-stretching.
//!
//! Every device here is real-time safe once constructed: `process`/`render`
//! never allocate, lock or do I/O. Construction (which may allocate delay
//! lines or decode samples) happens on a non-audio thread.

pub mod drums;
pub mod effects;
pub mod fm;
pub mod midi;
pub mod multisampler;
pub mod plugin_params;
pub mod rack;
pub mod sampler;
pub mod specs;
pub mod stretch;
pub mod synth;
pub mod util;
pub mod wavetable;

use loopcraft_core::{DeviceSpec, PluginRef};
use std::sync::Arc;

/// Per-block information devices may need (tempo-synced delays, LFOs).
#[derive(Debug, Clone, Copy)]
pub struct ProcessContext {
    pub sample_rate: f32,
    pub tempo: f64,
    /// Song position (beats) at the first sample of the block.
    pub beat: f64,
    pub playing: bool,
}

impl ProcessContext {
    pub fn new(sample_rate: f32, tempo: f64) -> Self {
        Self { sample_rate, tempo, beat: 0.0, playing: false }
    }
}

/// A parameter a device exposes for automation and UI.
#[derive(Debug, Clone, Copy)]
pub struct ParamInfo {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub unit: &'static str,
    /// JSON-pointer path of this parameter inside the device's saved params
    /// (e.g. `amp_env/attack`). Lets the UI read and write the saved spec
    /// with the same index the running device uses.
    pub path: &'static str,
}

impl ParamInfo {
    pub const fn new(name: &'static str, min: f32, max: f32, unit: &'static str) -> Self {
        Self { name, min, max, unit, path: "" }
    }
    pub const fn at(self, path: &'static str) -> Self {
        Self { path, ..self }
    }
    /// Map a normalized 0..1 automation value into this parameter's range.
    pub fn denormalize(&self, v: f32) -> f32 {
        self.min + (self.max - self.min) * v.clamp(0.0, 1.0)
    }
}

/// A sound source driven by notes.
pub trait Instrument: Send {
    fn note_on(&mut self, note: u8, velocity: u8);
    fn note_off(&mut self, note: u8);
    fn all_notes_off(&mut self);
    /// Render one block, *adding* into `left`/`right`.
    fn render(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]);
    fn params(&self) -> &'static [ParamInfo] {
        &[]
    }
    /// Set a parameter in its natural units (see `params()`).
    fn set_param(&mut self, _index: usize, _value: f32) {}
    fn name(&self) -> &str;
}

/// An in-place stereo audio processor.
pub trait Effect: Send {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]);
    fn reset(&mut self) {}
    fn params(&self) -> &'static [ParamInfo] {
        &[]
    }
    fn set_param(&mut self, _index: usize, _value: f32) {}
    /// Feed the sidechain key signal before `process` is called for this block.
    fn sidechain(&mut self, _left: &[f32], _right: &[f32]) {}
    fn wants_sidechain(&self) -> Option<usize> {
        None
    }
    /// Samples of delay this effect adds (lookahead, FFT blocks). The
    /// engine compensates so everything stays aligned.
    fn latency(&self) -> usize {
        0
    }
    fn name(&self) -> &str;
}

/// Decoded, in-memory audio at the engine sample rate.
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    pub sample_rate: f32,
    /// One Vec per channel. Mono files have one channel.
    pub channels: Vec<Vec<f32>>,
}

impl AudioBuffer {
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, |c| c.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn silence(sample_rate: f32, frames: usize) -> Self {
        Self { sample_rate, channels: vec![vec![0.0; frames]; 2] }
    }

    /// Linear-interpolated read of channel `ch` (mono files return the same
    /// data for both channels). Out-of-range positions read as silence.
    #[inline]
    pub fn read(&self, ch: usize, pos: f64) -> f32 {
        let c = &self.channels[ch.min(self.channels.len() - 1)];
        if pos < 0.0 {
            return 0.0;
        }
        let i = pos as usize;
        if i + 1 >= c.len() {
            return if i < c.len() { c[i] } else { 0.0 };
        }
        let f = (pos - i as f64) as f32;
        c[i] + (c[i + 1] - c[i]) * f
    }

    /// Resample to `target_sr` with linear interpolation. Done once at load
    /// time so the audio thread never resamples files on the fly.
    pub fn resampled(&self, target_sr: f32) -> Self {
        if (self.sample_rate - target_sr).abs() < 0.5 || self.is_empty() {
            return Self { sample_rate: target_sr, channels: self.channels.clone() };
        }
        let ratio = self.sample_rate as f64 / target_sr as f64;
        let out_len = (self.len() as f64 / ratio).floor() as usize;
        let channels = (0..self.channels.len()).map(|ch| (0..out_len).map(|i| self.read(ch, i as f64 * ratio)).collect()).collect();
        Self { sample_rate: target_sr, channels }
    }
}

/// Supplies decoded audio files to devices that need them (Sampler, Drum
/// Rack). Implemented by the IO layer; returns `None` for missing files.
pub trait SampleLoader {
    fn load(&self, path: &str) -> Option<Arc<AudioBuffer>>;
}

/// A loader that never finds anything; useful for tests and headless setups.
pub struct NoSamples;

impl SampleLoader for NoSamples {
    fn load(&self, _path: &str) -> Option<Arc<AudioBuffer>> {
        None
    }
}

pub enum Device {
    Instrument(Box<dyn Instrument>),
    MidiEffect(Box<dyn midi::MidiEffect>),
    Effect(Box<dyn Effect>),
}

impl Device {
    pub fn into_instrument(self) -> Option<Box<dyn Instrument>> {
        if let Device::Instrument(i) = self { Some(i) } else { None }
    }
    pub fn into_effect(self) -> Option<Box<dyn Effect>> {
        if let Device::Effect(e) = self { Some(e) } else { None }
    }
    pub fn into_midi_effect(self) -> Option<Box<dyn midi::MidiEffect>> {
        if let Device::MidiEffect(m) = self { Some(m) } else { None }
    }
}

/// Build a built-in device from its saved description. Plugins return
/// `None`; use [`build_device_with`] to supply a plugin host.
pub fn build_device(spec: &DeviceSpec, sample_rate: f32, loader: &dyn SampleLoader) -> Option<Device> {
    build_device_with(spec, sample_rate, loader, &|_| None)
}

/// Build any device, delegating third-party plugins (including ones nested
/// inside racks) to `plugins`.
pub fn build_device_with(spec: &DeviceSpec, sr: f32, loader: &dyn SampleLoader, plugins: &dyn Fn(&PluginRef) -> Option<Device>) -> Option<Device> {
    use effects::*;
    let fx = |e: Box<dyn Effect>| Some(Device::Effect(e));
    let inst = |i: Box<dyn Instrument>| Some(Device::Instrument(i));
    match spec {
        DeviceSpec::PolySynth(p) => inst(Box::new(synth::PolySynth::new(p.clone(), sr))),
        DeviceSpec::Wavetable(p) => inst(Box::new(wavetable::Wavetable::new(p.clone(), sr))),
        DeviceSpec::FmSynth(p) => inst(Box::new(fm::FmSynth::new(p.clone(), sr))),
        DeviceSpec::Sampler(p) => inst(Box::new(sampler::Sampler::new(p.clone(), loader.load(&p.file), sr))),
        DeviceSpec::MultiSampler(p) => inst(Box::new(multisampler::MultiSampler::new(p.clone(), sr, loader))),
        DeviceSpec::DrumRack(p) => inst(Box::new(drums::DrumRack::new(p, sr, loader))),
        DeviceSpec::InstrumentRack(p) => inst(Box::new(rack::InstrumentRack::new(
            p,
            |d| build_device_with(d, sr, loader, plugins).and_then(Device::into_instrument),
            |d| build_device_with(d, sr, loader, plugins).and_then(Device::into_effect),
        ))),
        DeviceSpec::Arpeggiator(_)
        | DeviceSpec::Chord(_)
        | DeviceSpec::Scale(_)
        | DeviceSpec::RandomNote(_)
        | DeviceSpec::NoteLength(_)
        | DeviceSpec::Velocity(_)
        | DeviceSpec::Pitch(_) => midi::build_midi_effect(spec).map(Device::MidiEffect),
        DeviceSpec::Eq(p) => fx(Box::new(eq::ParametricEq::new(p.clone(), sr))),
        DeviceSpec::Compressor(p) => fx(Box::new(compressor::Compressor::new(p.clone(), sr))),
        DeviceSpec::GlueCompressor(p) => fx(Box::new(dynamics::GlueCompressor::new(p.clone(), sr))),
        DeviceSpec::Limiter(p) => fx(Box::new(dynamics::Limiter::new(p.clone(), sr))),
        DeviceSpec::Gate(p) => fx(Box::new(dynamics::Gate::new(p.clone(), sr))),
        DeviceSpec::MultibandDynamics(p) => fx(Box::new(dynamics::Multiband::new(p.clone(), sr))),
        DeviceSpec::Delay(p) => fx(Box::new(delay::Delay::new(p.clone(), sr))),
        DeviceSpec::Reverb(p) => fx(Box::new(reverb::Reverb::new(p.clone(), sr))),
        DeviceSpec::ConvolutionReverb(p) => {
            let ir = if p.ir_file.is_empty() { None } else { loader.load(&p.ir_file) };
            fx(Box::new(convolution::Convolution::new(p.clone(), ir, sr)))
        }
        DeviceSpec::Saturator(p) => fx(Box::new(saturator::Saturator::new(p.clone()))),
        DeviceSpec::DrumBuss(p) => fx(Box::new(character::DrumBuss::new(p.clone(), sr))),
        DeviceSpec::Amp(p) => fx(Box::new(character::Amp::new(p.clone(), sr))),
        DeviceSpec::Redux(p) => fx(Box::new(modulation::Redux::new(p.clone()))),
        DeviceSpec::Chorus(p) => fx(Box::new(modulation::Chorus::new(p.clone(), sr))),
        DeviceSpec::Flanger(p) => fx(Box::new(modulation::Flanger::new(p.clone(), sr))),
        DeviceSpec::Phaser(p) => fx(Box::new(modulation::Phaser::new(p.clone(), sr))),
        DeviceSpec::AutoPan(p) => fx(Box::new(modulation::AutoPan::new(p.clone(), sr))),
        DeviceSpec::Utility(p) => fx(Box::new(utility::Utility::new(p.clone(), sr))),
        DeviceSpec::AutoFilter(p) => fx(Box::new(autofilter::AutoFilter::new(p.clone(), sr))),
        DeviceSpec::BeatRepeat(p) => fx(Box::new(creative::BeatRepeat::new(p.clone(), sr))),
        DeviceSpec::GrainDelay(p) => fx(Box::new(creative::GrainDelay::new(p.clone(), sr))),
        DeviceSpec::Vocoder(p) => fx(Box::new(creative::Vocoder::new(p.clone(), sr))),
        DeviceSpec::AudioRack(p) => fx(Box::new(rack::AudioRack::new(p, |d| build_device_with(d, sr, loader, plugins).and_then(Device::into_effect)))),
        DeviceSpec::Plugin(p) => plugins(p),
    }
}

/// Parameter table of a running device.
pub fn device_params(d: &Device) -> &'static [ParamInfo] {
    match d {
        Device::Instrument(i) => i.params(),
        Device::MidiEffect(m) => m.params(),
        Device::Effect(e) => e.params(),
    }
}

/// Peak and RMS of a block, for meters and tests.
pub fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

pub fn rms(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt()
}
