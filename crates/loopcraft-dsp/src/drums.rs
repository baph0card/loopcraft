//! Drum Rack: notes trigger pads; each pad is a synthesized drum voice or a
//! sample. Pads in the same choke group silence each other.

use crate::util::{Biquad, BiquadKind, Noise, pan_gains};
use crate::{AudioBuffer, Instrument, ParamInfo, ProcessContext, SampleLoader};
use loopcraft_core::{DrumRackParams, DrumVoice, PadSource, db_to_gain};
use std::f32::consts::TAU;
use std::sync::Arc;

pub(crate) const PARAMS: &[ParamInfo] = &[ParamInfo::new("Volume", -40.0, 6.0, "dB")];

/// One-shot synthesized drum voice.
struct DrumSynth {
    kind: DrumVoice,
    t: f32,
    phase: f32,
    phase2: f32,
    noise: Noise,
    bp: Biquad,
    hp: Biquad,
    active: bool,
    choke_gain: f32,
    tune: f32,
}

impl DrumSynth {
    fn new(kind: DrumVoice, sr: f32, tune_semitones: f32, seed: u32) -> Self {
        let mut bp = Biquad::default();
        let mut hp = Biquad::default();
        match kind {
            DrumVoice::Snare => {
                bp.design(BiquadKind::HighPass, 1_500.0, 0.7, 0.0, sr);
            }
            DrumVoice::ClosedHat | DrumVoice::OpenHat => {
                bp.design(BiquadKind::HighPass, 7_000.0, 0.7, 0.0, sr);
                hp.design(BiquadKind::Peak, 10_000.0, 1.0, 4.0, sr);
            }
            DrumVoice::Clap => {
                bp.design(BiquadKind::Peak, 1_200.0, 1.5, 9.0, sr);
                hp.design(BiquadKind::HighPass, 600.0, 0.7, 0.0, sr);
            }
            DrumVoice::Rim => {
                bp.design(BiquadKind::Peak, 1_800.0, 4.0, 10.0, sr);
                hp.design(BiquadKind::HighPass, 400.0, 0.7, 0.0, sr);
            }
            DrumVoice::Cowbell => {
                bp.design(BiquadKind::Peak, 700.0, 2.0, 8.0, sr);
                hp.design(BiquadKind::HighPass, 300.0, 0.7, 0.0, sr);
            }
            _ => {}
        }
        Self {
            kind,
            t: 0.0,
            phase: 0.0,
            phase2: 0.0,
            noise: Noise::with_seed(seed),
            bp,
            hp,
            active: false,
            choke_gain: 1.0,
            tune: 2f32.powf(tune_semitones / 12.0),
        }
    }

    fn trigger(&mut self) {
        self.t = 0.0;
        self.phase = 0.0;
        self.phase2 = 0.0;
        self.active = true;
        self.choke_gain = 1.0;
        self.bp.reset();
        self.hp.reset();
    }

    #[inline]
    fn next(&mut self, sr: f32) -> f32 {
        if !self.active {
            return 0.0;
        }
        let t = self.t;
        let dt = 1.0 / sr;
        self.t += dt;
        let tune = self.tune;
        let exp = |tau: f32| (-t / tau).exp();
        let (s, done) = match self.kind {
            DrumVoice::Kick => {
                let f = (45.0 + 110.0 * exp(0.035)) * tune;
                self.phase = (self.phase + f * dt) % 1.0;
                let body = (self.phase * TAU).sin() * exp(0.32);
                let click = self.noise.next() * exp(0.003) * 0.3;
                (body + click, t > 1.5)
            }
            DrumVoice::Snare => {
                let f = 185.0 * tune;
                self.phase = (self.phase + f * dt) % 1.0;
                let tone = (self.phase * TAU).sin() * exp(0.06) * 0.6;
                let n = self.bp.process(self.noise.next()) * exp(0.13);
                (tone + n * 0.9, t > 1.0)
            }
            DrumVoice::ClosedHat => (self.hp.process(self.bp.process(self.noise.next())) * exp(0.035) * 0.6, t > 0.4),
            DrumVoice::OpenHat => (self.hp.process(self.bp.process(self.noise.next())) * exp(0.28) * 0.5, t > 2.0),
            DrumVoice::Clap => {
                // Three quick bursts then a tail, the classic clap envelope.
                let burst = |start: f32| if t >= start { (-(t - start) / 0.008).exp() } else { 0.0 };
                let env = burst(0.0) + burst(0.011) + burst(0.022) + if t > 0.03 { (-(t - 0.03) / 0.16).exp() * 0.8 } else { 0.0 };
                (self.hp.process(self.bp.process(self.noise.next())) * env * 0.5, t > 1.2)
            }
            DrumVoice::Tom => {
                let f = (90.0 + 50.0 * exp(0.05)) * tune;
                self.phase = (self.phase + f * dt) % 1.0;
                ((self.phase * TAU).sin() * exp(0.28), t > 1.5)
            }
            DrumVoice::Rim => {
                self.phase = (self.phase + 820.0 * tune * dt) % 1.0;
                let tone = (self.phase * TAU).sin() * exp(0.012);
                (self.hp.process(self.bp.process(tone + self.noise.next() * 0.3 * exp(0.006))) * 0.6, t > 0.3)
            }
            DrumVoice::Cowbell => {
                self.phase = (self.phase + 540.0 * tune * dt) % 1.0;
                self.phase2 = (self.phase2 + 800.0 * tune * dt) % 1.0;
                let sq = |p: f32| if p < 0.5 { 1.0 } else { -1.0 };
                let s = (sq(self.phase) + sq(self.phase2)) * 0.25;
                (self.hp.process(self.bp.process(s)) * exp(0.18) * 0.5, t > 1.5)
            }
        };
        if done {
            self.active = false;
        }
        // Headroom: a full-velocity kick plus clap should stay below 0 dBFS.
        s * self.choke_gain * 0.7
    }
}

enum PadVoice {
    Synth(DrumSynth),
    Sample { buf: Option<Arc<AudioBuffer>>, pos: f64, rate: f64, active: bool, choke_gain: f32 },
}

struct Pad {
    note: u8,
    voice: PadVoice,
    gain_l: f32,
    gain_r: f32,
    velocity: f32,
    choke_group: Option<u8>,
    choking: bool,
}

pub struct DrumRack {
    pads: Vec<Pad>,
    sr: f32,
    gain: f32,
}

impl DrumRack {
    pub fn new(p: &DrumRackParams, sample_rate: f32, loader: &dyn SampleLoader) -> Self {
        let pads = p
            .pads
            .iter()
            .enumerate()
            .map(|(i, pad)| {
                let (pl, pr) = pan_gains(pad.pan);
                let g = db_to_gain(pad.gain_db);
                let voice = match &pad.source {
                    PadSource::Synth(kind) => PadVoice::Synth(DrumSynth::new(*kind, sample_rate, pad.tune_semitones, 0x9e37_79b9 ^ (i as u32 * 7919))),
                    PadSource::Sample(path) => {
                        PadVoice::Sample { buf: loader.load(path), pos: 0.0, rate: 2f64.powf(pad.tune_semitones as f64 / 12.0), active: false, choke_gain: 1.0 }
                    }
                };
                Pad {
                    note: pad.note,
                    voice,
                    gain_l: pl * g * std::f32::consts::SQRT_2,
                    gain_r: pr * g * std::f32::consts::SQRT_2,
                    velocity: 0.0,
                    choke_group: pad.choke_group,
                    choking: false,
                }
            })
            .collect();
        Self { pads, sr: sample_rate, gain: db_to_gain(p.gain_db) }
    }
}

impl Instrument for DrumRack {
    fn note_on(&mut self, note: u8, velocity: u8) {
        let group = self.pads.iter().find(|p| p.note == note).and_then(|p| p.choke_group);
        for pad in self.pads.iter_mut() {
            if pad.note == note {
                pad.velocity = velocity as f32 / 127.0;
                pad.choking = false;
                match &mut pad.voice {
                    PadVoice::Synth(s) => s.trigger(),
                    PadVoice::Sample { pos, active, choke_gain, .. } => {
                        *pos = 0.0;
                        *active = true;
                        *choke_gain = 1.0;
                    }
                }
            } else if group.is_some() && pad.choke_group == group {
                pad.choking = true;
            }
        }
    }

    fn note_off(&mut self, _note: u8) {
        // Drum pads are one-shots; note-off is ignored.
    }

    fn all_notes_off(&mut self) {
        for pad in self.pads.iter_mut() {
            pad.choking = true;
        }
    }

    fn render(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sr;
        // ~3 ms fade for choked pads, avoiding clicks.
        let choke_mul = (-1.0 / (0.003 * sr)).exp();
        for pad in self.pads.iter_mut() {
            let gl = pad.gain_l * pad.velocity * self.gain;
            let gr = pad.gain_r * pad.velocity * self.gain;
            match &mut pad.voice {
                PadVoice::Synth(s) => {
                    if !s.active {
                        continue;
                    }
                    for n in 0..left.len() {
                        if pad.choking {
                            s.choke_gain *= choke_mul;
                            if s.choke_gain < 1e-4 {
                                s.active = false;
                            }
                        }
                        let x = s.next(sr);
                        left[n] += x * gl;
                        right[n] += x * gr;
                        if !s.active {
                            break;
                        }
                    }
                }
                PadVoice::Sample { buf, pos, rate, active, choke_gain } => {
                    let Some(b) = buf else { continue };
                    if !*active {
                        continue;
                    }
                    let len = b.len() as f64;
                    let stereo = b.channels.len() > 1;
                    for n in 0..left.len() {
                        if *pos >= len - 1.0 {
                            *active = false;
                            break;
                        }
                        if pad.choking {
                            *choke_gain *= choke_mul;
                            if *choke_gain < 1e-4 {
                                *active = false;
                                break;
                            }
                        }
                        let l = b.read(0, *pos);
                        let r = if stereo { b.read(1, *pos) } else { l };
                        left[n] += l * gl * *choke_gain;
                        right[n] += r * gr * *choke_gain;
                        *pos += *rate;
                    }
                }
            }
        }
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index == 0 {
            self.gain = db_to_gain(value);
        }
    }

    fn name(&self) -> &str {
        "Drum Rack"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NoSamples, peak, rms};

    #[test]
    fn every_default_pad_makes_sound() {
        let params = DrumRackParams::default();
        for pad in &params.pads {
            let mut rack = DrumRack::new(&params, 48_000.0, &NoSamples);
            rack.note_on(pad.note, 127);
            let ctx = ProcessContext::new(48_000.0, 120.0);
            let (mut l, mut r) = (vec![0.0; 4800], vec![0.0; 4800]);
            rack.render(&ctx, &mut l, &mut r);
            assert!(rms(&l) > 0.005, "pad {} silent", pad.note);
            assert!(peak(&l) < 2.0, "pad {} too hot", pad.note);
        }
    }

    #[test]
    fn closed_hat_chokes_open_hat() {
        let params = DrumRackParams::default();
        let mut rack = DrumRack::new(&params, 48_000.0, &NoSamples);
        let ctx = ProcessContext::new(48_000.0, 120.0);
        rack.note_on(46, 127);
        let (mut l, mut r) = (vec![0.0; 2400], vec![0.0; 2400]);
        rack.render(&ctx, &mut l, &mut r);
        rack.note_on(42, 127);
        // Long enough for the closed hat to finish; the open hat must be gone too.
        let (mut l, mut r) = (vec![0.0; 48_000], vec![0.0; 48_000]);
        rack.render(&ctx, &mut l, &mut r);
        let tail = &l[30_000..];
        assert!(peak(tail) < 1e-3, "open hat rang through choke: {}", peak(tail));
    }
}
