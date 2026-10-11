//! Sampler: plays one audio file chromatically across the keyboard.

use crate::util::{Envelope, midi_to_hz};
use crate::{AudioBuffer, Instrument, ParamInfo, ProcessContext};
use loopcraft_core::{SamplerParams, db_to_gain};
use std::sync::Arc;

pub(crate) const PARAMS: &[ParamInfo] =
    &[ParamInfo::new("Volume", -40.0, 6.0, "dB"), ParamInfo::new("Attack", 0.001, 2.0, "s"), ParamInfo::new("Release", 0.005, 4.0, "s")];

#[derive(Clone)]
struct Voice {
    note: u8,
    pos: f64,
    rate: f64,
    velocity: f32,
    env: Envelope,
    held: bool,
}

pub struct Sampler {
    p: SamplerParams,
    sample: Option<Arc<AudioBuffer>>,
    voices: Vec<Voice>,
    next: usize,
    gain: f32,
}

impl Sampler {
    pub fn new(p: SamplerParams, sample: Option<Arc<AudioBuffer>>, sample_rate: f32) -> Self {
        let voice = Voice { note: 0, pos: 0.0, rate: 1.0, velocity: 0.0, env: Envelope::new(p.amp_env, sample_rate), held: false };
        Self { gain: db_to_gain(p.gain_db), voices: vec![voice; 16], next: 0, sample, p }
    }

    pub fn has_sample(&self) -> bool {
        self.sample.is_some()
    }
}

impl Instrument for Sampler {
    fn note_on(&mut self, note: u8, velocity: u8) {
        let i = self.voices.iter().position(|v| !v.env.is_active()).unwrap_or_else(|| {
            self.next = (self.next + 1) % self.voices.len();
            self.next
        });
        let ratio = midi_to_hz(note as f32) / midi_to_hz(self.p.root_key as f32);
        let v = &mut self.voices[i];
        v.note = note;
        v.pos = 0.0;
        v.rate = ratio as f64;
        v.velocity = velocity as f32 / 127.0;
        v.held = true;
        v.env.set_params(self.p.amp_env);
        v.env.gate_on();
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.note == note && v.held) {
            v.held = false;
            v.env.gate_off();
        }
    }

    fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut() {
            v.held = false;
            v.env.kill();
        }
    }

    fn render(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let Some(buf) = self.sample.as_ref() else { return };
        let len = buf.len() as f64;
        if len < 2.0 {
            return;
        }
        let stereo = buf.channels.len() > 1;
        for v in self.voices.iter_mut().filter(|v| v.env.is_active()) {
            for n in 0..left.len() {
                if v.pos >= len - 1.0 {
                    if self.p.loop_sample {
                        v.pos %= len - 1.0;
                    } else {
                        v.env.kill();
                    }
                }
                let a = v.env.next() * v.velocity * self.gain;
                let l = buf.read(0, v.pos);
                let r = if stereo { buf.read(1, v.pos) } else { l };
                left[n] += l * a;
                right[n] += r * a;
                v.pos += v.rate;
                if !v.env.is_active() {
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
            0 => {
                self.p.gain_db = value;
                self.gain = db_to_gain(value);
            }
            1 => self.p.amp_env.attack = value,
            2 => self.p.amp_env.release = value,
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Sampler"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octave_up_plays_twice_as_fast() {
        let sr = 1000.0;
        let ramp: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let buf = Arc::new(AudioBuffer { sample_rate: sr, channels: vec![ramp] });
        let mut s = Sampler::new(SamplerParams::default(), Some(buf), sr);
        s.note_on(72, 127);
        let ctx = ProcessContext::new(sr, 120.0);
        let (mut l, mut r) = (vec![0.0; 100], vec![0.0; 100]);
        s.render(&ctx, &mut l, &mut r);
        // After 100 output samples at 2x rate we've read ~200 input samples.
        let expected = 0.198;
        assert!((l[99] - expected).abs() < 0.02, "{}", l[99]);
    }
}
