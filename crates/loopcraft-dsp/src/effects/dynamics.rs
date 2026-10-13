//! Bus compressor, lookahead limiter, gate and three-band dynamics.

use super::{MAX_BLOCK, mix};
use crate::util::{Crossover, EnvFollower};
use crate::{Effect, ParamInfo, ProcessContext};
use loopcraft_core::*;

#[inline]
fn to_db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

// ---------------------------------------------------------------------------
// Glue compressor
// ---------------------------------------------------------------------------

const GLUE_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Threshold", -40.0, 0.0, "dB").at("threshold_db"),
    ParamInfo::new("Ratio", 1.0, 10.0, ":1").at("ratio"),
    ParamInfo::new("Attack", 0.01, 30.0, "ms").at("attack_ms"),
    ParamInfo::new("Release", 10.0, 1200.0, "ms").at("release_ms"),
    ParamInfo::new("Makeup", 0.0, 24.0, "dB").at("makeup_db"),
    ParamInfo::new("Range", 0.0, 70.0, "dB").at("range_db"),
    ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet"),
];

/// Smooth, program-dependent bus compressor. RMS-ish detection, soft knee,
/// and an "auto" release that blends a fast and a slow release so short
/// peaks recover quickly while sustained reduction recovers slowly.
pub struct GlueCompressor {
    p: GlueParams,
    sr: f32,
    gr_fast: f32,
    gr_slow: f32,
    att: f32,
    rel_fast: f32,
    rel_slow: f32,
    det: f32,
    det_coeff: f32,
    key: Vec<f32>,
    key_len: usize,
    pub gain_reduction_db: f32,
}

impl GlueCompressor {
    pub fn new(p: GlueParams, sr: f32) -> Self {
        let mut g = Self {
            p,
            sr,
            gr_fast: 0.0,
            gr_slow: 0.0,
            att: 0.0,
            rel_fast: 0.0,
            rel_slow: 0.0,
            det: 0.0,
            det_coeff: (-1.0 / (0.002 * sr)).exp(),
            key: vec![0.0; MAX_BLOCK],
            key_len: 0,
            gain_reduction_db: 0.0,
        };
        g.update();
        g
    }

    fn update(&mut self) {
        let c = |ms: f32| (-1.0 / (ms.max(0.01) * 0.001 * self.sr)).exp();
        self.att = c(self.p.attack_ms);
        let rel = self.p.release_ms.max(1.0);
        if self.p.auto_release {
            self.rel_fast = c(rel * 0.25);
            self.rel_slow = c(rel * 3.0);
        } else {
            self.rel_fast = c(rel);
            self.rel_slow = c(rel);
        }
    }

    fn curve(&self, level_db: f32) -> f32 {
        let over = level_db - self.p.threshold_db;
        let knee = 6.0;
        let slope = 1.0 - 1.0 / self.p.ratio.max(1.0);
        let gr = if over <= -knee / 2.0 {
            0.0
        } else if over < knee / 2.0 {
            slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
        } else {
            slope * over
        };
        gr.min(self.p.range_db.max(0.0))
    }
}

impl Effect for GlueCompressor {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let use_key = self.key_len >= left.len();
        let makeup = db_to_gain(self.p.makeup_db);
        let mut max_gr = 0.0f32;
        for n in 0..left.len() {
            let x = if use_key { self.key[n] } else { (left[n] * left[n] + right[n] * right[n]) * 0.5 };
            self.det = x + (self.det - x) * self.det_coeff;
            let target = self.curve(10.0 * self.det.max(1e-18).log10());
            // Attack drives both stages; they release at different speeds.
            for (gr, rel) in [(&mut self.gr_fast, self.rel_fast), (&mut self.gr_slow, self.rel_slow)] {
                let c = if target > *gr { self.att } else { rel };
                *gr = target + (*gr - target) * c;
            }
            // Auto release: the slow stage only holds when reduction has been sustained.
            let gr = if self.p.auto_release { self.gr_fast.max(self.gr_slow * 0.6) } else { self.gr_fast };
            max_gr = max_gr.max(gr);
            let g = db_to_gain(-gr) * makeup;
            let (mut l, mut r) = (left[n] * g, right[n] * g);
            if self.p.soft_clip {
                l = l.tanh();
                r = r.tanh();
            }
            left[n] = mix(left[n], l, self.p.dry_wet);
            right[n] = mix(right[n], r, self.p.dry_wet);
        }
        self.key_len = 0;
        self.gain_reduction_db = max_gr;
    }
    fn reset(&mut self) {
        self.gr_fast = 0.0;
        self.gr_slow = 0.0;
        self.det = 0.0;
    }
    fn params(&self) -> &'static [ParamInfo] {
        GLUE_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.threshold_db = v,
            1 => self.p.ratio = v,
            2 => self.p.attack_ms = v,
            3 => self.p.release_ms = v,
            4 => self.p.makeup_db = v,
            5 => self.p.range_db = v,
            6 => self.p.dry_wet = v,
            _ => return,
        }
        self.update();
    }
    fn sidechain(&mut self, l: &[f32], r: &[f32]) {
        let n = l.len().min(MAX_BLOCK);
        for i in 0..n {
            self.key[i] = (l[i] * l[i] + r[i] * r[i]) * 0.5;
        }
        self.key_len = n;
    }
    fn wants_sidechain(&self) -> Option<usize> {
        self.p.sidechain_track
    }
    fn name(&self) -> &str {
        "Glue Compressor"
    }
}

// ---------------------------------------------------------------------------
// Limiter
// ---------------------------------------------------------------------------

const LIMITER_PARAMS: &[ParamInfo] =
    &[ParamInfo::new("Gain", 0.0, 24.0, "dB").at("gain_db"), ParamInfo::new("Ceiling", -12.0, 0.0, "dB").at("ceiling_db"), ParamInfo::new("Release", 1.0, 1000.0, "ms").at("release_ms")];

/// Brickwall lookahead limiter. The required gain of every incoming sample
/// is pushed through a sliding-window minimum and a moving average over
/// the lookahead window, which guarantees the delayed output never exceeds
/// the ceiling while keeping the gain curve smooth.
pub struct Limiter {
    p: LimiterParams,
    la: usize,
    dl_l: Vec<f32>,
    dl_r: Vec<f32>,
    pos: usize,
    // sliding-window minimum (monotonic deque over a ring)
    dq_val: Vec<f32>,
    dq_idx: Vec<u64>,
    dq_head: usize,
    dq_len: usize,
    // moving average of the window minimum
    avg_ring: Vec<f32>,
    avg_sum: f64,
    t: u64,
    g: f32,
    rel: f32,
    sr: f32,
    pub gain_reduction_db: f32,
}

impl Limiter {
    pub fn new(p: LimiterParams, sr: f32) -> Self {
        let la = ((p.lookahead_ms.clamp(0.1, 10.0) * 0.001 * sr) as usize).max(1);
        let w = la + 1;
        let mut l = Self {
            p,
            la,
            dl_l: vec![0.0; la],
            dl_r: vec![0.0; la],
            pos: 0,
            dq_val: vec![0.0; w + 1],
            dq_idx: vec![0; w + 1],
            dq_head: 0,
            dq_len: 0,
            avg_ring: vec![1.0; w],
            avg_sum: w as f64,
            t: 0,
            g: 1.0,
            rel: 0.0,
            sr,
            gain_reduction_db: 0.0,
        };
        l.update();
        l
    }

    fn update(&mut self) {
        self.rel = (-1.0 / (self.p.release_ms.max(1.0) * 0.001 * self.sr)).exp();
    }

    fn window_min_push(&mut self, v: f32) -> f32 {
        let cap = self.dq_val.len();
        let w = (self.la + 1) as u64;
        // Pop larger values from the back.
        while self.dq_len > 0 {
            let back = (self.dq_head + self.dq_len - 1) % cap;
            if self.dq_val[back] >= v {
                self.dq_len -= 1;
            } else {
                break;
            }
        }
        let slot = (self.dq_head + self.dq_len) % cap;
        self.dq_val[slot] = v;
        self.dq_idx[slot] = self.t;
        self.dq_len += 1;
        // Drop expired values from the front.
        while self.dq_len > 0 && self.dq_idx[self.dq_head] + w <= self.t {
            self.dq_head = (self.dq_head + 1) % cap;
            self.dq_len -= 1;
        }
        self.dq_val[self.dq_head]
    }
}

impl Effect for Limiter {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let pre = db_to_gain(self.p.gain_db);
        let ceil = db_to_gain(self.p.ceiling_db.min(0.0));
        let w = self.la + 1;
        let mut max_gr = 0.0f32;
        for n in 0..left.len() {
            let (l, r) = (left[n] * pre, right[n] * pre);
            let peak = l.abs().max(r.abs());
            let need = if peak > ceil { ceil / peak } else { 1.0 };
            let m = self.window_min_push(need);
            let ai = (self.t as usize) % w;
            self.avg_sum += m as f64 - self.avg_ring[ai] as f64;
            self.avg_ring[ai] = m;
            let smooth = (self.avg_sum / w as f64) as f32;
            // Release upward slowly, but never above the safe curve.
            let released = smooth.min(self.g + (1.0 - self.g) * (1.0 - self.rel));
            self.g = released.min(smooth);
            // Delay line: write now, read the sample from `la` ago.
            let rp = self.pos;
            let (dl, dr) = (self.dl_l[rp], self.dl_r[rp]);
            self.dl_l[rp] = l;
            self.dl_r[rp] = r;
            self.pos = (self.pos + 1) % self.la;
            // The delayed sample is `la` old; the gain computed now covers it.
            let g = self.g;
            left[n] = (dl * g).clamp(-ceil, ceil);
            right[n] = (dr * g).clamp(-ceil, ceil);
            max_gr = max_gr.max(-to_db(g));
            self.t += 1;
        }
        self.gain_reduction_db = max_gr;
    }
    fn reset(&mut self) {
        self.dl_l.fill(0.0);
        self.dl_r.fill(0.0);
        self.avg_ring.fill(1.0);
        self.avg_sum = self.avg_ring.len() as f64;
        self.dq_len = 0;
        self.g = 1.0;
    }
    fn latency(&self) -> usize {
        self.la
    }
    fn params(&self) -> &'static [ParamInfo] {
        LIMITER_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.gain_db = v,
            1 => self.p.ceiling_db = v,
            2 => {
                self.p.release_ms = v;
                self.update();
            }
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Limiter"
    }
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

const GATE_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Threshold", -80.0, 0.0, "dB").at("threshold_db"),
    ParamInfo::new("Return", 0.0, 24.0, "dB").at("return_db"),
    ParamInfo::new("Attack", 0.01, 50.0, "ms").at("attack_ms"),
    ParamInfo::new("Hold", 0.0, 500.0, "ms").at("hold_ms"),
    ParamInfo::new("Release", 1.0, 2000.0, "ms").at("release_ms"),
    ParamInfo::new("Floor", -80.0, 0.0, "dB").at("floor_db"),
];

pub struct Gate {
    p: GateParams,
    sr: f32,
    det: EnvFollower,
    open: bool,
    hold_left: usize,
    g: f32,
    att: f32,
    rel: f32,
    key: Vec<f32>,
    key_len: usize,
}

impl Gate {
    pub fn new(p: GateParams, sr: f32) -> Self {
        let mut g = Self { p, sr, det: EnvFollower::new(0.1, 5.0, sr), open: false, hold_left: 0, g: 0.0, att: 0.0, rel: 0.0, key: vec![0.0; MAX_BLOCK], key_len: 0 };
        g.update();
        g
    }
    fn update(&mut self) {
        // Linear ramps sized so a full open/close takes the given time.
        self.att = 1.0 / (self.p.attack_ms.max(0.01) * 0.001 * self.sr);
        self.rel = (-4.6 / (self.p.release_ms.max(1.0) * 0.001 * self.sr)).exp();
    }
}

impl Effect for Gate {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let floor = db_to_gain(self.p.floor_db);
        let use_key = self.key_len >= left.len();
        let open_db = self.p.threshold_db;
        let close_db = self.p.threshold_db - self.p.return_db.max(0.0);
        let hold = (self.p.hold_ms * 0.001 * self.sr) as usize;
        for n in 0..left.len() {
            let x = if use_key { self.key[n] } else { left[n].abs().max(right[n].abs()) };
            let lvl = to_db(self.det.next(x));
            if lvl >= open_db {
                self.open = true;
                self.hold_left = hold;
            } else if lvl < close_db {
                if self.hold_left > 0 {
                    self.hold_left -= 1;
                } else {
                    self.open = false;
                }
            }
            if self.open {
                self.g = (self.g + self.att).min(1.0);
            } else {
                self.g *= self.rel;
            }
            let g = floor + (1.0 - floor) * self.g;
            left[n] *= g;
            right[n] *= g;
        }
        self.key_len = 0;
    }
    fn reset(&mut self) {
        self.det.reset();
        self.g = 0.0;
        self.open = false;
    }
    fn params(&self) -> &'static [ParamInfo] {
        GATE_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.threshold_db = v,
            1 => self.p.return_db = v,
            2 => self.p.attack_ms = v,
            3 => self.p.hold_ms = v,
            4 => self.p.release_ms = v,
            5 => self.p.floor_db = v,
            _ => return,
        }
        self.update();
    }
    fn sidechain(&mut self, l: &[f32], r: &[f32]) {
        let n = l.len().min(MAX_BLOCK);
        for i in 0..n {
            self.key[i] = l[i].abs().max(r[i].abs());
        }
        self.key_len = n;
    }
    fn wants_sidechain(&self) -> Option<usize> {
        self.p.sidechain_track
    }
    fn name(&self) -> &str {
        "Gate"
    }
}

// ---------------------------------------------------------------------------
// Multiband dynamics
// ---------------------------------------------------------------------------

const MB_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Low Thresh", -60.0, 0.0, "dB").at("thresholds_db/0"),
    ParamInfo::new("Mid Thresh", -60.0, 0.0, "dB").at("thresholds_db/1"),
    ParamInfo::new("High Thresh", -60.0, 0.0, "dB").at("thresholds_db/2"),
    ParamInfo::new("Low Gain", -24.0, 24.0, "dB").at("gains_db/0"),
    ParamInfo::new("Mid Gain", -24.0, 24.0, "dB").at("gains_db/1"),
    ParamInfo::new("High Gain", -24.0, 24.0, "dB").at("gains_db/2"),
    ParamInfo::new("Amount", 0.0, 1.0, "").at("amount"),
    ParamInfo::new("Low X-over", 40.0, 800.0, "Hz").at("low_crossover_hz"),
    ParamInfo::new("High X-over", 800.0, 12_000.0, "Hz").at("high_crossover_hz"),
];

struct Band {
    env: EnvFollower,
    gr_db: f32,
}

/// Three-band compressor/expander with Linkwitz-Riley crossovers; the low
/// band is phase-aligned through the high crossover's allpass so the bands
/// sum flat when no processing is applied.
pub struct Multiband {
    p: MultibandParams,
    sr: f32,
    x_low: [Crossover; 2],
    x_high: [Crossover; 2],
    ap_low: [Crossover; 2],
    bands: [Band; 3],
}

impl Multiband {
    pub fn new(p: MultibandParams, sr: f32) -> Self {
        let mk = |f: f32| [Crossover::new(f, sr), Crossover::new(f, sr)];
        let b = || Band { env: EnvFollower::new(p.attack_ms, p.release_ms, sr), gr_db: 0.0 };
        Self { x_low: mk(p.low_crossover_hz), x_high: mk(p.high_crossover_hz), ap_low: mk(p.high_crossover_hz), bands: [b(), b(), b()], p, sr }
    }

    fn band_gain(&mut self, b: usize, level: f32) -> f32 {
        let lvl = to_db(self.bands[b].env.next(level));
        let over = lvl - self.p.thresholds_db[b];
        let r = self.p.ratios[b].max(0.05);
        // ratio > 1: compress above threshold; ratio < 1: expand (boost) above it.
        let gr = if over > 0.0 { over * (1.0 - 1.0 / r) } else { 0.0 };
        let gr = gr.clamp(-24.0, 48.0) * self.p.amount;
        self.bands[b].gr_db = gr;
        db_to_gain(self.p.gains_db[b] - gr)
    }

    pub fn reduction_db(&self) -> [f32; 3] {
        [self.bands[0].gr_db, self.bands[1].gr_db, self.bands[2].gr_db]
    }
}

impl Effect for Multiband {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        for n in 0..left.len() {
            let mut lo = [0.0f32; 2];
            let mut mi = [0.0f32; 2];
            let mut hi = [0.0f32; 2];
            for (c, x) in [left[n], right[n]].into_iter().enumerate() {
                let (l, rest) = self.x_low[c].split(x);
                let (m, h) = self.x_high[c].split(rest);
                lo[c] = self.ap_low[c].allpass(l);
                mi[c] = m;
                hi[c] = h;
            }
            let g0 = self.band_gain(0, lo[0].abs().max(lo[1].abs()));
            let g1 = self.band_gain(1, mi[0].abs().max(mi[1].abs()));
            let g2 = self.band_gain(2, hi[0].abs().max(hi[1].abs()));
            left[n] = lo[0] * g0 + mi[0] * g1 + hi[0] * g2;
            right[n] = lo[1] * g0 + mi[1] * g1 + hi[1] * g2;
        }
    }
    fn params(&self) -> &'static [ParamInfo] {
        MB_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0..=2 => self.p.thresholds_db[i] = v,
            3..=5 => self.p.gains_db[i - 3] = v,
            6 => self.p.amount = v,
            7 => {
                self.p.low_crossover_hz = v;
                for x in self.x_low.iter_mut() {
                    x.set(v, self.sr);
                }
            }
            8 => {
                self.p.high_crossover_hz = v;
                for x in self.x_high.iter_mut().chain(self.ap_low.iter_mut()) {
                    x.set(v, self.sr);
                }
            }
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Multiband Dynamics"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{peak, rms};

    fn sine(hz: f32, amp: f32, n: usize, sr: f32) -> Vec<f32> {
        (0..n).map(|i| (std::f32::consts::TAU * hz * i as f32 / sr).sin() * amp).collect()
    }

    fn run(fx: &mut dyn Effect, l: &mut [f32], r: &mut [f32]) {
        let ctx = ProcessContext::new(48_000.0, 120.0);
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            fx.process(&ctx, a, b);
        }
    }

    #[test]
    fn limiter_never_exceeds_ceiling() {
        let sr = 48_000.0;
        let mut lim = Limiter::new(LimiterParams { gain_db: 12.0, ceiling_db: -1.0, release_ms: 50.0, lookahead_ms: 1.5 }, sr);
        // Spiky material: a loud click train over a sine.
        let mut l = sine(220.0, 0.5, 48_000, sr);
        for i in (0..48_000).step_by(4_000) {
            l[i] = 1.0;
        }
        let mut r = l.clone();
        run(&mut lim, &mut l, &mut r);
        let ceil = db_to_gain(-1.0);
        assert!(peak(&l) <= ceil + 1e-6, "peak {} > ceiling {}", peak(&l), ceil);
        assert!(rms(&l[4_800..]) > 0.3, "limiter crushed the level: {}", rms(&l[4_800..]));
        assert_eq!(lim.latency(), (0.0015 * sr) as usize);
    }

    #[test]
    fn limiter_is_transparent_below_ceiling() {
        let sr = 48_000.0;
        let mut lim = Limiter::new(LimiterParams { gain_db: 0.0, ceiling_db: -0.3, ..Default::default() }, sr);
        let src = sine(440.0, 0.3, 4_800, sr);
        let (mut l, mut r) = (src.clone(), src.clone());
        run(&mut lim, &mut l, &mut r);
        let d = lim.latency();
        for i in d..4_800 {
            assert!((l[i] - src[i - d]).abs() < 1e-5, "sample {i} changed");
        }
    }

    #[test]
    fn glue_reduces_loud_signals() {
        let sr = 48_000.0;
        let mut g = GlueCompressor::new(GlueParams { threshold_db: -20.0, ratio: 4.0, ..Default::default() }, sr);
        let (mut l, mut r) = (sine(100.0, 0.9, 24_000, sr), sine(100.0, 0.9, 24_000, sr));
        run(&mut g, &mut l, &mut r);
        assert!(g.gain_reduction_db > 6.0, "{}", g.gain_reduction_db);
        assert!(rms(&l[12_000..]) < 0.4);
    }

    #[test]
    fn gate_closes_on_quiet_and_opens_on_loud() {
        let sr = 48_000.0;
        let mut g = Gate::new(GateParams { threshold_db: -30.0, floor_db: -80.0, hold_ms: 5.0, release_ms: 20.0, ..Default::default() }, sr);
        let mut l = sine(200.0, 0.005, 9_600, sr); // -46 dB: below threshold
        l.extend(sine(200.0, 0.5, 9_600, sr));
        let mut r = l.clone();
        run(&mut g, &mut l, &mut r);
        assert!(peak(&l[4_800..9_600]) < 1e-4, "gate leaked quiet signal");
        assert!(rms(&l[14_400..]) > 0.3, "gate didn't open");
    }

    #[test]
    fn multiband_is_flat_when_idle() {
        let sr = 48_000.0;
        let mut mb = Multiband::new(MultibandParams { thresholds_db: [0.0; 3], ..Default::default() }, sr);
        for hz in [60.0, 500.0, 5_000.0] {
            let (mut l, mut r) = (sine(hz, 0.25, 24_000, sr), sine(hz, 0.25, 24_000, sr));
            run(&mut mb, &mut l, &mut r);
            let level = rms(&l[12_000..]);
            assert!((level - 0.177).abs() < 0.01, "{hz} Hz -> {level}");
        }
    }
}
