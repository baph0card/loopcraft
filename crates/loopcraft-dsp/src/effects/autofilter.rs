//! Auto Filter: resonant filter with a tempo-synced LFO locked to the song
//! position, so the sweep lines up with the beat every time playback starts.

use crate::util::Svf;
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::AutoFilterParams;

pub(crate) const PARAMS: &[ParamInfo] =
    &[ParamInfo::new("Cutoff", 20.0, 18_000.0, "Hz"), ParamInfo::new("Resonance", 0.0, 0.95, ""), ParamInfo::new("LFO Amount", 0.0, 5.0, "oct")];

pub struct AutoFilter {
    p: AutoFilterParams,
    sr: f32,
    fl: Svf,
    fr: Svf,
}

impl AutoFilter {
    pub fn new(p: AutoFilterParams, sample_rate: f32) -> Self {
        Self { p, sr: sample_rate, fl: Svf::default(), fr: Svf::default() }
    }
}

impl Effect for AutoFilter {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let beats_per_sample = ctx.tempo / 60.0 / self.sr as f64;
        let cycle = self.p.lfo_beats.max(0.0625);
        for n in 0..left.len() {
            if n % 16 == 0 {
                let beat = ctx.beat + n as f64 * beats_per_sample;
                let lfo = ((beat / cycle).fract() as f32 * std::f32::consts::TAU).sin();
                let cutoff = self.p.cutoff_hz * 2f32.powf(lfo * self.p.lfo_amount);
                self.fl.set(cutoff, self.p.resonance, self.sr);
                self.fr.set(cutoff, self.p.resonance, self.sr);
            }
            left[n] = self.fl.process(left[n], self.p.mode);
            right[n] = self.fr.process(right[n], self.p.mode);
        }
    }

    fn reset(&mut self) {
        self.fl.reset();
        self.fr.reset();
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.p.cutoff_hz = value,
            1 => self.p.resonance = value,
            2 => self.p.lfo_amount = value,
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Auto Filter"
    }
}
