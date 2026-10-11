//! Feed-forward compressor with soft knee and optional external sidechain.

use super::{MAX_BLOCK, mix};
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::{CompressorParams, db_to_gain};

pub(crate) const PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Threshold", -60.0, 0.0, "dB"),
    ParamInfo::new("Ratio", 1.0, 20.0, ":1"),
    ParamInfo::new("Attack", 0.1, 100.0, "ms"),
    ParamInfo::new("Release", 5.0, 1000.0, "ms"),
    ParamInfo::new("Makeup", 0.0, 24.0, "dB"),
    ParamInfo::new("Dry/Wet", 0.0, 1.0, ""),
];

pub struct Compressor {
    p: CompressorParams,
    sr: f32,
    env_db: f32,
    attack: f32,
    release: f32,
    makeup: f32,
    key: Vec<f32>,
    key_len: usize,
    /// Most recent gain reduction in dB (positive number), for metering.
    pub gain_reduction_db: f32,
}

impl Compressor {
    pub fn new(p: CompressorParams, sample_rate: f32) -> Self {
        let mut c =
            Self { sr: sample_rate, env_db: 0.0, attack: 0.0, release: 0.0, makeup: 0.0, key: vec![0.0; MAX_BLOCK], key_len: 0, gain_reduction_db: 0.0, p };
        c.update();
        c
    }

    fn update(&mut self) {
        self.attack = (-1.0 / (self.p.attack_ms.max(0.05) * 0.001 * self.sr)).exp();
        self.release = (-1.0 / (self.p.release_ms.max(1.0) * 0.001 * self.sr)).exp();
        self.makeup = db_to_gain(self.p.makeup_db);
    }

    /// Static curve: gain reduction (dB, >= 0) for an input level in dB.
    pub fn reduction_for(&self, level_db: f32) -> f32 {
        let t = self.p.threshold_db;
        let r = self.p.ratio.max(1.0);
        let k = self.p.knee_db.max(0.0);
        let over = level_db - t;
        let slope = 1.0 - 1.0 / r;
        if k > 0.0 && over.abs() <= k / 2.0 {
            slope * (over + k / 2.0).powi(2) / (2.0 * k)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        }
    }
}

impl Effect for Compressor {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let use_key = self.key_len >= left.len();
        let mut max_gr = 0.0f32;
        for n in 0..left.len() {
            let detect = if use_key { self.key[n] } else { left[n].abs().max(right[n].abs()) };
            let level_db = 20.0 * detect.max(1e-6).log10();
            let target = self.reduction_for(level_db);
            let coeff = if target > self.env_db { self.attack } else { self.release };
            self.env_db = target + (self.env_db - target) * coeff;
            max_gr = max_gr.max(self.env_db);
            let g = db_to_gain(-self.env_db) * self.makeup;
            left[n] = mix(left[n], left[n] * g, self.p.dry_wet);
            right[n] = mix(right[n], right[n] * g, self.p.dry_wet);
        }
        self.key_len = 0;
        self.gain_reduction_db = max_gr;
    }

    fn reset(&mut self) {
        self.env_db = 0.0;
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.p.threshold_db = value,
            1 => self.p.ratio = value,
            2 => self.p.attack_ms = value,
            3 => self.p.release_ms = value,
            4 => self.p.makeup_db = value,
            5 => self.p.dry_wet = value,
            _ => return,
        }
        self.update();
    }

    fn sidechain(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len().min(MAX_BLOCK);
        for i in 0..n {
            self.key[i] = left[i].abs().max(right[i].abs());
        }
        self.key_len = n;
    }

    fn wants_sidechain(&self) -> Option<usize> {
        self.p.sidechain_track
    }

    fn name(&self) -> &str {
        "Compressor"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_curve() {
        let c = Compressor::new(CompressorParams { threshold_db: -20.0, ratio: 4.0, knee_db: 0.0, ..Default::default() }, 48_000.0);
        assert_eq!(c.reduction_for(-30.0), 0.0);
        assert!((c.reduction_for(-10.0) - 7.5).abs() < 1e-4);
    }

    #[test]
    fn sidechain_ducks_signal() {
        let mut c = Compressor::new(
            CompressorParams { threshold_db: -30.0, ratio: 20.0, attack_ms: 0.1, knee_db: 0.0, sidechain_track: Some(0), ..Default::default() },
            48_000.0,
        );
        let ctx = ProcessContext::new(48_000.0, 120.0);
        let key = vec![1.0; 512];
        let mut l = vec![0.1; 512];
        let mut r = vec![0.1; 512];
        c.sidechain(&key, &key);
        c.process(&ctx, &mut l, &mut r);
        assert!(l[511] < 0.01, "not ducked: {}", l[511]);
    }
}
