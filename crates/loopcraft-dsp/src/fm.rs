//! Four-operator FM synthesizer.

use crate::util::{Envelope, midi_to_hz};
use crate::{Instrument, ParamInfo, ProcessContext};
use loopcraft_core::*;

/// For each algorithm: `mods[i]` lists operators that modulate operator `i`
/// (always lower indices), and `carriers` are summed to the output.
pub struct Algorithm {
    pub mods: [&'static [usize]; 4],
    pub carriers: &'static [usize],
    pub name: &'static str,
}

pub const FM_ALGORITHMS: [Algorithm; 8] = [
    Algorithm { mods: [&[], &[0], &[1], &[2]], carriers: &[3], name: "A>B>C>D" },
    Algorithm { mods: [&[], &[], &[0, 1], &[2]], carriers: &[3], name: "(A+B)>C>D" },
    Algorithm { mods: [&[], &[0], &[], &[1, 2]], carriers: &[3], name: "(A>B + C)>D" },
    Algorithm { mods: [&[], &[0], &[], &[2]], carriers: &[1, 3], name: "A>B, C>D" },
    Algorithm { mods: [&[], &[], &[], &[0, 1, 2]], carriers: &[3], name: "(A+B+C)>D" },
    Algorithm { mods: [&[], &[0], &[0], &[0]], carriers: &[1, 2, 3], name: "A>(B,C,D)" },
    Algorithm { mods: [&[], &[0], &[], &[]], carriers: &[1, 2, 3], name: "A>B, C, D" },
    Algorithm { mods: [&[], &[], &[], &[]], carriers: &[0, 1, 2, 3], name: "A+B+C+D" },
];

#[derive(Clone)]
struct Voice {
    note: u8,
    vel: f32,
    freq: f32,
    phase: [f32; 4],
    out: [f32; 4],
    fb: [f32; 2],
    env: [Envelope; 4],
    lp: f32,
    age: u64,
    held: bool,
}

impl Voice {
    fn active(&self) -> bool {
        // Active while any carrier envelope is still sounding.
        self.env.iter().any(|e| e.is_active())
    }
}

pub(crate) const PARAMS: &[ParamInfo] = &[
    ParamInfo::new("A Level", 0.0, 1.0, "").at("ops/0/level"),
    ParamInfo::new("B Level", 0.0, 1.0, "").at("ops/1/level"),
    ParamInfo::new("C Level", 0.0, 1.0, "").at("ops/2/level"),
    ParamInfo::new("D Level", 0.0, 1.0, "").at("ops/3/level"),
    ParamInfo::new("A Ratio", 0.25, 16.0, "").at("ops/0/ratio"),
    ParamInfo::new("B Ratio", 0.25, 16.0, "").at("ops/1/ratio"),
    ParamInfo::new("C Ratio", 0.25, 16.0, "").at("ops/2/ratio"),
    ParamInfo::new("D Ratio", 0.25, 16.0, "").at("ops/3/ratio"),
    ParamInfo::new("Feedback", 0.0, 1.0, "").at("feedback"),
    ParamInfo::new("Filter", 100.0, 20_000.0, "Hz").at("filter_cutoff_hz"),
    ParamInfo::new("Volume", -40.0, 6.0, "dB").at("gain_db"),
];

pub struct FmSynth {
    p: FmParams,
    sr: f32,
    voices: Vec<Voice>,
    clock: u64,
    gain: f32,
}

impl FmSynth {
    pub fn new(p: FmParams, sr: f32) -> Self {
        let v = Voice {
            note: 0,
            vel: 0.0,
            freq: 440.0,
            phase: [0.0; 4],
            out: [0.0; 4],
            fb: [0.0; 2],
            env: [Envelope::new(p.ops[0].env, sr), Envelope::new(p.ops[1].env, sr), Envelope::new(p.ops[2].env, sr), Envelope::new(p.ops[3].env, sr)],
            lp: 0.0,
            age: 0,
            held: false,
        };
        Self { voices: vec![v; p.voices.clamp(1, 32)], gain: db_to_gain(p.gain_db), p, sr, clock: 0 }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active()).count()
    }
}

impl Instrument for FmSynth {
    fn note_on(&mut self, note: u8, velocity: u8) {
        let i = self
            .voices
            .iter()
            .position(|v| v.active() && v.note == note)
            .or_else(|| self.voices.iter().position(|v| !v.active()))
            .unwrap_or_else(|| self.voices.iter().enumerate().min_by_key(|(_, v)| v.age).map(|(i, _)| i).unwrap_or(0));
        self.clock += 1;
        let ops = self.p.ops;
        let v = &mut self.voices[i];
        if !v.active() {
            v.phase = [0.0; 4];
            v.fb = [0.0; 2];
            v.lp = 0.0;
        }
        v.note = note;
        v.vel = velocity as f32 / 127.0;
        v.freq = midi_to_hz(note as f32);
        v.age = self.clock;
        v.held = true;
        for (k, e) in v.env.iter_mut().enumerate() {
            e.set_params(ops[k].env);
            e.gate_on();
        }
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.note == note && v.held) {
            v.held = false;
            v.env.iter_mut().for_each(Envelope::gate_off);
        }
    }

    fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut() {
            v.held = false;
            v.env.iter_mut().for_each(Envelope::kill);
        }
    }

    fn render(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let alg = &FM_ALGORITHMS[self.p.algorithm.min(FM_ALGORITHMS.len() - 1)];
        let sr = self.sr;
        let ops = self.p.ops;
        let fb_amt = self.p.feedback.clamp(0.0, 1.0) * 0.5;
        let lp_c = (-std::f32::consts::TAU * self.p.filter_cutoff_hz.min(sr * 0.45) / sr).exp();
        let carrier_norm = 1.0 / (alg.carriers.len() as f32).sqrt();
        let mut is_carrier = [false; 4];
        for &c in alg.carriers {
            is_carrier[c] = true;
        }
        for v in self.voices.iter_mut().filter(|v| v.active()) {
            let mut inc = [0.0f32; 4];
            for k in 0..4 {
                let f = ops[k].fixed_hz.unwrap_or(v.freq * ops[k].ratio) * 2f32.powf(ops[k].detune_cents / 1200.0);
                inc[k] = f / sr;
            }
            for n in 0..left.len() {
                let mut out = 0.0;
                for k in 0..4 {
                    let env = v.env[k].next();
                    let mut m = 0.0;
                    for &j in alg.mods[k] {
                        m += v.out[j];
                    }
                    if k == 0 && fb_amt > 0.0 {
                        m += (v.fb[0] + v.fb[1]) * 0.5 * fb_amt;
                    }
                    let level = ops[k].level.clamp(0.0, 1.0);
                    // Velocity brightens modulators and scales carriers.
                    let vel = if is_carrier[k] { v.vel } else { 0.5 + 0.5 * v.vel };
                    let y = ((v.phase[k] + m) * std::f32::consts::TAU).sin() * env * level * vel;
                    v.phase[k] = (v.phase[k] + inc[k]).fract();
                    v.out[k] = y;
                    if k == 0 {
                        v.fb[1] = v.fb[0];
                        v.fb[0] = y;
                    }
                    if is_carrier[k] {
                        out += y;
                    }
                }
                v.lp = out + (v.lp - out) * lp_c;
                let s = v.lp * carrier_norm * self.gain;
                left[n] += s;
                right[n] += s;
                if !v.active() {
                    break;
                }
            }
        }
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0..=3 => self.p.ops[i].level = v,
            4..=7 => self.p.ops[i - 4].ratio = v,
            8 => self.p.feedback = v,
            9 => self.p.filter_cutoff_hz = v,
            10 => {
                self.p.gain_db = v;
                self.gain = db_to_gain(v);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "FM Synth"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rms;

    #[test]
    fn every_algorithm_plays_and_releases() {
        for a in 0..FM_ALGORITHMS.len() {
            let mut s = FmSynth::new(FmParams { algorithm: a, feedback: 0.6, ..Default::default() }, 48_000.0);
            s.note_on(60, 120);
            let (mut l, mut r) = (vec![0.0; 9_600], vec![0.0; 9_600]);
            s.render(&ProcessContext::new(48_000.0, 120.0), &mut l, &mut r);
            assert!(rms(&l) > 0.01 && l.iter().all(|x| x.is_finite() && x.abs() < 4.0), "algorithm {a}");
            s.note_off(60);
            let (mut l, mut r) = (vec![0.0; 48_000], vec![0.0; 48_000]);
            s.render(&ProcessContext::new(48_000.0, 120.0), &mut l, &mut r);
            assert_eq!(s.active_voices(), 0, "algorithm {a} never released");
        }
    }

    #[test]
    fn modulation_adds_harmonics() {
        let measure = |level: f32| {
            let mut p = FmParams::default();
            p.ops[0].level = level;
            let mut s = FmSynth::new(p, 48_000.0);
            s.note_on(48, 127);
            let (mut l, mut r) = (vec![0.0; 4_800], vec![0.0; 4_800]);
            s.render(&ProcessContext::new(48_000.0, 120.0), &mut l, &mut r);
            // High-frequency content: energy of the first difference.
            let d: Vec<f32> = l.windows(2).map(|w| w[1] - w[0]).collect();
            rms(&d) / rms(&l).max(1e-9)
        };
        assert!(measure(0.8) > 2.0 * measure(0.0));
    }
}
