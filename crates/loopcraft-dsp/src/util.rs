//! Building blocks shared by instruments and effects.

use loopcraft_core::{Adsr, FilterMode};
use std::f32::consts::PI;

pub fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// Equal-power pan law. Returns (left, right) gains for pan in -1..1.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let p = (pan.clamp(-1.0, 1.0) + 1.0) * 0.25 * PI;
    (p.cos(), p.sin())
}

/// Cheap deterministic white noise (xorshift32).
#[derive(Clone)]
pub struct Noise(u32);

impl Default for Noise {
    fn default() -> Self {
        Noise(0x1234_5678)
    }
}

impl Noise {
    pub fn with_seed(seed: u32) -> Self {
        Noise(seed.max(1))
    }
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// Uniform in -1..1.
    #[inline]
    pub fn next(&mut self) -> f32 {
        (self.next_u32() as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
    /// Uniform in 0..1.
    #[inline]
    pub fn unit(&mut self) -> f32 {
        self.next_u32() as f32 / u32::MAX as f32
    }
}

/// One-pole parameter smoother to avoid zipper noise.
#[derive(Clone, Copy)]
pub struct Smoothed {
    pub current: f32,
    pub target: f32,
    coeff: f32,
}

impl Smoothed {
    pub fn new(value: f32, sample_rate: f32, time_ms: f32) -> Self {
        let coeff = (-1.0 / (time_ms.max(0.01) * 0.001 * sample_rate)).exp();
        Self { current: value, target: value, coeff }
    }
    #[inline]
    pub fn set(&mut self, v: f32) {
        self.target = v;
    }
    #[inline]
    pub fn next(&mut self) -> f32 {
        self.current = self.target + (self.current - self.target) * self.coeff;
        self.current
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Linear-attack, exponential decay/release ADSR envelope.
#[derive(Clone, Copy)]
pub struct Envelope {
    stage: Stage,
    level: f32,
    params: Adsr,
    sr: f32,
    attack_inc: f32,
    decay_coeff: f32,
    release_coeff: f32,
}

impl Envelope {
    pub fn new(params: Adsr, sample_rate: f32) -> Self {
        let mut e = Self { stage: Stage::Idle, level: 0.0, params, sr: sample_rate, attack_inc: 0.0, decay_coeff: 0.0, release_coeff: 0.0 };
        e.set_params(params);
        e
    }

    pub fn set_params(&mut self, p: Adsr) {
        self.params = p;
        self.attack_inc = 1.0 / (p.attack.max(0.0005) * self.sr);
        // Time constant chosen so the stage is ~99% complete after the given time.
        self.decay_coeff = (-4.6 / (p.decay.max(0.001) * self.sr)).exp();
        self.release_coeff = (-4.6 / (p.release.max(0.001) * self.sr)).exp();
    }

    pub fn gate_on(&mut self) {
        self.stage = Stage::Attack;
    }

    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    /// Fast fade used when a voice is stolen or a drum pad is choked.
    pub fn kill(&mut self) {
        self.stage = Stage::Release;
        self.release_coeff = (-4.6 / (0.003 * self.sr)).exp();
    }

    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    pub fn is_releasing(&self) -> bool {
        self.stage == Stage::Release
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += self.attack_inc;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                let s = self.params.sustain;
                self.level = s + (self.level - s) * self.decay_coeff;
                if (self.level - s).abs() < 1e-4 {
                    self.level = s;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = self.params.sustain,
            Stage::Release => {
                self.level *= self.release_coeff;
                if self.level < 1e-5 {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                    self.set_params(self.params); // restore after kill()
                }
            }
        }
        self.level
    }
}

/// Topology-preserving-transform state-variable filter (Zavalishin / Simper
/// formulation). Stable under fast cutoff modulation, which matters for
/// synth filter envelopes.
#[derive(Clone, Copy, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
    g: f32,
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl Svf {
    pub fn set(&mut self, cutoff_hz: f32, resonance: f32, sample_rate: f32) {
        let fc = cutoff_hz.clamp(10.0, sample_rate * 0.45);
        self.g = (PI * fc / sample_rate).tan();
        // resonance 0..1 -> Q 0.5..~20
        let q = 0.5 + resonance.clamp(0.0, 0.99).powi(2) * 19.5;
        self.k = 1.0 / q;
        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        self.a2 = self.g * self.a1;
        self.a3 = self.g * self.a2;
    }

    #[inline]
    pub fn process(&mut self, x: f32, mode: FilterMode) -> f32 {
        let v3 = x - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        match mode {
            FilterMode::LowPass => v2,
            FilterMode::BandPass => v1,
            FilterMode::HighPass => x - self.k * v1 - v2,
        }
    }

    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }
}

/// Direct-form-I biquad with RBJ cookbook coefficient design.
#[derive(Clone, Copy)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BiquadKind {
    LowPass,
    HighPass,
    /// Constant 0 dB peak-gain bandpass.
    BandPass,
    Peak,
    LowShelf,
    HighShelf,
    Notch,
}

impl Biquad {
    pub fn design(&mut self, kind: BiquadKind, freq: f32, q: f32, gain_db: f32, sr: f32) {
        let f = freq.clamp(10.0, sr * 0.49);
        let w0 = 2.0 * PI * f / sr;
        let (sw, cw) = w0.sin_cos();
        let q = q.max(0.05);
        let alpha = sw / (2.0 * q);
        let a = 10f32.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BiquadKind::LowPass => ((1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            BiquadKind::HighPass => ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            BiquadKind::Notch => (1.0, -2.0 * cw, 1.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            BiquadKind::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
            BiquadKind::Peak => (1.0 + alpha * a, -2.0 * cw, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cw, 1.0 - alpha / a),
            BiquadKind::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cw + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
                    a * ((a + 1.0) - (a - 1.0) * cw - s),
                    (a + 1.0) + (a - 1.0) * cw + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cw),
                    (a + 1.0) + (a - 1.0) * cw - s,
                )
            }
            BiquadKind::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cw + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
                    a * ((a + 1.0) + (a - 1.0) * cw - s),
                    (a + 1.0) - (a - 1.0) * cw + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cw),
                    (a + 1.0) - (a - 1.0) * cw - s,
                )
            }
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        // Flush denormals so long silent tails don't spike CPU.
        self.y1 = if y.abs() < 1e-20 { 0.0 } else { y };
        self.y1
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    /// Magnitude response at `freq`, for drawing EQ curves.
    pub fn magnitude(&self, freq: f32, sr: f32) -> f32 {
        let w = 2.0 * PI * freq / sr;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

/// Fractional delay line with linear interpolation.
#[derive(Clone)]
pub struct DelayLine {
    buf: Vec<f32>,
    pos: usize,
}

impl DelayLine {
    pub fn new(max_samples: usize) -> Self {
        Self { buf: vec![0.0; max_samples.max(2)], pos: 0 }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    #[inline]
    pub fn write(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.pos = (self.pos + 1) % self.buf.len();
    }

    /// Read `delay` samples behind the most recent write.
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        let n = self.buf.len();
        let d = delay.clamp(1.0, (n - 1) as f32);
        let di = d.floor() as usize;
        let frac = d - di as f32;
        let i0 = (self.pos + n - di) % n;
        let i1 = (i0 + n - 1) % n;
        self.buf[i0] * (1.0 - frac) + self.buf[i1] * frac
    }

    pub fn clear(&mut self) {
        self.buf.iter_mut().for_each(|s| *s = 0.0);
    }
}

/// Evaluate an LFO shape at phase 0..1, returning -1..1.
#[inline]
pub fn lfo_shape(shape: loopcraft_core::LfoShape, phase: f64) -> f32 {
    use loopcraft_core::LfoShape::*;
    let p = phase.rem_euclid(1.0) as f32;
    match shape {
        Sine => (p * std::f32::consts::TAU).sin(),
        Triangle => 1.0 - 4.0 * (p - 0.5).abs(),
        Square => {
            if p < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        SawUp => 2.0 * p - 1.0,
        SawDown => 1.0 - 2.0 * p,
    }
}

/// Free-running sine LFO in Hz.
#[derive(Clone, Copy)]
pub struct Lfo {
    phase: f64,
    inc: f64,
}

impl Lfo {
    pub fn new(rate_hz: f32, sample_rate: f32, phase: f64) -> Self {
        Self { phase, inc: rate_hz as f64 / sample_rate as f64 }
    }
    pub fn set_rate(&mut self, rate_hz: f32, sample_rate: f32) {
        self.inc = rate_hz as f64 / sample_rate as f64;
    }
    #[inline]
    pub fn next(&mut self) -> f32 {
        let v = (self.phase * std::f64::consts::TAU).sin() as f32;
        self.phase = (self.phase + self.inc).fract();
        v
    }
    pub fn phase(&self) -> f64 {
        self.phase
    }
    pub fn set_phase(&mut self, p: f64) {
        self.phase = p.rem_euclid(1.0);
    }
}

/// Peak envelope follower with separate attack and release, in linear units.
#[derive(Clone, Copy)]
pub struct EnvFollower {
    env: f32,
    att: f32,
    rel: f32,
}

impl EnvFollower {
    pub fn new(attack_ms: f32, release_ms: f32, sample_rate: f32) -> Self {
        let mut e = Self { env: 0.0, att: 0.0, rel: 0.0 };
        e.set(attack_ms, release_ms, sample_rate);
        e
    }
    pub fn set(&mut self, attack_ms: f32, release_ms: f32, sample_rate: f32) {
        self.att = (-1.0 / (attack_ms.max(0.01) * 0.001 * sample_rate)).exp();
        self.rel = (-1.0 / (release_ms.max(0.1) * 0.001 * sample_rate)).exp();
    }
    #[inline]
    pub fn next(&mut self, x: f32) -> f32 {
        let a = x.abs();
        let c = if a > self.env { self.att } else { self.rel };
        self.env = a + (self.env - a) * c;
        if self.env < 1e-12 {
            self.env = 0.0;
        }
        self.env
    }
    pub fn value(&self) -> f32 {
        self.env
    }
    pub fn reset(&mut self) {
        self.env = 0.0;
    }
}

/// Fourth-order Linkwitz-Riley crossover (two cascaded Butterworth biquads
/// per side). Low + high sum to an allpass with flat magnitude.
#[derive(Clone, Copy, Default)]
pub struct Crossover {
    lp: [Biquad; 2],
    hp: [Biquad; 2],
}

impl Crossover {
    pub fn new(freq: f32, sr: f32) -> Self {
        let mut c = Self::default();
        c.set(freq, sr);
        c
    }
    pub fn set(&mut self, freq: f32, sr: f32) {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        for b in self.lp.iter_mut() {
            b.design(BiquadKind::LowPass, freq, q, 0.0, sr);
        }
        for b in self.hp.iter_mut() {
            b.design(BiquadKind::HighPass, freq, q, 0.0, sr);
        }
    }
    /// Returns (low, high).
    #[inline]
    pub fn split(&mut self, x: f32) -> (f32, f32) {
        let l = { let t = self.lp[0].process(x); self.lp[1].process(t) };
        let h = { let t = self.hp[0].process(x); self.hp[1].process(t) };
        (l, h)
    }
    /// Pass through as the allpass sum, for phase-aligning other bands.
    #[inline]
    pub fn allpass(&mut self, x: f32) -> f32 {
        let (l, h) = self.split(x);
        l + h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_reaches_sustain_and_releases() {
        let mut e = Envelope::new(Adsr { attack: 0.01, decay: 0.01, sustain: 0.5, release: 0.01 }, 1000.0);
        e.gate_on();
        for _ in 0..200 {
            e.next();
        }
        assert!((e.level() - 0.5).abs() < 1e-3);
        e.gate_off();
        for _ in 0..200 {
            e.next();
        }
        assert!(!e.is_active());
    }

    #[test]
    fn lowpass_attenuates_highs() {
        let sr = 48_000.0;
        let mut b = Biquad::default();
        b.design(BiquadKind::LowPass, 1_000.0, 0.707, 0.0, sr);
        assert!((b.magnitude(100.0, sr) - 1.0).abs() < 0.05);
        assert!(b.magnitude(10_000.0, sr) < 0.02);
    }

    #[test]
    fn peak_boosts_at_center() {
        let sr = 48_000.0;
        let mut b = Biquad::default();
        b.design(BiquadKind::Peak, 1_000.0, 1.0, 6.0, sr);
        let db = 20.0 * b.magnitude(1_000.0, sr).log10();
        assert!((db - 6.0).abs() < 0.1, "{db}");
    }

    #[test]
    fn crossover_sums_flat() {
        let sr = 48_000.0;
        // Feed a sine at the crossover frequency; low+high must keep its level.
        let mut c = Crossover::new(1_000.0, sr);
        let mut out = Vec::new();
        for i in 0..48_000 {
            let x = (std::f32::consts::TAU * 1_000.0 * i as f32 / sr).sin();
            let (l, h) = c.split(x);
            out.push(l + h);
        }
        let r = crate::rms(&out[4_800..]);
        assert!((r - 0.707).abs() < 0.02, "{r}");
    }

    #[test]
    fn pan_is_equal_power() {
        let (l, r) = pan_gains(0.0);
        assert!((l * l + r * r - 1.0).abs() < 1e-5);
        let (l, r) = pan_gains(-1.0);
        assert!(l > 0.99 && r < 0.01);
    }
}
