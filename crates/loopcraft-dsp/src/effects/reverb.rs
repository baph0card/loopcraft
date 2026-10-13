//! Algorithmic reverb using the public-domain Freeverb topology
//! (parallel lowpass-feedback combs into series allpasses).

use super::mix;
use crate::util::DelayLine;
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::ReverbParams;

pub(crate) const PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Size", 0.0, 1.0, "").at("size"),
    ParamInfo::new("Damping", 0.0, 1.0, "").at("damping"),
    ParamInfo::new("Width", 0.0, 1.0, "").at("width"),
    ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet"),
];

// Tunings at 44.1 kHz, scaled to the actual rate.
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;

struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(1)], pos: 0, store: 0.0 }
    }
    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let y = self.buf[self.pos];
        self.store = y * (1.0 - damp) + self.store * damp;
        self.buf[self.pos] = x + self.store * feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        y
    }
}

struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(1)], pos: 0 }
    }
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.pos];
        self.buf[self.pos] = x + b * 0.5;
        self.pos = (self.pos + 1) % self.buf.len();
        b - x
    }
}

pub struct Reverb {
    p: ReverbParams,
    combs_l: Vec<Comb>,
    combs_r: Vec<Comb>,
    ap_l: Vec<Allpass>,
    ap_r: Vec<Allpass>,
    predelay: DelayLine,
    predelay_samples: f32,
}

impl Reverb {
    pub fn new(p: ReverbParams, sample_rate: f32) -> Self {
        let s = sample_rate / 44_100.0;
        let sc = |n: usize| ((n as f32) * s) as usize;
        Self {
            combs_l: COMBS.iter().map(|&n| Comb::new(sc(n))).collect(),
            combs_r: COMBS.iter().map(|&n| Comb::new(sc(n + SPREAD))).collect(),
            ap_l: ALLPASSES.iter().map(|&n| Allpass::new(sc(n))).collect(),
            ap_r: ALLPASSES.iter().map(|&n| Allpass::new(sc(n + SPREAD))).collect(),
            predelay: DelayLine::new((0.25 * sample_rate) as usize),
            predelay_samples: (p.predelay_ms * 0.001 * sample_rate).max(1.0),
            p,
        }
    }
}

impl Effect for Reverb {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let feedback = 0.7 + self.p.size.clamp(0.0, 1.0) * 0.28;
        let damp = self.p.damping.clamp(0.0, 1.0) * 0.4;
        let w = self.p.width.clamp(0.0, 1.0);
        let (wet1, wet2) = ((w / 2.0 + 0.5), ((1.0 - w) / 2.0));
        let gain = 0.015;
        for n in 0..left.len() {
            let (dl, dr) = (left[n], right[n]);
            self.predelay.write((dl + dr) * gain);
            let x = self.predelay.read(self.predelay_samples);
            let mut ol = 0.0;
            let mut or = 0.0;
            for c in self.combs_l.iter_mut() {
                ol += c.process(x, feedback, damp);
            }
            for c in self.combs_r.iter_mut() {
                or += c.process(x, feedback, damp);
            }
            for a in self.ap_l.iter_mut() {
                ol = a.process(ol);
            }
            for a in self.ap_r.iter_mut() {
                or = a.process(or);
            }
            let wl = ol * wet1 + or * wet2;
            let wr = or * wet1 + ol * wet2;
            left[n] = mix(dl, wl, self.p.dry_wet);
            right[n] = mix(dr, wr, self.p.dry_wet);
        }
    }

    fn reset(&mut self) {
        for c in self.combs_l.iter_mut().chain(self.combs_r.iter_mut()) {
            c.buf.iter_mut().for_each(|s| *s = 0.0);
            c.store = 0.0;
        }
        for a in self.ap_l.iter_mut().chain(self.ap_r.iter_mut()) {
            a.buf.iter_mut().for_each(|s| *s = 0.0);
        }
        self.predelay.clear();
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.p.size = value,
            1 => self.p.damping = value,
            2 => self.p.width = value,
            3 => self.p.dry_wet = value,
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Reverb"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rms;

    #[test]
    fn impulse_produces_decaying_tail() {
        let mut rv = Reverb::new(ReverbParams { dry_wet: 1.0, ..Default::default() }, 48_000.0);
        let ctx = ProcessContext::new(48_000.0, 120.0);
        let mut l = vec![0.0; 96_000];
        let mut r = vec![0.0; 96_000];
        l[0] = 1.0;
        r[0] = 1.0;
        rv.process(&ctx, &mut l, &mut r);
        let early = rms(&l[4_800..24_000]);
        let late = rms(&l[72_000..96_000]);
        assert!(early > 1e-3, "no tail: {early}");
        assert!(late < early, "tail grows: {early} -> {late}");
        assert!(l.iter().all(|x| x.is_finite()));
    }
}
