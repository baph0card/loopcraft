//! Creative effects: Beat Repeat, Grain Delay and Vocoder.

use super::{MAX_BLOCK, mix};
use crate::util::{Biquad, BiquadKind, DelayLine, EnvFollower, Noise};
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::*;

// ---------------------------------------------------------------------------
// Beat Repeat
// ---------------------------------------------------------------------------

const BR_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Interval", 0.25, 16.0, "beats").at("interval"),
    ParamInfo::new("Offset", 0.0, 16.0, "beats").at("offset"),
    ParamInfo::new("Grid", 0.03125, 4.0, "beats").at("grid"),
    ParamInfo::new("Gate", 0.0625, 16.0, "beats").at("gate"),
    ParamInfo::new("Chance", 0.0, 1.0, "").at("chance"),
    ParamInfo::new("Decay", 0.0, 1.0, "").at("decay"),
];

/// Captures a slice of the incoming audio on the beat grid and stutters it.
pub struct BeatRepeat {
    p: BeatRepeatParams,
    sr: f32,
    buf: [Vec<f32>; 2],
    cap_len: usize,
    /// Song beat where the active repeat started.
    active: Option<f64>,
    last_interval: i64,
    rng: Noise,
}

impl BeatRepeat {
    pub fn new(p: BeatRepeatParams, sr: f32) -> Self {
        // Up to 4 beats of capture at 30 bpm.
        let n = (8.0 * sr) as usize;
        Self { p, sr, buf: [vec![0.0; n], vec![0.0; n]], cap_len: 0, active: None, last_interval: i64::MIN, rng: Noise::with_seed(0xbea7) }
    }
}

impl Effect for BeatRepeat {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        if !ctx.playing {
            self.active = None;
            return;
        }
        let bpf = ctx.tempo / 60.0 / self.sr as f64;
        let frames_per_beat = 1.0 / bpf;
        let grid_frames = ((self.p.grid.max(1.0 / 64.0) * frames_per_beat) as usize).clamp(1, self.buf[0].len());
        let interval = self.p.interval.max(0.25);
        let vol = db_to_gain(self.p.volume_db);
        for n in 0..left.len() {
            let beat = ctx.beat + n as f64 * bpf;
            let iv = (beat / interval).floor() as i64;
            let in_iv = beat - iv as f64 * interval;
            // Trigger once per interval at the offset.
            if iv != self.last_interval && in_iv >= self.p.offset.rem_euclid(interval) {
                self.last_interval = iv;
                if self.rng.unit() < self.p.chance {
                    self.active = Some(beat);
                    self.cap_len = 0;
                }
            }
            let Some(start) = self.active else { continue };
            let since = beat - start;
            if since >= self.p.gate {
                self.active = None;
                continue;
            }
            let i = (since * frames_per_beat) as usize;
            if i < grid_frames {
                // First pass: record while playing through.
                if self.cap_len < self.buf[0].len() {
                    self.buf[0][self.cap_len] = left[n];
                    self.buf[1][self.cap_len] = right[n];
                    self.cap_len += 1;
                }
                continue;
            }
            let len = self.cap_len.max(1);
            let rep = i / grid_frames;
            let k = (i % grid_frames).min(len - 1);
            let g = vol * (1.0 - self.p.decay.clamp(0.0, 1.0)).powi(rep as i32);
            let (wl, wr) = (self.buf[0][k] * g, self.buf[1][k] * g);
            if self.p.insert {
                left[n] = wl;
                right[n] = wr;
            } else {
                left[n] += wl;
                right[n] += wr;
            }
        }
    }
    fn reset(&mut self) {
        self.active = None;
        self.last_interval = i64::MIN;
    }
    fn params(&self) -> &'static [ParamInfo] {
        BR_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.interval = v as f64,
            1 => self.p.offset = v as f64,
            2 => self.p.grid = v as f64,
            3 => self.p.gate = v as f64,
            4 => self.p.chance = v,
            5 => self.p.decay = v,
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Beat Repeat"
    }
}

// ---------------------------------------------------------------------------
// Grain Delay
// ---------------------------------------------------------------------------

const GD_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Time", 0.0625, 4.0, "beats").at("time_beats"),
    ParamInfo::new("Feedback", 0.0, 0.95, "").at("feedback"),
    ParamInfo::new("Pitch", -24.0, 24.0, "st").at("pitch_semitones"),
    ParamInfo::new("Spray", 0.0, 12.0, "st").at("spray"),
    ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet"),
];

/// Delay whose output is pitch-shifted by two crossfading read heads.
pub struct GrainDelay {
    p: GrainDelayParams,
    sr: f32,
    dl: [DelayLine; 2],
    phase: f32,
    ratio: f32,
    rng: Noise,
    fb: [f32; 2],
}

impl GrainDelay {
    pub fn new(p: GrainDelayParams, sr: f32) -> Self {
        let n = (5.0 * sr) as usize;
        Self { dl: [DelayLine::new(n), DelayLine::new(n)], phase: 0.0, ratio: 2f32.powf(p.pitch_semitones / 12.0), rng: Noise::with_seed(77), fb: [0.0; 2], p, sr }
    }
}

impl Effect for GrainDelay {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let base = ((self.p.time_beats * 60.0 / ctx.tempo.max(1.0)) as f32 * self.sr).clamp(64.0, self.dl[0].len() as f32 - 4.0 * self.sr * 0.25);
        let grain = (self.p.grain_ms.clamp(10.0, 250.0) * 0.001 * self.sr).max(32.0);
        for n in 0..left.len() {
            // Two heads half a grain apart sweep the pitch-shift window.
            self.phase += (1.0 - self.ratio) / grain;
            if self.phase >= 1.0 || self.phase < 0.0 {
                self.phase = self.phase.rem_euclid(1.0);
                if self.p.spray > 0.0 {
                    let st = self.p.pitch_semitones + self.rng.next() * self.p.spray;
                    self.ratio = 2f32.powf(st / 12.0);
                }
            }
            let ins = [left[n], right[n]];
            let mut wet = [0.0f32; 2];
            for (c, w) in wet.iter_mut().enumerate() {
                let mut y = 0.0;
                for k in 0..2 {
                    let ph = (self.phase + k as f32 * 0.5).fract();
                    let win = (std::f32::consts::PI * ph).sin();
                    y += self.dl[c].read(base + ph * grain) * win * win;
                }
                *w = y;
                self.dl[c].write(ins[c] + self.fb[c] * self.p.feedback.clamp(0.0, 0.95));
                self.fb[c] = y;
            }
            left[n] = mix(ins[0], wet[0], self.p.dry_wet);
            right[n] = mix(ins[1], wet[1], self.p.dry_wet);
        }
    }
    fn reset(&mut self) {
        self.dl.iter_mut().for_each(DelayLine::clear);
        self.fb = [0.0; 2];
    }
    fn params(&self) -> &'static [ParamInfo] {
        GD_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.time_beats = v as f64,
            1 => self.p.feedback = v,
            2 => {
                self.p.pitch_semitones = v;
                self.ratio = 2f32.powf(v / 12.0);
            }
            3 => self.p.spray = v,
            4 => self.p.dry_wet = v,
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Grain Delay"
    }
}

// ---------------------------------------------------------------------------
// Vocoder
// ---------------------------------------------------------------------------

const VOC_PARAMS: &[ParamInfo] = &[ParamInfo::new("Carrier Hz", 40.0, 880.0, "Hz").at("carrier_hz"), ParamInfo::new("Release", 1.0, 500.0, "ms").at("release_ms"), ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet")];

const MAX_BANDS: usize = 40;

/// Channel vocoder: the input (modulator) is split into bands, their
/// envelopes shape the matching bands of a carrier.
pub struct Vocoder {
    p: VocoderParams,
    sr: f32,
    n: usize,
    mod_bp: [[Biquad; 2]; MAX_BANDS],
    car_bp: [[Biquad; 2]; MAX_BANDS],
    env: [EnvFollower; MAX_BANDS],
    phases: [f32; 3],
    noise: Noise,
    key: Vec<f32>,
    key_len: usize,
}

impl Vocoder {
    pub fn new(p: VocoderParams, sr: f32) -> Self {
        let mut v = Self {
            n: 0,
            mod_bp: [[Biquad::default(); 2]; MAX_BANDS],
            car_bp: [[Biquad::default(); 2]; MAX_BANDS],
            env: [EnvFollower::new(2.0, p.release_ms, sr); MAX_BANDS],
            phases: [0.0, 0.33, 0.66],
            noise: Noise::with_seed(9),
            key: vec![0.0; MAX_BLOCK],
            key_len: 0,
            p,
            sr,
        };
        v.update();
        v
    }

    fn update(&mut self) {
        self.n = self.p.bands.clamp(4, MAX_BANDS);
        let lo = self.p.low_hz.max(20.0);
        let hi = self.p.high_hz.min(self.sr * 0.45).max(lo * 2.0);
        let ratio = (hi / lo).powf(1.0 / (self.n - 1) as f32);
        // Q so neighbouring bands cross near -3 dB.
        let q = ratio.sqrt() / (ratio - 1.0);
        for b in 0..self.n {
            let f = lo * ratio.powi(b as i32);
            for k in 0..2 {
                self.mod_bp[b][k].design(BiquadKind::BandPass, f, q, 0.0, self.sr);
                self.car_bp[b][k].design(BiquadKind::BandPass, f, q, 0.0, self.sr);
            }
            self.env[b].set(2.0, self.p.release_ms, self.sr);
        }
    }
}

impl Effect for Vocoder {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let use_key = self.p.carrier == VocoderCarrier::External && self.key_len >= left.len();
        let inc = self.p.carrier_hz / self.sr;
        let n_b = self.n;
        // Compensate for the energy lost by splitting into narrow bands.
        let makeup = 2.0 * (n_b as f32).sqrt();
        for n in 0..left.len() {
            let m = (left[n] + right[n]) * 0.5;
            let carrier = if use_key {
                self.key[n]
            } else {
                match self.p.carrier {
                    VocoderCarrier::Noise => self.noise.next() * 0.5,
                    _ => {
                        // Three saws a fifth and an octave apart.
                        let mut s = 0.0;
                        for (k, mult) in [1.0f32, 1.5, 2.0].into_iter().enumerate() {
                            self.phases[k] = (self.phases[k] + inc * mult).fract();
                            s += 2.0 * self.phases[k] - 1.0;
                        }
                        s / 3.0
                    }
                }
            };
            let mut y = 0.0;
            for b in 0..n_b {
                let mb = { let t = self.mod_bp[b][0].process(m); self.mod_bp[b][1].process(t) };
                let e = self.env[b].next(mb);
                let cb = { let t = self.car_bp[b][0].process(carrier); self.car_bp[b][1].process(t) };
                y += cb * e;
            }
            let y = (y * makeup * 4.0).clamp(-2.0, 2.0);
            left[n] = mix(left[n], y, self.p.dry_wet);
            right[n] = mix(right[n], y, self.p.dry_wet);
        }
        self.key_len = 0;
    }
    fn params(&self) -> &'static [ParamInfo] {
        VOC_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.carrier_hz = v,
            1 => {
                self.p.release_ms = v;
                self.update();
            }
            2 => self.p.dry_wet = v,
            _ => {}
        }
    }
    fn sidechain(&mut self, l: &[f32], r: &[f32]) {
        let n = l.len().min(MAX_BLOCK);
        for i in 0..n {
            self.key[i] = (l[i] + r[i]) * 0.5;
        }
        self.key_len = n;
    }
    fn wants_sidechain(&self) -> Option<usize> {
        if self.p.carrier == VocoderCarrier::External { self.p.sidechain_track } else { None }
    }
    fn name(&self) -> &str {
        "Vocoder"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rms;

    fn ctx(beat: f64) -> ProcessContext {
        ProcessContext { sample_rate: 48_000.0, tempo: 120.0, beat, playing: true }
    }

    #[test]
    fn beat_repeat_stutters_the_captured_slice() {
        let sr = 48_000.0;
        let p = BeatRepeatParams { interval: 4.0, offset: 0.0, grid: 0.25, gate: 1.0, chance: 1.0, decay: 0.0, insert: true, volume_db: 0.0 };
        let mut br = BeatRepeat::new(p, sr);
        // A ramp makes it easy to see repeats: output should restart every 6000 frames.
        let n = 24_000; // one beat at 120 bpm
        let mut l: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
        let mut r = l.clone();
        br.process(&ctx(0.0), &mut l, &mut r);
        let grid = 6_000;
        assert!((l[grid + 10] - l[10]).abs() < 1e-6);
        assert!((l[3 * grid + 100] - l[100]).abs() < 1e-6);
    }

    #[test]
    fn grain_delay_shifts_pitch() {
        let sr = 48_000.0;
        let mut gd = GrainDelay::new(GrainDelayParams { time_beats: 0.25, feedback: 0.0, pitch_semitones: 12.0, spray: 0.0, grain_ms: 50.0, dry_wet: 1.0 }, sr);
        let mut l: Vec<f32> = (0..96_000).map(|i| (std::f32::consts::TAU * 220.0 * i as f32 / sr).sin() * 0.5).collect();
        let mut r = l.clone();
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            gd.process(&ctx(0.0), a, b);
        }
        let body = &l[48_000..];
        let z = body.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32 / (body.len() as f32 / sr);
        assert!((z - 440.0).abs() < 30.0, "pitch {z}");
    }

    #[test]
    fn vocoder_follows_modulator() {
        let sr = 48_000.0;
        let mut v = Vocoder::new(VocoderParams::default(), sr);
        // Silence in, silence out; signal in, carrier out.
        let (mut l, mut r) = (vec![0.0; 9_600], vec![0.0; 9_600]);
        v.process(&ctx(0.0), &mut l, &mut r);
        assert!(rms(&l) < 1e-4);
        let mut noise = Noise::with_seed(3);
        let (mut l, mut r): (Vec<f32>, Vec<f32>) = (0..24_000).map(|_| (noise.next() * 0.3, 0.0)).unzip();
        v.process(&ctx(0.0), &mut l, &mut r);
        assert!(rms(&l[12_000..]) > 0.01, "{}", rms(&l[12_000..]));
        assert!(l.iter().all(|x| x.is_finite()));
    }
}
