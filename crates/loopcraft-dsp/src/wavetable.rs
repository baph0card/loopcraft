//! Wavetable synthesizer with band-limited (mip-mapped) generated tables,
//! unison, a modulation envelope and LFO on table position and cutoff.

use crate::util::{Envelope, Svf, midi_to_hz, pan_gains};
use crate::{Instrument, ParamInfo, ProcessContext};
use loopcraft_core::*;
use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;
use std::sync::{Arc, OnceLock};

pub const TABLE_SIZE: usize = 2048;
pub const FRAMES: usize = 16;
pub const LEVELS: usize = 10;
const MAX_HARM: usize = 512;

/// `FRAMES` positions x `LEVELS` band limits x `TABLE_SIZE` samples.
pub struct WaveTable {
    data: Vec<f32>,
}

impl WaveTable {
    #[inline]
    fn frame(&self, level: usize, frame: usize) -> &[f32] {
        let o = (level * FRAMES + frame) * TABLE_SIZE;
        &self.data[o..o + TABLE_SIZE]
    }

    /// Read at `phase` (0..1) and `position` (0..1), band-limited for `f0`.
    #[inline]
    pub fn read(&self, phase: f32, position: f32, f0: f32, sr: f32) -> f32 {
        let max_h = (sr * 0.5 / f0.max(1.0)).max(1.0);
        let mut level = 0;
        while level < LEVELS - 1 && (MAX_HARM >> level) as f32 > max_h {
            level += 1;
        }
        let fp = position.clamp(0.0, 1.0) * (FRAMES - 1) as f32;
        let f0i = fp as usize;
        let f1i = (f0i + 1).min(FRAMES - 1);
        let fr = fp - f0i as f32;
        let x = phase.rem_euclid(1.0) * TABLE_SIZE as f32;
        let i0 = x as usize % TABLE_SIZE;
        let i1 = (i0 + 1) % TABLE_SIZE;
        let t = x - x.floor();
        let a = self.frame(level, f0i);
        let b = self.frame(level, f1i);
        let va = a[i0] + (a[i1] - a[i0]) * t;
        let vb = b[i0] + (b[i1] - b[i0]) * t;
        va + (vb - va) * fr
    }
}

/// Complex harmonic amplitudes (cosine, sine) for harmonics 0..=MAX_HARM.
fn spectrum(kind: WavetableKind, pos: f32) -> Vec<Complex32> {
    use std::f32::consts::PI;
    let mut s = vec![Complex32::new(0.0, 0.0); MAX_HARM + 1];
    let sine_amp = |s: &mut Vec<Complex32>, k: usize, a: f32| s[k].im += a;
    match kind {
        WavetableKind::Basic => {
            // sine -> triangle -> saw -> square, crossfading neighbours.
            let shapes: [fn(usize) -> f32; 4] = [
                |k| if k == 1 { 1.0 } else { 0.0 },
                |k| if k % 2 == 1 { 8.0 / (PI * PI * (k * k) as f32) * if (k / 2) % 2 == 0 { 1.0 } else { -1.0 } } else { 0.0 },
                |k| 2.0 / (PI * k as f32) * if k % 2 == 1 { 1.0 } else { -1.0 },
                |k| if k % 2 == 1 { 4.0 / (PI * k as f32) } else { 0.0 },
            ];
            let x = pos.clamp(0.0, 1.0) * 3.0;
            let i = (x as usize).min(2);
            let f = x - i as f32;
            for k in 1..=MAX_HARM {
                sine_amp(&mut s, k, shapes[i](k) * (1.0 - f) + shapes[i + 1](k) * f);
            }
        }
        WavetableKind::Harmonics => {
            let top = 1.0 + pos.clamp(0.0, 1.0) * 63.0;
            for k in 1..=MAX_HARM {
                let w = (top - k as f32 + 1.0).clamp(0.0, 1.0);
                sine_amp(&mut s, k, w / k as f32);
            }
        }
        WavetableKind::Pulse => {
            let w = 0.5 - 0.47 * pos.clamp(0.0, 1.0);
            for k in 1..=MAX_HARM {
                s[k].re += 2.0 / (PI * k as f32) * (PI * k as f32 * w).sin();
            }
        }
        WavetableKind::Formant => {
            let f1 = 2.0 + 8.0 * pos;
            let f2 = 6.0 + 22.0 * pos;
            for k in 1..=MAX_HARM {
                let kf = k as f32;
                let g = (-(kf - f1).powi(2) / 4.0).exp() + 0.6 * (-(kf - f2).powi(2) / 12.0).exp();
                sine_amp(&mut s, k, (g + 0.02) / kf.sqrt());
            }
        }
        WavetableKind::Sync => {
            // Hard-synced saw rendered in the time domain, then analysed.
            let ratio = 1.0 + 7.0 * pos.clamp(0.0, 1.0);
            let mut buf: Vec<Complex32> = (0..TABLE_SIZE).map(|i| Complex32::new(2.0 * ((i as f32 / TABLE_SIZE as f32 * ratio).fract()) - 1.0, 0.0)).collect();
            let mut planner = FftPlanner::new();
            planner.plan_fft_forward(TABLE_SIZE).process(&mut buf);
            let n = TABLE_SIZE as f32;
            for k in 1..=MAX_HARM {
                // x(t) = sum (a_k cos + b_k sin); FFT gives (a - i b) * N/2.
                s[k] = Complex32::new(buf[k].re * 2.0 / n, -buf[k].im * 2.0 / n);
            }
        }
    }
    s
}

fn build(kind: WavetableKind) -> WaveTable {
    let mut planner = FftPlanner::new();
    let ifft = planner.plan_fft_inverse(TABLE_SIZE);
    let mut data = vec![0.0f32; LEVELS * FRAMES * TABLE_SIZE];
    for f in 0..FRAMES {
        let spec = spectrum(kind, f as f32 / (FRAMES - 1) as f32);
        let mut norm = 1.0;
        for level in 0..LEVELS {
            let limit = MAX_HARM >> level;
            let mut buf = vec![Complex32::new(0.0, 0.0); TABLE_SIZE];
            for k in 1..=limit {
                // Inverse of the analysis convention above.
                let c = spec[k];
                buf[k] = Complex32::new(c.re * 0.5, -c.im * 0.5);
                buf[TABLE_SIZE - k] = Complex32::new(c.re * 0.5, c.im * 0.5);
            }
            ifft.process(&mut buf);
            let o = (level * FRAMES + f) * TABLE_SIZE;
            if level == 0 {
                let peak = buf.iter().fold(0.0f32, |m, c| m.max(c.re.abs()));
                norm = if peak > 1e-9 { 1.0 / peak } else { 1.0 };
            }
            for (i, c) in buf.iter().enumerate() {
                data[o + i] = c.re * norm;
            }
        }
    }
    WaveTable { data }
}

/// Shared, lazily built tables (each kind is built once per process).
pub fn table(kind: WavetableKind) -> Arc<WaveTable> {
    static TABLES: [OnceLock<Arc<WaveTable>>; 5] = [OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let i = match kind {
        WavetableKind::Basic => 0,
        WavetableKind::Harmonics => 1,
        WavetableKind::Pulse => 2,
        WavetableKind::Formant => 3,
        WavetableKind::Sync => 4,
    };
    TABLES[i].get_or_init(|| Arc::new(build(kind))).clone()
}

const MAX_UNISON: usize = 8;

#[derive(Clone)]
struct Voice {
    note: u8,
    vel: f32,
    freq: f32,
    phases: [[f32; MAX_UNISON]; 2],
    filt: [Svf; 2],
    amp: Envelope,
    modenv: Envelope,
    age: u64,
    held: bool,
}

pub(crate) const PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Position 1", 0.0, 1.0, "").at("position1"),
    ParamInfo::new("Position 2", 0.0, 1.0, "").at("position2"),
    ParamInfo::new("Cutoff", 20.0, 18_000.0, "Hz").at("cutoff_hz"),
    ParamInfo::new("Resonance", 0.0, 0.95, "").at("resonance"),
    ParamInfo::new("Osc Mix", 0.0, 1.0, "").at("osc_mix"),
    ParamInfo::new("Detune", 0.0, 1.0, "").at("unison_detune"),
    ParamInfo::new("Env > Pos", -1.0, 1.0, "").at("env_to_position"),
    ParamInfo::new("LFO > Pos", 0.0, 1.0, "").at("lfo_to_position"),
    ParamInfo::new("Volume", -40.0, 6.0, "dB").at("gain_db"),
];

pub struct Wavetable {
    p: WavetableParams,
    sr: f32,
    t1: Arc<WaveTable>,
    t2: Arc<WaveTable>,
    voices: Vec<Voice>,
    clock: u64,
    lfo_phase: f32,
    gain: f32,
}

impl Wavetable {
    pub fn new(p: WavetableParams, sr: f32) -> Self {
        let v = Voice {
            note: 0,
            vel: 0.0,
            freq: 440.0,
            phases: [[0.0; MAX_UNISON]; 2],
            filt: [Svf::default(); 2],
            amp: Envelope::new(p.amp_env, sr),
            modenv: Envelope::new(p.mod_env, sr),
            age: 0,
            held: false,
        };
        Self { t1: table(p.table1), t2: table(p.table2), voices: vec![v; p.voices.clamp(1, 32)], gain: db_to_gain(p.gain_db), p, sr, clock: 0, lfo_phase: 0.0 }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.amp.is_active()).count()
    }
}

impl Instrument for Wavetable {
    fn note_on(&mut self, note: u8, velocity: u8) {
        let i = self
            .voices
            .iter()
            .position(|v| v.amp.is_active() && v.note == note)
            .or_else(|| self.voices.iter().position(|v| !v.amp.is_active()))
            .unwrap_or_else(|| self.voices.iter().enumerate().min_by_key(|(_, v)| (!v.amp.is_releasing(), v.age)).map(|(i, _)| i).unwrap_or(0));
        self.clock += 1;
        let v = &mut self.voices[i];
        if !v.amp.is_active() {
            // Spread unison start phases so voices don't phase-cancel on attack.
            for o in 0..2 {
                for (u, ph) in v.phases[o].iter_mut().enumerate() {
                    *ph = (u as f32 * 0.618_034).fract();
                }
            }
            v.filt = [Svf::default(); 2];
        }
        v.note = note;
        v.vel = velocity as f32 / 127.0;
        v.freq = midi_to_hz(note as f32);
        v.age = self.clock;
        v.held = true;
        v.amp.set_params(self.p.amp_env);
        v.modenv.set_params(self.p.mod_env);
        v.amp.gate_on();
        v.modenv.gate_on();
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.note == note && v.held) {
            v.held = false;
            v.amp.gate_off();
            v.modenv.gate_off();
        }
    }

    fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut() {
            v.held = false;
            v.amp.kill();
            v.modenv.gate_off();
        }
    }

    fn render(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let p = &self.p;
        let sr = self.sr;
        let uni = p.unison.clamp(1, MAX_UNISON);
        let detune2 = 2f32.powf(p.osc2_semitones / 12.0);
        let lfo_inc = p.lfo_rate_hz / sr;
        let mix2 = p.osc_mix.clamp(0.0, 1.0);
        let uni_norm = 1.0 / (uni as f32).sqrt();
        // Per-unison detune (cents-ish ratio) and pan.
        let mut ratios = [1.0f32; MAX_UNISON];
        let mut pans = [(0.707f32, 0.707f32); MAX_UNISON];
        for u in 0..uni {
            let x = if uni == 1 { 0.0 } else { u as f32 / (uni - 1) as f32 * 2.0 - 1.0 };
            ratios[u] = 2f32.powf(x * p.unison_detune * 0.5 / 12.0);
            pans[u] = pan_gains(x * 0.8);
        }
        let start_lfo = self.lfo_phase;
        for v in self.voices.iter_mut().filter(|v| v.amp.is_active()) {
            let mut lfo_ph = start_lfo;
            for n in 0..left.len() {
                let lfo = (lfo_ph * std::f32::consts::TAU).sin();
                lfo_ph = (lfo_ph + lfo_inc).fract();
                let me = v.modenv.next();
                let pos1 = (p.position1 + p.env_to_position * me + p.lfo_to_position * lfo * 0.5).clamp(0.0, 1.0);
                let pos2 = (p.position2 + p.env_to_position * me + p.lfo_to_position * lfo * 0.5).clamp(0.0, 1.0);
                let (mut sl, mut sr_) = (0.0f32, 0.0f32);
                for u in 0..uni {
                    let f1 = v.freq * ratios[u];
                    let f2 = f1 * detune2;
                    let a = self.t1.read(v.phases[0][u], pos1, f1, sr);
                    let b = self.t2.read(v.phases[1][u], pos2, f2, sr);
                    v.phases[0][u] = (v.phases[0][u] + f1 / sr).fract();
                    v.phases[1][u] = (v.phases[1][u] + f2 / sr).fract();
                    let s = a * (1.0 - mix2) + b * mix2;
                    sl += s * pans[u].0;
                    sr_ += s * pans[u].1;
                }
                if n % 16 == 0 {
                    let cutoff = p.cutoff_hz * 2f32.powf(p.filter_env_amount * me + p.lfo_to_cutoff * lfo);
                    v.filt[0].set(cutoff, p.resonance, sr);
                    v.filt[1].set(cutoff, p.resonance, sr);
                }
                let yl = v.filt[0].process(sl * uni_norm, p.filter_mode);
                let yr = v.filt[1].process(sr_ * uni_norm, p.filter_mode);
                let a = v.amp.next() * v.vel * self.gain;
                left[n] += yl * a;
                right[n] += yr * a;
                if !v.amp.is_active() {
                    break;
                }
            }
        }
        self.lfo_phase = (start_lfo + lfo_inc * left.len() as f32).fract();
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, i: usize, v: f32) {
        let p = &mut self.p;
        match i {
            0 => p.position1 = v,
            1 => p.position2 = v,
            2 => p.cutoff_hz = v,
            3 => p.resonance = v,
            4 => p.osc_mix = v,
            5 => p.unison_detune = v,
            6 => p.env_to_position = v,
            7 => p.lfo_to_position = v,
            8 => {
                p.gain_db = v;
                self.gain = db_to_gain(v);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Wavetable"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rms;

    #[test]
    fn basic_table_morphs_and_is_bandlimited() {
        let t = table(WavetableKind::Basic);
        // Position 0 is a sine: quarter phase = peak.
        assert!((t.read(0.25, 0.0, 100.0, 48_000.0) - 1.0).abs() < 0.02);
        // A very high note must use a near-sine mip level (no harsh edges):
        // check the curve is smooth by comparing neighbouring samples.
        let mut maxd = 0.0f32;
        let mut prev = t.read(0.0, 1.0, 8_000.0, 48_000.0);
        for i in 1..1000 {
            let x = t.read(i as f32 / 1000.0, 1.0, 8_000.0, 48_000.0);
            maxd = maxd.max((x - prev).abs());
            prev = x;
        }
        assert!(maxd < 0.05, "square at 8 kHz should be band-limited, step {maxd}");
    }

    #[test]
    fn all_tables_build_and_play() {
        for kind in [WavetableKind::Basic, WavetableKind::Harmonics, WavetableKind::Pulse, WavetableKind::Formant, WavetableKind::Sync] {
            let mut s = Wavetable::new(WavetableParams { table1: kind, table2: kind, ..Default::default() }, 48_000.0);
            s.note_on(57, 110);
            let (mut l, mut r) = (vec![0.0; 9_600], vec![0.0; 9_600]);
            s.render(&ProcessContext::new(48_000.0, 120.0), &mut l, &mut r);
            assert!(rms(&l) > 0.005 && l.iter().all(|x| x.is_finite()), "{kind:?}");
            s.note_off(57);
            let (mut l, mut r) = (vec![0.0; 48_000], vec![0.0; 48_000]);
            s.render(&ProcessContext::new(48_000.0, 120.0), &mut l, &mut r);
            assert_eq!(s.active_voices(), 0);
        }
    }
}
