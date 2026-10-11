//! Utility: gain, stereo width, polarity and bass mono-ing.

use crate::util::{Biquad, BiquadKind};
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::{UtilityParams, db_to_gain};

pub(crate) const PARAMS: &[ParamInfo] = &[ParamInfo::new("Gain", -35.0, 35.0, "dB"), ParamInfo::new("Width", 0.0, 2.0, "")];

pub struct Utility {
    p: UtilityParams,
    gain: f32,
    side_hp: Biquad,
}

impl Utility {
    pub fn new(p: UtilityParams, sample_rate: f32) -> Self {
        let mut side_hp = Biquad::default();
        if let Some(hz) = p.mono_bass_hz {
            side_hp.design(BiquadKind::HighPass, hz, 0.707, 0.0, sample_rate);
        }
        Self { gain: db_to_gain(p.gain_db), side_hp, p }
    }
}

impl Effect for Utility {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let w = self.p.width.clamp(0.0, 2.0);
        let (il, ir) = (if self.p.invert_left { -1.0 } else { 1.0 }, if self.p.invert_right { -1.0 } else { 1.0 });
        let mono_bass = self.p.mono_bass_hz.is_some();
        for n in 0..left.len() {
            let l = left[n] * il;
            let r = right[n] * ir;
            let m = (l + r) * 0.5;
            let mut s = (l - r) * 0.5 * w;
            if mono_bass {
                s = self.side_hp.process(s);
            }
            left[n] = (m + s) * self.gain;
            right[n] = (m - s) * self.gain;
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
            1 => self.p.width = value,
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Utility"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_width_is_mono() {
        let mut u = Utility::new(UtilityParams { width: 0.0, ..Default::default() }, 48_000.0);
        let ctx = ProcessContext::new(48_000.0, 120.0);
        let mut l = vec![1.0, 0.0];
        let mut r = vec![0.0, 1.0];
        u.process(&ctx, &mut l, &mut r);
        assert_eq!(l, r);
    }
}
