//! Stereo delay with tempo sync, ping-pong and a filtered feedback path.

use super::mix;
use crate::util::{DelayLine, Smoothed};
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::DelayParams;

const PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Time (beats)", 0.0625, 4.0, "beats"),
    ParamInfo::new("Feedback", 0.0, 0.98, ""),
    ParamInfo::new("Filter", 200.0, 18_000.0, "Hz"),
    ParamInfo::new("Dry/Wet", 0.0, 1.0, ""),
];

const MAX_SECONDS: f32 = 4.0;

pub struct Delay {
    p: DelayParams,
    sr: f32,
    l: DelayLine,
    r: DelayLine,
    time: Smoothed,
    lp_l: f32,
    lp_r: f32,
    lp_coeff: f32,
}

impl Delay {
    pub fn new(p: DelayParams, sample_rate: f32) -> Self {
        let max = (MAX_SECONDS * sample_rate) as usize + 4;
        let mut d = Self { sr: sample_rate, l: DelayLine::new(max), r: DelayLine::new(max), time: Smoothed::new(0.0, sample_rate, 60.0), lp_l: 0.0, lp_r: 0.0, lp_coeff: 0.0, p };
        d.update_filter();
        d
    }

    fn update_filter(&mut self) {
        self.lp_coeff = (-std::f32::consts::TAU * self.p.filter_hz / self.sr).exp();
    }

    fn delay_samples(&self, tempo: f64) -> f32 {
        let secs = if self.p.sync { self.p.time_beats * 60.0 / tempo.max(1.0) } else { self.p.time_ms as f64 / 1000.0 };
        (secs as f32 * self.sr).clamp(1.0, MAX_SECONDS * self.sr)
    }
}

impl Effect for Delay {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let target = self.delay_samples(ctx.tempo);
        if self.time.current == 0.0 {
            self.time = Smoothed::new(target, self.sr, 60.0);
        }
        self.time.set(target);
        let fb = self.p.feedback.clamp(0.0, 0.98);
        let c = self.lp_coeff;
        for n in 0..left.len() {
            let d = self.time.next();
            let wl = self.l.read(d);
            let wr = self.r.read(d);
            self.lp_l = wl + (self.lp_l - wl) * c;
            self.lp_r = wr + (self.lp_r - wr) * c;
            let (in_l, in_r) = (left[n], right[n]);
            if self.p.ping_pong {
                // Input enters on the left only; feedback crosses sides.
                self.l.write((in_l + in_r) * 0.5 + self.lp_r * fb);
                self.r.write(self.lp_l * fb);
            } else {
                self.l.write(in_l + self.lp_l * fb);
                self.r.write(in_r + self.lp_r * fb);
            }
            left[n] = mix(in_l, wl, self.p.dry_wet);
            right[n] = mix(in_r, wr, self.p.dry_wet);
        }
    }

    fn reset(&mut self) {
        self.l.clear();
        self.r.clear();
        self.lp_l = 0.0;
        self.lp_r = 0.0;
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.p.time_beats = value as f64,
            1 => self.p.feedback = value,
            2 => {
                self.p.filter_hz = value;
                self.update_filter();
            }
            3 => self.p.dry_wet = value,
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Delay"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_lands_on_the_beat() {
        // 120 bpm, 1 beat = 0.5 s = 500 samples at 1 kHz.
        let p = DelayParams { sync: true, time_beats: 1.0, feedback: 0.0, ping_pong: false, filter_hz: 18_000.0, dry_wet: 1.0, ..Default::default() };
        let mut d = Delay::new(p, 1_000.0);
        let ctx = ProcessContext::new(1_000.0, 120.0);
        let mut l = vec![0.0; 1000];
        let mut r = vec![0.0; 1000];
        l[0] = 1.0;
        r[0] = 1.0;
        d.process(&ctx, &mut l, &mut r);
        let peak_at = l.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
        assert!((499..=501).contains(&peak_at), "{peak_at}");
    }
}
