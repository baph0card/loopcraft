//! Poly Synth: a two-oscillator subtractive synthesizer.
//!
//! Signal path per voice: osc1 + osc2 (+ sub) -> state-variable filter
//! (with its own envelope) -> amp envelope. Saw and square use PolyBLEP
//! correction to keep aliasing down without oversampling.

use crate::util::{Envelope, Noise, Svf, midi_to_hz};
use crate::{Instrument, ParamInfo, ProcessContext};
use loopcraft_core::{SynthParams, Waveform, db_to_gain};

pub(crate) const PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Cutoff", 20.0, 18_000.0, "Hz"),
    ParamInfo::new("Resonance", 0.0, 0.95, ""),
    ParamInfo::new("Filter Env", -4.0, 6.0, "oct"),
    ParamInfo::new("Osc Mix", 0.0, 1.0, ""),
    ParamInfo::new("Volume", -40.0, 6.0, "dB"),
    ParamInfo::new("Attack", 0.001, 2.0, "s"),
    ParamInfo::new("Release", 0.005, 4.0, "s"),
];

#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

#[inline]
fn osc(wave: Waveform, phase: f32, dt: f32, noise: &mut Noise) -> f32 {
    match wave {
        Waveform::Sine => (phase * std::f32::consts::TAU).sin(),
        Waveform::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
        Waveform::Saw => (2.0 * phase - 1.0) - poly_blep(phase, dt),
        Waveform::Square => {
            let mut v = if phase < 0.5 { 1.0 } else { -1.0 };
            v += poly_blep(phase, dt);
            v -= poly_blep((phase + 0.5) % 1.0, dt);
            v
        }
        Waveform::Noise => noise.next(),
    }
}

#[derive(Clone)]
struct Voice {
    note: u8,
    velocity: f32,
    freq: f32,
    target_freq: f32,
    phase1: f32,
    phase2: f32,
    phase_sub: f32,
    filter_l: Svf,
    amp_env: Envelope,
    filt_env: Envelope,
    age: u64,
    held: bool,
}

pub struct PolySynth {
    p: SynthParams,
    sr: f32,
    voices: Vec<Voice>,
    clock: u64,
    noise: Noise,
    gain: f32,
    last_freq: f32,
}

impl PolySynth {
    pub fn new(p: SynthParams, sample_rate: f32) -> Self {
        let n = p.voices.clamp(1, 64);
        let voice = Voice {
            note: 0,
            velocity: 0.0,
            freq: 440.0,
            target_freq: 440.0,
            phase1: 0.0,
            phase2: 0.0,
            phase_sub: 0.0,
            filter_l: Svf::default(),
            amp_env: Envelope::new(p.amp_env, sample_rate),
            filt_env: Envelope::new(p.filter_env, sample_rate),
            age: 0,
            held: false,
        };
        Self { gain: db_to_gain(p.gain_db), voices: vec![voice; n], p, sr: sample_rate, clock: 0, noise: Noise::default(), last_freq: 0.0 }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.amp_env.is_active()).count()
    }

    fn pick_voice(&mut self, note: u8) -> usize {
        // Retrigger the same note if it is still sounding.
        if let Some(i) = self.voices.iter().position(|v| v.amp_env.is_active() && v.note == note) {
            return i;
        }
        if let Some(i) = self.voices.iter().position(|v| !v.amp_env.is_active()) {
            return i;
        }
        // Steal: prefer the oldest releasing voice, then the oldest overall.
        let releasing = self.voices.iter().enumerate().filter(|(_, v)| v.amp_env.is_releasing()).min_by_key(|(_, v)| v.age).map(|(i, _)| i);
        releasing.unwrap_or_else(|| self.voices.iter().enumerate().min_by_key(|(_, v)| v.age).map(|(i, _)| i).unwrap_or(0))
    }
}

impl Instrument for PolySynth {
    fn note_on(&mut self, note: u8, velocity: u8) {
        let i = self.pick_voice(note);
        self.clock += 1;
        let freq = midi_to_hz(note as f32);
        let glide_from = if self.p.glide > 0.0 && self.last_freq > 0.0 { self.last_freq } else { freq };
        let v = &mut self.voices[i];
        let was_active = v.amp_env.is_active();
        v.note = note;
        v.velocity = velocity as f32 / 127.0;
        v.target_freq = freq;
        v.freq = glide_from;
        v.age = self.clock;
        v.held = true;
        if !was_active {
            v.phase1 = 0.0;
            v.phase2 = 0.25;
            v.phase_sub = 0.0;
            v.filter_l.reset();
        }
        v.amp_env.set_params(self.p.amp_env);
        v.filt_env.set_params(self.p.filter_env);
        v.amp_env.gate_on();
        v.filt_env.gate_on();
        self.last_freq = freq;
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.note == note && v.held) {
            v.held = false;
            v.amp_env.gate_off();
            v.filt_env.gate_off();
        }
    }

    fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut() {
            v.held = false;
            v.amp_env.kill();
            v.filt_env.gate_off();
        }
    }

    fn render(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let p = &self.p;
        let sr = self.sr;
        let detune = 2f32.powf((p.osc2_semitones + p.osc2_detune_cents / 100.0) / 12.0);
        let glide_coeff = if p.glide > 0.0 { (-1.0 / (p.glide * sr)).exp() } else { 0.0 };
        let mix2 = p.osc_mix.clamp(0.0, 1.0);
        let mix1 = 1.0 - mix2;
        let noise = &mut self.noise;
        for v in self.voices.iter_mut().filter(|v| v.amp_env.is_active()) {
            for n in 0..left.len() {
                v.freq = v.target_freq + (v.freq - v.target_freq) * glide_coeff;
                let dt1 = v.freq / sr;
                let dt2 = dt1 * detune;
                let mut s = osc(p.osc1, v.phase1, dt1, noise) * mix1 + osc(p.osc2, v.phase2, dt2, noise) * mix2;
                if p.sub_level > 0.0 {
                    s += (v.phase_sub * std::f32::consts::TAU).sin() * p.sub_level;
                }
                v.phase1 = (v.phase1 + dt1) % 1.0;
                v.phase2 = (v.phase2 + dt2) % 1.0;
                v.phase_sub = (v.phase_sub + dt1 * 0.5) % 1.0;

                // Filter envelope modulates cutoff in octaves. Coefficients are
                // updated every 16 samples: cheap and inaudible.
                let fe = v.filt_env.next();
                if n % 16 == 0 {
                    let cutoff = p.cutoff_hz * 2f32.powf(p.filter_env_amount * fe * (0.5 + 0.5 * v.velocity));
                    v.filter_l.set(cutoff, p.resonance, sr);
                }
                let y = v.filter_l.process(s, p.filter_mode);
                let a = v.amp_env.next() * v.velocity * self.gain;
                let out = y * a;
                left[n] += out;
                right[n] += out;
                if !v.amp_env.is_active() {
                    break;
                }
            }
        }
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.p.cutoff_hz = value,
            1 => self.p.resonance = value,
            2 => self.p.filter_env_amount = value,
            3 => self.p.osc_mix = value,
            4 => {
                self.p.gain_db = value;
                self.gain = db_to_gain(value);
            }
            5 => self.p.amp_env.attack = value,
            6 => self.p.amp_env.release = value,
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Poly Synth"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{peak, rms};

    fn render(s: &mut PolySynth, frames: usize) -> Vec<f32> {
        let ctx = ProcessContext::new(48_000.0, 120.0);
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        s.render(&ctx, &mut l, &mut r);
        l
    }

    #[test]
    fn plays_and_releases() {
        let mut s = PolySynth::new(SynthParams::default(), 48_000.0);
        s.note_on(60, 100);
        let on = render(&mut s, 4800);
        assert!(rms(&on) > 0.01);
        assert!(peak(&on) < 1.5);
        s.note_off(60);
        let _ = render(&mut s, 48_000);
        assert_eq!(s.active_voices(), 0);
        assert!(peak(&render(&mut s, 512)) == 0.0);
    }

    #[test]
    fn voice_stealing_caps_polyphony() {
        let mut s = PolySynth::new(SynthParams { voices: 4, ..Default::default() }, 48_000.0);
        for n in 50..60 {
            s.note_on(n, 100);
        }
        assert_eq!(s.active_voices(), 4);
        let out = render(&mut s, 1024);
        assert!(out.iter().all(|x| x.is_finite()));
    }
}
