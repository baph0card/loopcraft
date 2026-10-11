//! LoopCraft DSP: built-in instruments, audio effects and time-stretching.
//!
//! Every device here is real-time safe once constructed: `process`/`render`
//! never allocate, lock or do I/O. Construction (which may allocate delay
//! lines or decode samples) happens on a non-audio thread.

pub mod drums;
pub mod effects;
pub mod sampler;
pub mod specs;
pub mod stretch;
pub mod synth;
pub mod util;

use loopcraft_core::DeviceSpec;
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
}

impl ParamInfo {
    pub const fn new(name: &'static str, min: f32, max: f32, unit: &'static str) -> Self {
        Self { name, min, max, unit }
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
        let channels = (0..self.channels.len())
            .map(|ch| (0..out_len).map(|i| self.read(ch, i as f64 * ratio)).collect())
            .collect();
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
    Effect(Box<dyn Effect>),
}

/// Build a built-in device from its saved description. Plugins are handled by
/// `loopcraft-plugin` and return `None` here.
pub fn build_device(spec: &DeviceSpec, sample_rate: f32, loader: &dyn SampleLoader) -> Option<Device> {
    use effects::*;
    Some(match spec {
        DeviceSpec::PolySynth(p) => Device::Instrument(Box::new(synth::PolySynth::new(p.clone(), sample_rate))),
        DeviceSpec::Sampler(p) => Device::Instrument(Box::new(sampler::Sampler::new(p.clone(), loader.load(&p.file), sample_rate))),
        DeviceSpec::DrumRack(p) => Device::Instrument(Box::new(drums::DrumRack::new(p, sample_rate, loader))),
        DeviceSpec::Eq(p) => Device::Effect(Box::new(eq::ParametricEq::new(p.clone(), sample_rate))),
        DeviceSpec::Compressor(p) => Device::Effect(Box::new(compressor::Compressor::new(p.clone(), sample_rate))),
        DeviceSpec::Delay(p) => Device::Effect(Box::new(delay::Delay::new(p.clone(), sample_rate))),
        DeviceSpec::Reverb(p) => Device::Effect(Box::new(reverb::Reverb::new(p.clone(), sample_rate))),
        DeviceSpec::Saturator(p) => Device::Effect(Box::new(saturator::Saturator::new(p.clone()))),
        DeviceSpec::Utility(p) => Device::Effect(Box::new(utility::Utility::new(p.clone(), sample_rate))),
        DeviceSpec::AutoFilter(p) => Device::Effect(Box::new(autofilter::AutoFilter::new(p.clone(), sample_rate))),
        DeviceSpec::Plugin(_) => return None,
    })
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
