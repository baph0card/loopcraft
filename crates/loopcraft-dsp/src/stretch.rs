//! Real-time WSOLA time-stretching for warped audio clips.
//!
//! The caller maps song position to a source-file position every sample
//! (that mapping is what keeps clips beat-locked). Every `hop` output
//! samples the stretcher starts a new Hann-windowed grain near the mapped
//! position. Following WSOLA (waveform-similarity overlap-add), the exact
//! start is nudged within a small search window to the offset whose
//! waveform best matches the natural continuation of the grain that is
//! fading out, which avoids the phase cancellation that plain granular
//! stretching suffers from. Grains read the source at `pitch_ratio`, so
//! tempo and pitch are independent.
//!
//! Also here: [`BeatsStretcher`] (transient slicing, for drums) and
//! [`PvStretcher`] (phase vocoder with identity phase locking, for full
//! mixes), plus [`detect_transients`], which both the Beats mode and the
//! UI's warp-marker suggestions use.

use crate::AudioBuffer;
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

#[derive(Clone, Copy, Default)]
struct Grain {
    start: f64,
    age: usize,
    active: bool,
}

pub struct GrainStretcher {
    grain_len: usize,
    hop: usize,
    window: Vec<f32>,
    grains: [Grain; 4],
    last: Option<usize>,
    counter: usize,
    search: usize,
    corr_len: usize,
    pub pitch_ratio: f64,
}

impl GrainStretcher {
    /// `grain_ms` around 40-80 ms works well for full mixes and loops.
    pub fn new(sample_rate: f32, grain_ms: f32) -> Self {
        let grain_len = ((grain_ms * 0.001 * sample_rate) as usize).max(64) & !1;
        let window = (0..grain_len).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / grain_len as f32).cos()).collect();
        Self {
            hop: grain_len / 2,
            grain_len,
            window,
            grains: [Grain::default(); 4],
            last: None,
            counter: 0,
            // +-8 ms search, 4 ms correlation window.
            search: (0.008 * sample_rate) as usize,
            corr_len: ((0.004 * sample_rate) as usize).max(16),
            pitch_ratio: 1.0,
        }
    }

    pub fn reset(&mut self) {
        self.grains = [Grain::default(); 4];
        self.last = None;
        self.counter = 0;
    }

    /// Find the start near `nominal` whose waveform best continues `cont`.
    fn best_start(&self, buf: &AudioBuffer, nominal: f64, cont: f64) -> f64 {
        let ch = &buf.channels[0];
        let len = ch.len() as i64;
        let c0 = cont.round() as i64;
        let n0 = nominal.round() as i64;
        let cl = self.corr_len as i64;
        if c0 < 0 || c0 + cl >= len {
            return nominal;
        }
        let s = self.search as i64;
        let (mut best, mut best_d) = (f32::MIN, 0i64);
        let mut d = -s;
        while d <= s {
            let p = n0 + d;
            if p >= 0 && p + cl < len {
                let mut acc = 0.0f32;
                let mut k = 0;
                while k < cl {
                    acc += ch[(p + k) as usize] * ch[(c0 + k) as usize];
                    k += 2;
                }
                if acc > best {
                    best = acc;
                    best_d = d;
                }
            }
            d += 2;
        }
        nominal + best_d as f64
    }

    /// Produce one stereo frame. `source_pos` is where the song position
    /// maps into the file right now, in source samples.
    #[inline]
    pub fn next(&mut self, buf: &AudioBuffer, source_pos: f64) -> (f32, f32) {
        if self.counter.is_multiple_of(self.hop) {
            let start = match self.last.map(|i| self.grains[i]) {
                Some(g) if g.active => self.best_start(buf, source_pos, g.start + g.age as f64 * self.pitch_ratio),
                _ => source_pos,
            };
            if let Some(i) = self.grains.iter().position(|g| !g.active) {
                self.grains[i] = Grain { start, age: 0, active: true };
                self.last = Some(i);
            }
        }
        self.counter = self.counter.wrapping_add(1);
        let stereo = buf.channels.len() > 1;
        let (mut l, mut r) = (0.0, 0.0);
        for g in self.grains.iter_mut().filter(|g| g.active) {
            let w = self.window[g.age];
            let pos = g.start + g.age as f64 * self.pitch_ratio;
            let sl = buf.read(0, pos);
            l += sl * w;
            r += if stereo { buf.read(1, pos) } else { sl } * w;
            g.age += 1;
            if g.age >= self.grain_len {
                g.active = false;
            }
        }
        (l, r)
    }
}

/// Onset positions (in samples) found from jumps in high-frequency energy.
/// Always includes 0. Onsets are at least `min_gap_ms` apart.
pub fn detect_transients(buf: &AudioBuffer, min_gap_ms: f32) -> Vec<usize> {
    let hop = 256usize;
    let n = buf.len();
    let mut out = vec![0usize];
    if n < hop * 4 {
        return out;
    }
    let mono = |i: usize| -> f32 { buf.channels.iter().map(|c| c[i]).sum::<f32>() / buf.channels.len() as f32 };
    // High-passed energy per hop (first difference emphasises attacks).
    let frames = n / hop;
    let mut e = vec![0.0f32; frames];
    for (f, ef) in e.iter_mut().enumerate() {
        let mut acc = 0.0;
        for i in f * hop + 1..(f + 1) * hop {
            let d = mono(i) - mono(i - 1);
            acc += d * d;
        }
        *ef = acc / hop as f32;
    }
    let peak = e.iter().cloned().fold(0.0f32, f32::max).max(1e-12);
    let floor = peak * 1e-3;
    let min_gap = ((min_gap_ms * 0.001 * buf.sample_rate) as usize / hop).max(1);
    let mut last = 0usize;
    for f in 2..frames {
        let prev = (e[f - 1] + e[f - 2]) * 0.5;
        if e[f] > floor && e[f] > prev * 4.0 && e[f] >= e[f - 1] && f - last >= min_gap {
            // Refine to the sample where the hop's energy starts rising.
            let start = f * hop;
            let mut best = start;
            let mut best_d = 0.0;
            for i in (start.saturating_sub(hop)).max(1)..(start + hop).min(n) {
                let d = (mono(i) - mono(i - 1)).abs();
                if d > best_d * 1.5 && d > 1e-4 {
                    best = i;
                    break;
                }
                best_d = f32::max(best_d, d);
            }
            let pos = best.saturating_sub(32);
            if pos > *out.last().unwrap_or(&0) + 64 {
                out.push(pos);
                last = f;
            }
        }
    }
    out
}

/// Beats mode: each transient slice plays at native speed starting when the
/// song reaches it; slices are faded out early when the clip is slowed down
/// (no smearing), and the next slice simply cuts in when it is sped up.
pub struct BeatsStretcher {
    slices: Arc<Vec<usize>>,
    cur: Option<usize>,
    head: f64,
    prev_head: f64,
    xfade: usize,
    xfade_len: usize,
    env: f32,
    env_dec: f32,
    pub pitch_ratio: f64,
}

impl BeatsStretcher {
    pub fn new(sample_rate: f32, slices: Arc<Vec<usize>>) -> Self {
        Self {
            slices,
            cur: None,
            head: 0.0,
            prev_head: 0.0,
            xfade: 0,
            xfade_len: (0.002 * sample_rate) as usize + 1,
            env: 1.0,
            env_dec: (-1.0 / (0.012 * sample_rate)).exp(),
            pitch_ratio: 1.0,
        }
    }

    pub fn set_slices(&mut self, slices: Arc<Vec<usize>>) {
        self.slices = slices;
        self.reset();
    }

    pub fn reset(&mut self) {
        self.cur = None;
        self.xfade = 0;
        self.env = 1.0;
    }

    fn slice_of(&self, pos: f64) -> usize {
        let p = pos.max(0.0) as usize;
        match self.slices.binary_search(&p) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    #[inline]
    pub fn next(&mut self, buf: &AudioBuffer, source_pos: f64) -> (f32, f32) {
        let s = self.slice_of(source_pos);
        if self.cur != Some(s) {
            // Enter the slice where the mapping says we are (handles launching mid-slice).
            self.prev_head = self.head;
            self.head = self.slices.get(s).copied().unwrap_or(0) as f64 + (source_pos - self.slices.get(s).copied().unwrap_or(0) as f64).max(0.0);
            self.xfade = if self.cur.is_some() { self.xfade_len } else { 0 };
            self.cur = Some(s);
            self.env = 1.0;
        }
        let slice_end = self.slices.get(s + 1).copied().unwrap_or(buf.len()) as f64;
        // Played past the slice end (slowed down): fade out instead of running into the next hit.
        if self.head >= slice_end {
            self.env *= self.env_dec;
        }
        let stereo = buf.channels.len() > 1;
        let read = |p: f64| {
            let l = buf.read(0, p);
            (l, if stereo { buf.read(1, p) } else { l })
        };
        let (mut l, mut r) = read(self.head);
        l *= self.env;
        r *= self.env;
        if self.xfade > 0 {
            let t = self.xfade as f32 / self.xfade_len as f32;
            let (pl, pr) = read(self.prev_head);
            l = l * (1.0 - t) + pl * t;
            r = r * (1.0 - t) + pr * t;
            self.xfade -= 1;
            self.prev_head += self.pitch_ratio;
        }
        self.head += self.pitch_ratio;
        (l, r)
    }
}

/// Phase-vocoder stretcher (frame size 2048, hop 512, Hann windows) with
/// identity phase locking around spectral peaks. Instantaneous frequency is
/// measured from two analysis frames one hop apart at the mapped source
/// position, so any position mapping (tempo changes, warp markers) works.
pub struct PvStretcher {
    n: usize,
    hop: usize,
    window: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex32>,
    a: Vec<Complex32>,
    b: Vec<Complex32>,
    syn: [Vec<Complex32>; 2],
    phase: [Vec<f32>; 2],
    mag: Vec<f32>,
    peaks: Vec<usize>,
    acc: [Vec<f32>; 2],
    acc_pos: usize,
    counter: usize,
    started: bool,
    pub pitch_ratio: f64,
}

impl PvStretcher {
    pub fn new() -> Self {
        let n = 2048;
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);
        let sl = fft.get_inplace_scratch_len().max(ifft.get_inplace_scratch_len());
        let z = vec![Complex32::new(0.0, 0.0); n];
        Self {
            n,
            hop: n / 4,
            window: (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect(),
            fft,
            ifft,
            scratch: vec![Complex32::new(0.0, 0.0); sl],
            a: z.clone(),
            b: z.clone(),
            syn: [z.clone(), z.clone()],
            phase: [vec![0.0; n / 2 + 1], vec![0.0; n / 2 + 1]],
            mag: vec![0.0; n / 2 + 1],
            peaks: Vec::with_capacity(n / 2),
            acc: [vec![0.0; n], vec![0.0; n]],
            acc_pos: 0,
            counter: 0,
            started: false,
            pitch_ratio: 1.0,
        }
    }

    pub fn reset(&mut self) {
        self.acc.iter_mut().for_each(|a| a.fill(0.0));
        self.counter = 0;
        self.started = false;
    }

    fn frame(&mut self, buf: &AudioBuffer, ch: usize, pos: f64) {
        let n = self.n;
        let hop = self.hop as f64;
        let src_ch = ch.min(buf.channels.len() - 1);
        // Analysis at pos and pos - hop.
        for i in 0..n {
            let w = self.window[i];
            self.a[i] = Complex32::new(buf.read(src_ch, pos + i as f64) * w, 0.0);
            self.b[i] = Complex32::new(buf.read(src_ch, pos - hop + i as f64) * w, 0.0);
        }
        self.fft.process_with_scratch(&mut self.a, &mut self.scratch);
        self.fft.process_with_scratch(&mut self.b, &mut self.scratch);
        let bins = n / 2 + 1;
        let tau = std::f32::consts::TAU;
        for k in 0..bins {
            self.mag[k] = self.a[k].norm();
        }
        // Peaks: local maxima over +-2 bins.
        self.peaks.clear();
        for k in 2..bins - 2 {
            let m = self.mag[k];
            if m > self.mag[k - 1] && m >= self.mag[k + 1] && m > self.mag[k - 2] && m >= self.mag[k + 2] && m > 1e-6 {
                self.peaks.push(k);
            }
        }
        let ratio = self.pitch_ratio as f32;
        let syn = &mut self.syn[ch];
        syn.iter_mut().for_each(|c| *c = Complex32::new(0.0, 0.0));
        let phase = &mut self.phase[ch];
        // Advance each peak's synthesis phase by its instantaneous frequency.
        let mut new_phase = [0.0f32; 1025];
        for &k in &self.peaks {
            let pa = self.a[k].arg();
            let pb = self.b[k].arg();
            let omega = tau * k as f32 / n as f32;
            let mut d = pa - pb - omega * hop as f32;
            d -= tau * (d / tau).round();
            let inst = omega + d / hop as f32;
            if !self.started {
                new_phase[k] = pa;
            } else {
                new_phase[k] = phase[k] + inst * hop as f32 * ratio;
            }
        }
        // Identity phase locking: each bin follows its nearest peak.
        let mut pi = 0usize;
        for k in 0..bins {
            while pi + 1 < self.peaks.len() && (self.peaks[pi + 1] as isize - k as isize).abs() < (self.peaks[pi] as isize - k as isize).abs() {
                pi += 1;
            }
            let (ph, shift) = if let Some(&p) = self.peaks.get(pi) {
                // Pitch shift: move the whole peak region so its shape is kept.
                (new_phase[p] + (self.a[k].arg() - self.a[p].arg()), ((p as f32) * ratio).round() as isize - p as isize)
            } else {
                (self.a[k].arg(), 0)
            };
            phase[k] = ph;
            let dst = k as isize + shift;
            if dst >= 0 && (dst as usize) < bins {
                let dst = dst as usize;
                let c = Complex32::from_polar(self.mag[k], ph);
                syn[dst] += c;
                if dst > 0 && dst < n / 2 {
                    syn[n - dst] += c.conj();
                }
            }
        }
        self.ifft.process_with_scratch(syn, &mut self.scratch);
        // Hann analysis * Hann synthesis at 75% overlap sums to 1.5.
        let norm = 1.0 / (n as f32 * 1.5);
        let acc = &mut self.acc[ch];
        for i in 0..n {
            acc[(self.acc_pos + i) % n] += syn[i].re * self.window[i] * norm;
        }
    }

    #[inline]
    pub fn next(&mut self, buf: &AudioBuffer, source_pos: f64) -> (f32, f32) {
        if self.counter % self.hop == 0 {
            for ch in 0..2 {
                self.frame(buf, ch, source_pos);
            }
            self.started = true;
        }
        self.counter = self.counter.wrapping_add(1);
        let i = self.acc_pos;
        let out = (self.acc[0][i], self.acc[1][i]);
        self.acc[0][i] = 0.0;
        self.acc[1][i] = 0.0;
        self.acc_pos = (self.acc_pos + 1) % self.n;
        out
    }
}

impl Default for PvStretcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rms;

    fn sine(sr: f32, hz: f32, secs: f32) -> AudioBuffer {
        let n = (sr * secs) as usize;
        AudioBuffer { sample_rate: sr, channels: vec![(0..n).map(|i| (std::f32::consts::TAU * hz * i as f32 / sr).sin()).collect()] }
    }

    fn freq(buf: &[f32], sr: f32) -> f32 {
        let z = buf.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        z as f32 / (buf.len() as f32 / sr)
    }

    fn check(rate: f64) {
        let sr = 48_000.0;
        let src = sine(sr, 220.0, 4.0);
        let mut st = GrainStretcher::new(sr, 60.0);
        let out: Vec<f32> = (0..48_000).map(|i| st.next(&src, i as f64 * rate).0).collect();
        let body = &out[4_800..];
        let f = freq(body, sr);
        assert!((f - 220.0).abs() < 3.0, "rate {rate}: pitch drifted to {f}");
        let level = rms(body);
        assert!((level - 0.707).abs() < 0.08, "rate {rate}: level {level}");
    }

    fn pv_check(rate: f64, ratio: f64, expect_hz: f32) {
        let sr = 48_000.0;
        let src = sine(sr, 220.0, 4.0);
        let mut st = PvStretcher::new();
        st.pitch_ratio = ratio;
        let out: Vec<f32> = (0..48_000).map(|i| st.next(&src, i as f64 * rate).0).collect();
        let body = &out[9_600..];
        let f = freq(body, sr);
        assert!((f - expect_hz).abs() < 4.0, "rate {rate} ratio {ratio}: {f} Hz");
        let level = rms(body);
        assert!((level - 0.707).abs() < 0.12, "rate {rate}: level {level}");
    }

    #[test]
    fn phase_vocoder_stretches_and_shifts() {
        pv_check(0.5, 1.0, 220.0);
        pv_check(1.7, 1.0, 220.0);
        pv_check(1.0, 2.0, 440.0);
    }

    fn clicks(sr: f32, every: usize, n: usize) -> AudioBuffer {
        // Short decaying noise bursts, like hi-hats.
        let mut noise = crate::util::Noise::with_seed(5);
        let ch: Vec<f32> = (0..n)
            .map(|i| {
                let t = i % every;
                if t < 2_000 { noise.next() * (-(t as f32) / 300.0).exp() } else { 0.0 }
            })
            .collect();
        AudioBuffer { sample_rate: sr, channels: vec![ch] }
    }

    #[test]
    fn transients_are_found() {
        let buf = clicks(48_000.0, 12_000, 96_000);
        let t = detect_transients(&buf, 30.0);
        assert_eq!(t.len(), 8, "{t:?}");
        for (k, &p) in t.iter().enumerate() {
            assert!((p as i64 - (k * 12_000) as i64).abs() < 300, "transient {k} at {p}");
        }
    }

    #[test]
    fn beats_mode_keeps_hits_crisp_when_slowed() {
        let sr = 48_000.0;
        let buf = clicks(sr, 12_000, 48_000);
        let slices = Arc::new(detect_transients(&buf, 30.0));
        let mut st = BeatsStretcher::new(sr, slices);
        // Half speed: hits every 24 000 samples, and silence (not smear) between.
        let out: Vec<f32> = (0..96_000).map(|i| st.next(&buf, i as f64 * 0.5).0).collect();
        for k in 0..4 {
            let hit = &out[k * 24_000..k * 24_000 + 1_500];
            let gap = &out[k * 24_000 + 13_000..k * 24_000 + 23_000];
            assert!(rms(hit) > 0.05, "hit {k} missing");
            assert!(rms(gap) < 1e-3, "slice {k} smeared into the gap: {}", rms(gap));
        }
    }

    #[test]
    fn slower_keeps_pitch_and_level() {
        check(0.5);
    }

    #[test]
    fn faster_keeps_pitch_and_level() {
        check(2.0);
    }

    #[test]
    fn odd_ratio_keeps_pitch_and_level() {
        check(1.37);
    }
}
