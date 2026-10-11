//! Parametric EQ with up to eight bands.

use crate::util::{Biquad, BiquadKind};
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::{EqBandKind, EqParams, db_to_gain};

/// Params are laid out per band: [freq, gain, q] for bands 0..8, then output.
const PARAMS: &[ParamInfo] = &[
    ParamInfo::new("1 Freq", 20.0, 20_000.0, "Hz"),
    ParamInfo::new("1 Gain", -15.0, 15.0, "dB"),
    ParamInfo::new("1 Q", 0.1, 18.0, ""),
    ParamInfo::new("2 Freq", 20.0, 20_000.0, "Hz"),
    ParamInfo::new("2 Gain", -15.0, 15.0, "dB"),
    ParamInfo::new("2 Q", 0.1, 18.0, ""),
    ParamInfo::new("3 Freq", 20.0, 20_000.0, "Hz"),
    ParamInfo::new("3 Gain", -15.0, 15.0, "dB"),
    ParamInfo::new("3 Q", 0.1, 18.0, ""),
    ParamInfo::new("4 Freq", 20.0, 20_000.0, "Hz"),
    ParamInfo::new("4 Gain", -15.0, 15.0, "dB"),
    ParamInfo::new("4 Q", 0.1, 18.0, ""),
    ParamInfo::new("5 Freq", 20.0, 20_000.0, "Hz"),
    ParamInfo::new("5 Gain", -15.0, 15.0, "dB"),
    ParamInfo::new("5 Q", 0.1, 18.0, ""),
    ParamInfo::new("6 Freq", 20.0, 20_000.0, "Hz"),
    ParamInfo::new("6 Gain", -15.0, 15.0, "dB"),
    ParamInfo::new("6 Q", 0.1, 18.0, ""),
    ParamInfo::new("7 Freq", 20.0, 20_000.0, "Hz"),
    ParamInfo::new("7 Gain", -15.0, 15.0, "dB"),
    ParamInfo::new("7 Q", 0.1, 18.0, ""),
    ParamInfo::new("8 Freq", 20.0, 20_000.0, "Hz"),
    ParamInfo::new("8 Gain", -15.0, 15.0, "dB"),
    ParamInfo::new("8 Q", 0.1, 18.0, ""),
    ParamInfo::new("Output", -24.0, 24.0, "dB"),
];

fn kind_of(k: EqBandKind) -> BiquadKind {
    match k {
        EqBandKind::LowCut => BiquadKind::HighPass,
        EqBandKind::HighCut => BiquadKind::LowPass,
        EqBandKind::LowShelf => BiquadKind::LowShelf,
        EqBandKind::HighShelf => BiquadKind::HighShelf,
        EqBandKind::Peak => BiquadKind::Peak,
        EqBandKind::Notch => BiquadKind::Notch,
    }
}

pub struct ParametricEq {
    p: EqParams,
    sr: f32,
    filters: Vec<[Biquad; 2]>,
    out_gain: f32,
}

impl ParametricEq {
    pub fn new(mut p: EqParams, sample_rate: f32) -> Self {
        p.bands.truncate(8);
        let mut eq = Self { filters: vec![[Biquad::default(); 2]; p.bands.len()], sr: sample_rate, out_gain: db_to_gain(p.output_db), p };
        for i in 0..eq.p.bands.len() {
            eq.redesign(i);
        }
        eq
    }

    fn redesign(&mut self, i: usize) {
        let b = self.p.bands[i];
        for f in self.filters[i].iter_mut() {
            f.design(kind_of(b.kind), b.freq_hz, b.q, b.gain_db, self.sr);
        }
    }

    /// Combined magnitude response in dB, for drawing the curve.
    pub fn response_db(&self, freq: f32) -> f32 {
        let mag: f32 = self.p.bands.iter().zip(&self.filters).filter(|(b, _)| b.enabled).map(|(_, f)| f[0].magnitude(freq, self.sr)).product();
        20.0 * (mag * self.out_gain).max(1e-6).log10()
    }
}

impl Effect for ParametricEq {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        for (b, f) in self.p.bands.iter().zip(self.filters.iter_mut()) {
            if !b.enabled {
                continue;
            }
            let [fl, fr] = f;
            left.iter_mut().for_each(|s| *s = fl.process(*s));
            right.iter_mut().for_each(|s| *s = fr.process(*s));
        }
        if (self.out_gain - 1.0).abs() > 1e-6 {
            left.iter_mut().chain(right.iter_mut()).for_each(|s| *s *= self.out_gain);
        }
    }

    fn reset(&mut self) {
        self.filters.iter_mut().flatten().for_each(Biquad::reset);
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index == 24 {
            self.p.output_db = value;
            self.out_gain = db_to_gain(value);
            return;
        }
        let (band, field) = (index / 3, index % 3);
        if band >= self.p.bands.len() {
            return;
        }
        match field {
            0 => self.p.bands[band].freq_hz = value,
            1 => self.p.bands[band].gain_db = value,
            _ => self.p.bands[band].q = value,
        }
        self.redesign(band);
    }

    fn name(&self) -> &str {
        "EQ"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcraft_core::EqBand;

    #[test]
    fn response_reflects_bands() {
        let p = EqParams {
            bands: vec![EqBand { kind: EqBandKind::Peak, freq_hz: 2_000.0, gain_db: -9.0, q: 1.0, enabled: true }],
            output_db: 0.0,
        };
        let eq = ParametricEq::new(p, 48_000.0);
        assert!((eq.response_db(2_000.0) + 9.0).abs() < 0.2);
        assert!(eq.response_db(100.0).abs() < 0.5);
    }
}
