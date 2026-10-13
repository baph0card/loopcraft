//! Character effects: Drum Buss and Amp.

use super::mix;
use crate::util::{Biquad, BiquadKind, EnvFollower};
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::*;

const DRUMBUSS_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Drive", 0.0, 1.0, "").at("drive"),
    ParamInfo::new("Crunch", 0.0, 1.0, "").at("crunch"),
    ParamInfo::new("Boom", 0.0, 1.0, "").at("boom"),
    ParamInfo::new("Boom Freq", 30.0, 120.0, "Hz").at("boom_hz"),
    ParamInfo::new("Transients", -1.0, 1.0, "").at("transients"),
    ParamInfo::new("Damping", 500.0, 20_000.0, "Hz").at("damping_hz"),
    ParamInfo::new("Output", -24.0, 12.0, "dB").at("output_db"),
    ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet"),
];

/// Drum bus processor: drive, mid-range crunch, a tuned low resonator
/// ("boom") excited by the low end, transient shaping and damping.
pub struct DrumBuss {
    p: DrumBussParams,
    sr: f32,
    fast: [EnvFollower; 2],
    slow: [EnvFollower; 2],
    boom_in: [Biquad; 2],
    boom_res: [Biquad; 2],
    crunch_bp: [Biquad; 2],
    damp: [Biquad; 2],
}

impl DrumBuss {
    pub fn new(p: DrumBussParams, sr: f32) -> Self {
        let mut d = Self {
            fast: [EnvFollower::new(0.5, 30.0, sr); 2],
            slow: [EnvFollower::new(15.0, 60.0, sr); 2],
            boom_in: [Biquad::default(); 2],
            boom_res: [Biquad::default(); 2],
            crunch_bp: [Biquad::default(); 2],
            damp: [Biquad::default(); 2],
            p,
            sr,
        };
        d.update();
        d
    }
    fn update(&mut self) {
        let q = 2.0 + self.p.boom_decay.clamp(0.0, 1.0) * 28.0;
        for c in 0..2 {
            self.boom_in[c].design(BiquadKind::LowPass, self.p.boom_hz * 1.5, 0.707, 0.0, self.sr);
            self.boom_res[c].design(BiquadKind::BandPass, self.p.boom_hz, q, 0.0, self.sr);
            self.crunch_bp[c].design(BiquadKind::Peak, 2_500.0, 0.8, 9.0, self.sr);
            self.damp[c].design(BiquadKind::LowPass, self.p.damping_hz.min(self.sr * 0.45), 0.707, 0.0, self.sr);
        }
    }
}

impl Effect for DrumBuss {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let drive = 1.0 + self.p.drive.clamp(0.0, 1.0) * 8.0;
        let out = db_to_gain(self.p.output_db);
        for n in 0..left.len() {
            let ins = [left[n], right[n]];
            let mut ys = [0.0f32; 2];
            for c in 0..2 {
                let x = ins[c];
                // Transient shaper: difference between fast and slow envelopes.
                let f = self.fast[c].next(x);
                let s = self.slow[c].next(x);
                let tr = if s > 1e-6 { ((f / s) - 1.0).clamp(-1.0, 4.0) } else { 0.0 };
                let shaped = x * (1.0 + self.p.transients * tr * 0.5).clamp(0.2, 3.0);
                // Drive and crunch.
                let mut y = (shaped * drive).tanh() / drive.tanh();
                if self.p.crunch > 0.0 {
                    let mid = self.crunch_bp[c].process(y) - y;
                    y += (mid * (1.0 + 10.0 * self.p.crunch)).tanh() * self.p.crunch * 0.5;
                }
                y = self.damp[c].process(y);
                // Boom: low resonator excited by the input's low end.
                if self.p.boom > 0.0 {
                    let low = self.boom_in[c].process(x);
                    let ring = self.boom_res[c].process(low);
                    y += ring * self.p.boom * 2.0;
                }
                ys[c] = y * out;
            }
            left[n] = mix(ins[0], ys[0], self.p.dry_wet);
            right[n] = mix(ins[1], ys[1], self.p.dry_wet);
        }
    }
    fn params(&self) -> &'static [ParamInfo] {
        DRUMBUSS_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.drive = v,
            1 => self.p.crunch = v,
            2 => self.p.boom = v,
            3 => self.p.boom_hz = v,
            4 => self.p.transients = v,
            5 => self.p.damping_hz = v,
            6 => self.p.output_db = v,
            7 => self.p.dry_wet = v,
            _ => return,
        }
        self.update();
    }
    fn name(&self) -> &str {
        "Drum Buss"
    }
}

const AMP_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Gain", 0.0, 1.0, "").at("gain"),
    ParamInfo::new("Bass", 0.0, 1.0, "").at("bass"),
    ParamInfo::new("Middle", 0.0, 1.0, "").at("middle"),
    ParamInfo::new("Treble", 0.0, 1.0, "").at("treble"),
    ParamInfo::new("Presence", 0.0, 1.0, "").at("presence"),
    ParamInfo::new("Output", -36.0, 12.0, "dB").at("output_db"),
    ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet"),
];

/// Guitar/bass amp: input gain into an asymmetric tube-style waveshaper,
/// a three-band tone stack, presence, and a speaker-cabinet filter.
pub struct Amp {
    p: AmpParams,
    sr: f32,
    pre_hp: [Biquad; 2],
    bass: [Biquad; 2],
    mid: [Biquad; 2],
    treble: [Biquad; 2],
    presence: [Biquad; 2],
    cab: [[Biquad; 3]; 2],
    dc: [f32; 2],
    dc_y: [f32; 2],
}

impl Amp {
    pub fn new(p: AmpParams, sr: f32) -> Self {
        let mut a = Self {
            pre_hp: [Biquad::default(); 2],
            bass: [Biquad::default(); 2],
            mid: [Biquad::default(); 2],
            treble: [Biquad::default(); 2],
            presence: [Biquad::default(); 2],
            cab: [[Biquad::default(); 3]; 2],
            dc: [0.0; 2],
            dc_y: [0.0; 2],
            p,
            sr,
        };
        a.update();
        a
    }

    fn update(&mut self) {
        let db = |v: f32| (v.clamp(0.0, 1.0) - 0.5) * 24.0;
        let sr = self.sr;
        let hp = if self.p.model == AmpModel::Bass { 30.0 } else { 90.0 };
        for c in 0..2 {
            self.pre_hp[c].design(BiquadKind::HighPass, hp, 0.707, 0.0, sr);
            self.bass[c].design(BiquadKind::LowShelf, 120.0, 0.707, db(self.p.bass), sr);
            self.mid[c].design(BiquadKind::Peak, if self.p.model == AmpModel::Bass { 500.0 } else { 750.0 }, 0.8, db(self.p.middle), sr);
            self.treble[c].design(BiquadKind::HighShelf, 2_800.0, 0.707, db(self.p.treble), sr);
            self.presence[c].design(BiquadKind::Peak, 4_500.0, 1.2, db(self.p.presence) * 0.6, sr);
            let top = if self.p.model == AmpModel::Bass { 3_500.0 } else { 5_500.0 };
            self.cab[c][0].design(BiquadKind::LowPass, top, 0.9, 0.0, sr);
            self.cab[c][1].design(BiquadKind::Peak, 2_200.0, 2.0, 4.0, sr);
            self.cab[c][2].design(BiquadKind::HighPass, if self.p.model == AmpModel::Bass { 40.0 } else { 80.0 }, 0.707, 0.0, sr);
        }
    }

    #[inline]
    fn shape(&self, x: f32) -> f32 {
        // Asymmetric soft clip: positive half saturates harder (even harmonics).
        match self.p.model {
            AmpModel::Clean => (x * 0.7).tanh() / 0.7f32.tanh(),
            AmpModel::Crunch | AmpModel::Bass => {
                if x >= 0.0 {
                    x.tanh()
                } else {
                    (x * 0.8).tanh() / 0.8
                }
            }
            AmpModel::Lead => {
                let y = (x * 1.6).tanh();
                (y + 0.15 * y * y).clamp(-1.0, 1.0)
            }
        }
    }
}

impl Effect for Amp {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let base = match self.p.model {
            AmpModel::Clean => 1.0,
            AmpModel::Crunch => 6.0,
            AmpModel::Lead => 20.0,
            AmpModel::Bass => 3.0,
        };
        let gain = base * (0.25 + 4.0 * self.p.gain.clamp(0.0, 1.0).powi(2));
        let out = db_to_gain(self.p.output_db);
        for n in 0..left.len() {
            let ins = [left[n], right[n]];
            let mut ys = [0.0f32; 2];
            for c in 0..2 {
                let mut y = self.pre_hp[c].process(ins[c]) * gain;
                y = self.shape(y);
                // DC blocker after the asymmetric stage.
                let d = y - self.dc[c] + 0.995 * self.dc_y[c];
                self.dc[c] = y;
                self.dc_y[c] = d;
                y = self.presence[c].process(self.treble[c].process(self.mid[c].process(self.bass[c].process(d))));
                if self.p.cabinet {
                    for b in self.cab[c].iter_mut() {
                        y = b.process(y);
                    }
                }
                ys[c] = y * out;
            }
            left[n] = mix(ins[0], ys[0], self.p.dry_wet);
            right[n] = mix(ins[1], ys[1], self.p.dry_wet);
        }
    }
    fn params(&self) -> &'static [ParamInfo] {
        AMP_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.gain = v,
            1 => self.p.bass = v,
            2 => self.p.middle = v,
            3 => self.p.treble = v,
            4 => self.p.presence = v,
            5 => self.p.output_db = v,
            6 => self.p.dry_wet = v,
            _ => return,
        }
        self.update();
    }
    fn name(&self) -> &str {
        "Amp"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{peak, rms};

    #[test]
    fn character_effects_are_stable() {
        let sr = 48_000.0;
        for model in [AmpModel::Clean, AmpModel::Crunch, AmpModel::Lead, AmpModel::Bass] {
            let mut a = Amp::new(AmpParams { model, gain: 1.0, ..Default::default() }, sr);
            let mut l: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.8).collect();
            let mut r = l.clone();
            a.process(&ProcessContext::new(sr, 120.0), &mut l, &mut r);
            assert!(l.iter().all(|x| x.is_finite()) && peak(&l) < 4.0, "{model:?}");
            assert!(rms(&l) > 0.01);
        }
        let mut d = DrumBuss::new(DrumBussParams { drive: 1.0, crunch: 1.0, boom: 1.0, boom_decay: 1.0, transients: 1.0, ..Default::default() }, sr);
        // Kick-like hits: decaying 55 Hz bursts every quarter second.
        let l0: Vec<f32> = (0..48_000)
            .map(|i| {
                let t = (i % 12_000) as f32 / sr;
                (std::f32::consts::TAU * 55.0 * t).sin() * (-t / 0.03).exp()
            })
            .collect();
        let mut l = l0.clone();
        let mut r = l.clone();
        d.process(&ProcessContext::new(sr, 120.0), &mut l, &mut r);
        assert!(l.iter().all(|x| x.is_finite()) && peak(&l) < 8.0);
        // The boom resonator should ring on after each hit has decayed.
        let tail = |x: &[f32]| rms(&x[7_000..11_000]);
        assert!(tail(&l) > 5.0 * tail(&l0), "no boom tail: {} vs {}", tail(&l), tail(&l0));
    }
}
