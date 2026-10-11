//! Decoding and encoding audio files.

use anyhow::{Context, Result, bail};
use loopcraft_dsp::{AudioBuffer, SampleLoader};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Decode a WAV file (8/16/24/32-bit integer or 32-bit float, any channel
/// count; channels beyond two are dropped).
pub fn read_wav(path: &Path) -> Result<AudioBuffer> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    if interleaved.is_empty() {
        bail!("{} contains no audio", path.display());
    }
    let keep = ch.min(2);
    let frames = interleaved.len() / ch;
    let channels = (0..keep).map(|c| (0..frames).map(|f| interleaved[f * ch + c]).collect()).collect();
    Ok(AudioBuffer { sample_rate: spec.sample_rate as f32, channels })
}

#[derive(Clone, Copy, Debug)]
pub enum BitDepth {
    Int16,
    Int24,
    Float32,
}

/// Write planar stereo to a WAV file.
pub fn write_wav(path: &Path, left: &[f32], right: &[f32], sample_rate: u32, depth: BitDepth) -> Result<()> {
    let (bits, fmt) = match depth {
        BitDepth::Int16 => (16, hound::SampleFormat::Int),
        BitDepth::Int24 => (24, hound::SampleFormat::Int),
        BitDepth::Float32 => (32, hound::SampleFormat::Float),
    };
    let spec = hound::WavSpec { channels: 2, sample_rate, bits_per_sample: bits, sample_format: fmt };
    let mut w = hound::WavWriter::create(path, spec).with_context(|| format!("creating {}", path.display()))?;
    let n = left.len().min(right.len());
    match depth {
        BitDepth::Float32 => {
            for i in 0..n {
                w.write_sample(left[i])?;
                w.write_sample(right[i])?;
            }
        }
        BitDepth::Int16 | BitDepth::Int24 => {
            let max = ((1i64 << (bits - 1)) - 1) as f32;
            // TPDF dither on quantization to integer formats.
            let mut seed = 0x2545_F491u32;
            let mut rnd = move || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as f32 / u32::MAX as f32
            };
            for i in 0..n {
                for s in [left[i], right[i]] {
                    let d = (rnd() - rnd()) / max;
                    let v = ((s + d).clamp(-1.0, 1.0) * max).round() as i32;
                    w.write_sample(v)?;
                }
            }
        }
    }
    w.finalize()?;
    Ok(())
}

/// Loads files relative to a project directory, resamples them to the
/// engine rate once, and caches the result.
pub struct FileLoader {
    base: PathBuf,
    sample_rate: f32,
    cache: Mutex<HashMap<String, Option<Arc<AudioBuffer>>>>,
    errors: Mutex<Vec<String>>,
}

impl FileLoader {
    pub fn new(base: impl Into<PathBuf>, sample_rate: f32) -> Self {
        Self { base: base.into(), sample_rate, cache: Mutex::default(), errors: Mutex::default() }
    }

    pub fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() { p.to_path_buf() } else { self.base.join(p) }
    }

    /// Files that failed to load, with the reason.
    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().map(|e| e.clone()).unwrap_or_default()
    }
}

impl SampleLoader for FileLoader {
    fn load(&self, path: &str) -> Option<Arc<AudioBuffer>> {
        if path.is_empty() {
            return None;
        }
        let mut cache = self.cache.lock().ok()?;
        if let Some(hit) = cache.get(path) {
            return hit.clone();
        }
        let result = match read_wav(&self.resolve(path)) {
            Ok(buf) => Some(Arc::new(buf.resampled(self.sample_rate))),
            Err(e) => {
                if let Ok(mut errs) = self.errors.lock() {
                    errs.push(format!("{path}: {e:#}"));
                }
                None
            }
        };
        cache.insert(path.to_string(), result.clone());
        result
    }
}
