//! Soft-clipping saturator.

use super::mix;
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::{SaturatorParams, db_to_gain};

pub(crate) const PARAMS: &[ParamInfo] =
    &[ParamInfo::new("Drive", 0.0, 36.0, "dB").at("drive_db"), ParamInfo::new("Output", -36.0, 0.0, "dB").at("output_db"), ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet")];

pub struct Saturator {
    p: SaturatorParams,
    drive: f32,
    out: f32,
}

impl Saturator {
    pub fn new(p: SaturatorParams) -> Self {
        Self { drive: db_to_gain(p.drive_db), out: db_to_gain(p.output_db), p }
    }
}

impl Effect for Saturator {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        for s in left.iter_mut().chain(right.iter_mut()) {
            let wet = (*s * self.drive).tanh() * self.out;
            *s = mix(*s, wet, self.p.dry_wet);
        }
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => {
                self.p.drive_db = value;
                self.drive = db_to_gain(value);
            }
            1 => {
                self.p.output_db = value;
                self.out = db_to_gain(value);
            }
            2 => self.p.dry_wet = value,
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Saturator"
    }
}
