//! Modulation effects: Chorus, Flanger, Phaser, Auto Pan, plus Redux.

use super::mix;
use crate::util::{DelayLine, Lfo, lfo_shape};
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::*;

// ---------------------------------------------------------------------------
// Chorus
// ---------------------------------------------------------------------------

const CHORUS_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Rate", 0.01, 10.0, "Hz").at("rate_hz"),
    ParamInfo::new("Depth", 0.0, 20.0, "ms").at("depth_ms"),
    ParamInfo::new("Delay", 1.0, 40.0, "ms").at("delay_ms"),
    ParamInfo::new("Feedback", 0.0, 0.9, "").at("feedback"),
    ParamInfo::new("Width", 0.0, 1.0, "").at("width"),
    ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet"),
];

pub struct Chorus {
    p: ChorusParams,
    sr: f32,
    dl: [DelayLine; 2],
    lfos: [Lfo; 4],
    fb: [f32; 2],
}

impl Chorus {
    pub fn new(p: ChorusParams, sr: f32) -> Self {
        let n = (0.08 * sr) as usize;
        let rate = p.rate_hz;
        Self {
            dl: [DelayLine::new(n), DelayLine::new(n)],
            lfos: [Lfo::new(rate, sr, 0.0), Lfo::new(rate, sr, 0.25), Lfo::new(rate * 1.13, sr, 0.5), Lfo::new(rate * 1.13, sr, 0.75)],
            fb: [0.0; 2],
            p,
            sr,
        }
    }
}

impl Effect for Chorus {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let voices = self.p.voices.clamp(1, 2);
        let base = self.p.delay_ms * 0.001 * self.sr;
        let depth = self.p.depth_ms * 0.001 * self.sr;
        let w = self.p.width.clamp(0.0, 1.0);
        for n in 0..left.len() {
            let ins = [left[n], right[n]];
            let mut wet = [0.0f32; 2];
            for ch in 0..2 {
                let mut acc = 0.0;
                for v in 0..voices {
                    // Each channel uses its own quadrature LFOs; width blends them.
                    let li = ch + 2 * v;
                    let own = self.lfos[li].next();
                    let d = base + depth * 0.5 * (1.0 + own * (0.5 + 0.5 * w));
                    acc += self.dl[ch].read(d.max(1.0));
                }
                wet[ch] = acc / voices as f32;
                self.dl[ch].write(ins[ch] + self.fb[ch] * self.p.feedback.clamp(0.0, 0.9));
                self.fb[ch] = wet[ch];
            }
            // Width: 0 = mono wet, 1 = full stereo.
            let m = (wet[0] + wet[1]) * 0.5;
            let (wl, wr) = (m + (wet[0] - m) * w, m + (wet[1] - m) * w);
            left[n] = mix(ins[0], wl, self.p.dry_wet);
            right[n] = mix(ins[1], wr, self.p.dry_wet);
        }
    }
    fn reset(&mut self) {
        self.dl.iter_mut().for_each(DelayLine::clear);
        self.fb = [0.0; 2];
    }
    fn params(&self) -> &'static [ParamInfo] {
        CHORUS_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => {
                self.p.rate_hz = v;
                for (k, l) in self.lfos.iter_mut().enumerate() {
                    l.set_rate(if k >= 2 { v * 1.13 } else { v }, self.sr);
                }
            }
            1 => self.p.depth_ms = v,
            2 => self.p.delay_ms = v,
            3 => self.p.feedback = v,
            4 => self.p.width = v,
            5 => self.p.dry_wet = v,
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Chorus"
    }
}

// ---------------------------------------------------------------------------
// Flanger
// ---------------------------------------------------------------------------

const FLANGER_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Rate", 0.01, 10.0, "Hz").at("rate_hz"),
    ParamInfo::new("Depth", 0.0, 10.0, "ms").at("depth_ms"),
    ParamInfo::new("Delay", 0.1, 10.0, "ms").at("delay_ms"),
    ParamInfo::new("Feedback", -0.95, 0.95, "").at("feedback"),
    ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet"),
];

pub struct Flanger {
    p: FlangerParams,
    sr: f32,
    dl: [DelayLine; 2],
    lfo: Lfo,
    fb: [f32; 2],
}

impl Flanger {
    pub fn new(p: FlangerParams, sr: f32) -> Self {
        let n = (0.03 * sr) as usize;
        Self { dl: [DelayLine::new(n), DelayLine::new(n)], lfo: Lfo::new(p.rate_hz, sr, 0.0), fb: [0.0; 2], p, sr }
    }
}

impl Effect for Flanger {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let base = self.p.delay_ms * 0.001 * self.sr;
        let depth = self.p.depth_ms * 0.001 * self.sr;
        let fbk = self.p.feedback.clamp(-0.95, 0.95);
        let bpf = ctx.tempo / 60.0 / self.sr as f64;
        for n in 0..left.len() {
            let (ph_l, ph_r) = match self.p.sync_beats {
                Some(period) => {
                    let ph = (ctx.beat + n as f64 * bpf) / period.max(1.0 / 16.0);
                    (lfo_shape(LfoShape::Triangle, ph), lfo_shape(LfoShape::Triangle, ph + 0.25))
                }
                None => {
                    let ph = self.lfo.phase();
                    self.lfo.next();
                    (lfo_shape(LfoShape::Triangle, ph), lfo_shape(LfoShape::Triangle, ph + 0.25))
                }
            };
            let ins = [left[n], right[n]];
            for (ch, ph) in [ph_l, ph_r].into_iter().enumerate() {
                let d = base + depth * 0.5 * (1.0 + ph);
                let wet = self.dl[ch].read(d.max(1.0));
                self.dl[ch].write(ins[ch] + wet * fbk);
                self.fb[ch] = wet;
            }
            left[n] = mix(ins[0], (ins[0] + self.fb[0]) * 0.5, self.p.dry_wet * 2.0).clamp(-4.0, 4.0);
            right[n] = mix(ins[1], (ins[1] + self.fb[1]) * 0.5, self.p.dry_wet * 2.0).clamp(-4.0, 4.0);
        }
    }
    fn reset(&mut self) {
        self.dl.iter_mut().for_each(DelayLine::clear);
    }
    fn params(&self) -> &'static [ParamInfo] {
        FLANGER_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => {
                self.p.rate_hz = v;
                self.lfo.set_rate(v, self.sr);
            }
            1 => self.p.depth_ms = v,
            2 => self.p.delay_ms = v,
            3 => self.p.feedback = v,
            4 => self.p.dry_wet = v,
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Flanger"
    }
}

// ---------------------------------------------------------------------------
// Phaser
// ---------------------------------------------------------------------------

const PHASER_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Rate", 0.01, 10.0, "Hz").at("rate_hz"),
    ParamInfo::new("Center", 100.0, 8_000.0, "Hz").at("center_hz"),
    ParamInfo::new("Depth", 0.0, 4.0, "oct").at("depth"),
    ParamInfo::new("Feedback", 0.0, 0.95, "").at("feedback"),
    ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet"),
];

/// First-order allpass stage (bilinear).
#[derive(Clone, Copy, Default)]
struct Allpass1 {
    z: f32,
}

impl Allpass1 {
    #[inline]
    fn process(&mut self, x: f32, a: f32) -> f32 {
        let y = -a * x + self.z;
        self.z = x + a * y;
        y
    }
}

pub struct Phaser {
    p: PhaserParams,
    sr: f32,
    stages: [[Allpass1; 12]; 2],
    lfo: Lfo,
    fb: [f32; 2],
}

impl Phaser {
    pub fn new(p: PhaserParams, sr: f32) -> Self {
        Self { stages: [[Allpass1::default(); 12]; 2], lfo: Lfo::new(p.rate_hz, sr, 0.0), fb: [0.0; 2], p, sr }
    }
}

impl Effect for Phaser {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let n_st = self.p.stages.clamp(2, 12);
        let bpf = ctx.tempo / 60.0 / self.sr as f64;
        for n in 0..left.len() {
            let ph = match self.p.sync_beats {
                Some(period) => (ctx.beat + n as f64 * bpf) / period.max(1.0 / 16.0),
                None => {
                    let ph = self.lfo.phase();
                    self.lfo.next();
                    ph
                }
            };
            let ins = [left[n], right[n]];
            let mut outs = [0.0f32; 2];
            for ch in 0..2 {
                // Right channel offset by a quarter cycle for stereo motion.
                let m = lfo_shape(LfoShape::Sine, ph + ch as f64 * 0.25);
                let f = (self.p.center_hz * 2f32.powf(m * self.p.depth * 0.5)).clamp(20.0, self.sr * 0.45);
                let t = (std::f32::consts::PI * f / self.sr).tan();
                let a = (1.0 - t) / (1.0 + t);
                let mut y = ins[ch] + self.fb[ch] * self.p.feedback.clamp(0.0, 0.95);
                for s in self.stages[ch].iter_mut().take(n_st) {
                    y = s.process(y, a);
                }
                self.fb[ch] = y;
                outs[ch] = y;
            }
            left[n] = mix(ins[0], (ins[0] + outs[0]) * 0.5, self.p.dry_wet * 2.0);
            right[n] = mix(ins[1], (ins[1] + outs[1]) * 0.5, self.p.dry_wet * 2.0);
        }
    }
    fn reset(&mut self) {
        self.stages = [[Allpass1::default(); 12]; 2];
        self.fb = [0.0; 2];
    }
    fn params(&self) -> &'static [ParamInfo] {
        PHASER_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => {
                self.p.rate_hz = v;
                self.lfo.set_rate(v, self.sr);
            }
            1 => self.p.center_hz = v,
            2 => self.p.depth = v,
            3 => self.p.feedback = v,
            4 => self.p.dry_wet = v,
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Phaser"
    }
}

// ---------------------------------------------------------------------------
// Auto Pan / tremolo
// ---------------------------------------------------------------------------

const AUTOPAN_PARAMS: &[ParamInfo] = &[ParamInfo::new("Rate", 0.0625, 16.0, "beats").at("rate_beats"), ParamInfo::new("Amount", 0.0, 1.0, "").at("amount"), ParamInfo::new("Phase", 0.0, 360.0, "deg").at("phase_deg")];

pub struct AutoPan {
    p: AutoPanParams,
    sr: f32,
}

impl AutoPan {
    pub fn new(p: AutoPanParams, sr: f32) -> Self {
        Self { p, sr }
    }
}

impl Effect for AutoPan {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let bpf = ctx.tempo / 60.0 / self.sr as f64;
        let period = self.p.rate_beats.max(1.0 / 32.0);
        let off = self.p.phase_deg as f64 / 360.0;
        let a = self.p.amount.clamp(0.0, 1.0);
        for n in 0..left.len() {
            let ph = (ctx.beat + n as f64 * bpf) / period;
            let gl = 1.0 - a * 0.5 * (1.0 - lfo_shape(self.p.shape, ph));
            let gr = 1.0 - a * 0.5 * (1.0 - lfo_shape(self.p.shape, ph + off));
            left[n] *= gl;
            right[n] *= gr;
        }
    }
    fn params(&self) -> &'static [ParamInfo] {
        AUTOPAN_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.rate_beats = v as f64,
            1 => self.p.amount = v,
            2 => self.p.phase_deg = v,
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Auto Pan"
    }
}

// ---------------------------------------------------------------------------
// Redux
// ---------------------------------------------------------------------------

const REDUX_PARAMS: &[ParamInfo] = &[ParamInfo::new("Bits", 1.0, 24.0, "").at("bits"), ParamInfo::new("Downsample", 1.0, 64.0, "x").at("downsample"), ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet")];

pub struct Redux {
    p: ReduxParams,
    hold: [f32; 2],
    count: f32,
}

impl Redux {
    pub fn new(p: ReduxParams) -> Self {
        Self { p, hold: [0.0; 2], count: 0.0 }
    }
}

impl Effect for Redux {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let levels = 2f32.powf(self.p.bits.clamp(1.0, 24.0) - 1.0);
        let ds = self.p.downsample.max(1.0);
        for n in 0..left.len() {
            self.count -= 1.0;
            if self.count <= 0.0 {
                self.count += ds;
                self.hold = [(left[n] * levels).round() / levels, (right[n] * levels).round() / levels];
            }
            left[n] = mix(left[n], self.hold[0], self.p.dry_wet);
            right[n] = mix(right[n], self.hold[1], self.p.dry_wet);
        }
    }
    fn params(&self) -> &'static [ParamInfo] {
        REDUX_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.bits = v,
            1 => self.p.downsample = v,
            2 => self.p.dry_wet = v,
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Redux"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rms;

    fn sine(hz: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (std::f32::consts::TAU * hz * i as f32 / 48_000.0).sin() * 0.5).collect()
    }

    fn process(fx: &mut dyn Effect, l: &mut [f32], r: &mut [f32]) {
        let mut ctx = ProcessContext::new(48_000.0, 120.0);
        ctx.playing = true;
        let bpf = 120.0 / 60.0 / 48_000.0;
        let mut pos = 0;
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            ctx.beat = pos as f64 * bpf;
            fx.process(&ctx, a, b);
            pos += a.len();
        }
    }

    #[test]
    fn modulation_effects_stay_stable_and_audible() {
        let sr = 48_000.0;
        let mut fxs: Vec<Box<dyn Effect>> = vec![
            Box::new(Chorus::new(ChorusParams::default(), sr)),
            Box::new(Flanger::new(FlangerParams { feedback: 0.9, ..Default::default() }, sr)),
            Box::new(Phaser::new(PhaserParams { feedback: 0.9, ..Default::default() }, sr)),
            Box::new(AutoPan::new(AutoPanParams::default(), sr)),
            Box::new(Redux::new(ReduxParams::default())),
        ];
        for fx in fxs.iter_mut() {
            let (mut l, mut r) = (sine(330.0, 96_000), sine(330.0, 96_000));
            process(fx.as_mut(), &mut l, &mut r);
            assert!(l.iter().chain(r.iter()).all(|x| x.is_finite() && x.abs() < 4.0), "{} unstable", fx.name());
            assert!(rms(&l[48_000..]) > 0.05, "{} silent", fx.name());
        }
    }

    #[test]
    fn autopan_moves_opposite_sides() {
        let mut ap = AutoPan::new(AutoPanParams { rate_beats: 1.0, amount: 1.0, shape: LfoShape::Sine, phase_deg: 180.0 }, 48_000.0);
        let (mut l, mut r) = (vec![1.0; 24_000], vec![1.0; 24_000]);
        process(&mut ap, &mut l, &mut r);
        // A quarter beat in (sine peak): left loud, right quiet.
        let q = 6_000;
        assert!(l[q] > 0.95 && r[q] < 0.05, "{} {}", l[q], r[q]);
    }

    #[test]
    fn redux_quantizes() {
        let mut rd = Redux::new(ReduxParams { bits: 2.0, downsample: 1.0, dry_wet: 1.0 });
        let (mut l, mut r) = (vec![0.3, 0.6, -0.8], vec![0.0; 3]);
        process(&mut rd, &mut l, &mut r);
        assert_eq!(l, vec![0.5, 0.5, -1.0]);
    }
}
