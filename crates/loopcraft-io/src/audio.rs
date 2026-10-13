//! Decoding and encoding audio files.

use anyhow::{Context, Result, bail};
use loopcraft_dsp::{AudioBuffer, SampleLoader};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// File extensions `read_audio` can decode.
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "wave", "aif", "aiff", "flac", "mp3", "ogg", "oga"];

/// Decode any supported audio file (WAV, AIFF, FLAC, MP3, Ogg Vorbis).
/// Channels beyond two are dropped; mono stays mono.
pub fn read_audio(path: &Path) -> Result<AudioBuffer> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if ext == "wav" || ext == "wave" {
        // hound is faster and handles WAV formats symphonia may not.
        if let Ok(b) = read_wav(path) {
            return Ok(b);
        }
    }
    decode_with_symphonia(path)
}

fn decode_with_symphonia(path: &Path) -> Result<AudioBuffer> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error as SymErr;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .with_context(|| format!("{}: unsupported or corrupt audio file", path.display()))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or_else(|| anyhow::anyhow!("{} has no audio track", path.display()))?;
    let track_id = track.id;
    let sr = track.codec_params.sample_rate.unwrap_or(44_100) as f32;
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut chans: Vec<Vec<f32>> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymErr::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymErr::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count().max(1);
        if chans.is_empty() {
            chans = vec![Vec::new(); ch.min(2)];
        }
        let mut sb = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        sb.copy_interleaved_ref(decoded);
        for frame in sb.samples().chunks(ch) {
            for (c, out) in chans.iter_mut().enumerate() {
                out.push(frame[c.min(frame.len() - 1)]);
            }
        }
    }
    if chans.is_empty() || chans[0].is_empty() {
        bail!("{} contains no audio", path.display());
    }
    Ok(AudioBuffer { sample_rate: sr, channels: chans })
}

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

/// Encode stereo to FLAC (16 or 24 bit).
pub fn write_flac(path: &Path, left: &[f32], right: &[f32], sample_rate: u32, bits: u32) -> Result<()> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    let bits = if bits >= 24 { 24 } else { 16 };
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    let n = left.len().min(right.len());
    let mut samples = Vec::with_capacity(n * 2);
    for i in 0..n {
        samples.push((left[i].clamp(-1.0, 1.0) * max).round() as i32);
        samples.push((right[i].clamp(-1.0, 1.0) * max).round() as i32);
    }
    let config = flacenc::config::Encoder::default().into_verified().map_err(|e| anyhow::anyhow!("flac config: {e:?}"))?;
    let source = flacenc::source::MemSource::from_samples(&samples, 2, bits as usize, sample_rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size).map_err(|e| anyhow::anyhow!("flac encode: {e:?}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| anyhow::anyhow!("flac write: {e:?}"))?;
    let mut bytes = sink.as_slice().to_vec();
    // The spec says STREAMINFO's minimum block size excludes the final
    // (short) block; some decoders reject files where it doesn't.
    if bytes.len() > 12 && &bytes[0..4] == b"fLaC" {
        let (a, b) = (bytes[10], bytes[11]);
        bytes[8] = a;
        bytes[9] = b;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// 80-bit IEEE extended float, as AIFF stores its sample rate.
fn ieee_extended(v: f64) -> [u8; 10] {
    let mut out = [0u8; 10];
    if v <= 0.0 {
        return out;
    }
    let exp = v.log2().floor() as i32;
    let mant = (v / 2f64.powi(exp) * (1u64 << 63) as f64) as u64;
    let e = (exp + 16383) as u16;
    out[0..2].copy_from_slice(&e.to_be_bytes());
    out[2..10].copy_from_slice(&mant.to_be_bytes());
    out
}

/// Write 16/24-bit stereo AIFF.
pub fn write_aiff(path: &Path, left: &[f32], right: &[f32], sample_rate: u32, bits: u32) -> Result<()> {
    let bits: u16 = if bits >= 24 { 24 } else { 16 };
    let bytes = (bits / 8) as usize;
    let n = left.len().min(right.len());
    let data_len = n * 2 * bytes;
    let mut out = Vec::with_capacity(54 + data_len);
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&((4 + 26 + 16 + data_len) as u32).to_be_bytes());
    out.extend_from_slice(b"AIFF");
    out.extend_from_slice(b"COMM");
    out.extend_from_slice(&18u32.to_be_bytes());
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(&(n as u32).to_be_bytes());
    out.extend_from_slice(&bits.to_be_bytes());
    out.extend_from_slice(&ieee_extended(sample_rate as f64));
    out.extend_from_slice(b"SSND");
    out.extend_from_slice(&((8 + data_len) as u32).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    for i in 0..n {
        for s in [left[i], right[i]] {
            let v = (s.clamp(-1.0, 1.0) * max).round() as i32;
            out.extend_from_slice(&v.to_be_bytes()[4 - bytes..]);
        }
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Write by extension: .wav, .flac, .aif/.aiff.
pub fn write_audio(path: &Path, left: &[f32], right: &[f32], sample_rate: u32, depth: BitDepth) -> Result<()> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let bits = match depth {
        BitDepth::Int16 => 16,
        _ => 24,
    };
    match ext.as_str() {
        "flac" => write_flac(path, left, right, sample_rate, bits),
        "aif" | "aiff" => write_aiff(path, left, right, sample_rate, bits),
        _ => write_wav(path, left, right, sample_rate, depth),
    }
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
        let result = match read_audio(&self.resolve(path)) {
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
