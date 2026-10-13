//! Zero-latency convolution reverb.
//!
//! The first `B` taps of the impulse response run as a direct FIR; the rest
//! use uniformly partitioned FFT convolution (overlap-save) with partition
//! size `B`. The FFT path inherently has `B` samples of latency, which lines
//! up exactly with the tail starting at tap `B`, so the whole thing is
//! latency-free.

use super::mix;
use crate::util::{Biquad, BiquadKind, Noise};
use crate::{AudioBuffer, Effect, ParamInfo, ProcessContext};
use loopcraft_core::ConvolutionParams;
use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

const B: usize = 512;
const MAX_IR_SECONDS: f32 = 10.0;

pub(crate) const PARAMS: &[ParamInfo] = &[ParamInfo::new("Dry/Wet", 0.0, 1.0, "").at("dry_wet")];

/// Synthesize a stereo hall-like impulse response: decorrelated noise with
/// an exponential decay, darker as it decays, plus a few early reflections.
pub fn synth_ir(decay_s: f32, sr: f32) -> AudioBuffer {
    let len = ((decay_s.clamp(0.1, MAX_IR_SECONDS) * 1.2) * sr) as usize;
    let mut chans = vec![vec![0.0f32; len]; 2];
    for (c, ch) in chans.iter_mut().enumerate() {
        let mut noise = Noise::with_seed(0x1357_9bdf ^ (c as u32 * 7_919 + 1));
        let mut lp = 0.0f32;
        for (i, s) in ch.iter_mut().enumerate() {
            let t = i as f32 / sr;
            let env = (-6.9 * t / decay_s.max(0.05)).exp();
            // Lowpass coefficient closes over time: high frequencies die first.
            let a = 0.15 + 0.8 * (t / decay_s.max(0.05)).min(1.0);
            lp = lp + (noise.next() - lp) * (1.0 - a);
            *s = lp * env;
        }
        for (k, (ms, g)) in [(7.0f32, 0.5f32), (13.0, 0.35), (19.0, 0.3), (29.0, 0.22)].into_iter().enumerate() {
            let i = ((ms + c as f32 * 1.7 + k as f32 * 0.3) * 0.001 * sr) as usize;
            if i < len {
                ch[i] += g * if (k + c) % 2 == 0 { 1.0 } else { -1.0 };
            }
        }
        // Normalize energy so wet level is comparable across decay times.
        let e: f32 = ch.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        ch.iter_mut().for_each(|x| *x /= e * 1.2);
    }
    AudioBuffer { sample_rate: sr, channels: chans }
}

struct Channel {
    head: Vec<f32>, // first B taps, reversed for the FIR
    hist: Vec<f32>, // input history for the FIR (ring, 2B)
    hpos: usize,
    parts: Vec<Vec<Complex32>>, // spectra of tail partitions
    fdl: Vec<Vec<Complex32>>,   // frequency-domain delay line
    fdl_pos: usize,
    in_block: Vec<f32>, // last 2B input samples (overlap-save)
    out_block: Vec<f32>,
    acc: Vec<Complex32>,
    buf: Vec<Complex32>,
}

pub struct Convolution {
    p: ConvolutionParams,
    ch: [Channel; 2],
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex32>,
    fill: usize,
    predelay: Vec<[f32; 2]>,
    pd_pos: usize,
    pd_len: usize,
    lo: [Biquad; 2],
    hi: [Biquad; 2],
}

impl Convolution {
    pub fn new(p: ConvolutionParams, ir: Option<Arc<AudioBuffer>>, sr: f32) -> Self {
        let ir = match ir {
            Some(b) => (*b).clone(),
            None => synth_ir(p.decay_s, sr),
        };
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(2 * B);
        let ifft = planner.plan_fft_inverse(2 * B);
        let scratch_len = fft.get_inplace_scratch_len().max(ifft.get_inplace_scratch_len());
        let max_len = (MAX_IR_SECONDS * sr) as usize;
        let make = |c: usize| {
            let src = &ir.channels[c.min(ir.channels.len() - 1)];
            let src = &src[..src.len().min(max_len)];
            let mut head = vec![0.0; B];
            for (i, h) in head.iter_mut().enumerate() {
                *h = src.get(i).copied().unwrap_or(0.0);
            }
            let tail = if src.len() > B { &src[B..] } else { &[][..] };
            let n_parts = tail.len().div_ceil(B);
            let parts: Vec<Vec<Complex32>> = (0..n_parts)
                .map(|k| {
                    let mut v = vec![Complex32::new(0.0, 0.0); 2 * B];
                    for i in 0..B {
                        if let Some(&x) = tail.get(k * B + i) {
                            v[i] = Complex32::new(x, 0.0);
                        }
                    }
                    fft.process(&mut v);
                    v
                })
                .collect();
            Channel {
                head,
                hist: vec![0.0; 2 * B],
                hpos: 0,
                fdl: vec![vec![Complex32::new(0.0, 0.0); 2 * B]; n_parts.max(1)],
                parts,
                fdl_pos: 0,
                in_block: vec![0.0; 2 * B],
                out_block: vec![0.0; B],
                acc: vec![Complex32::new(0.0, 0.0); 2 * B],
                buf: vec![Complex32::new(0.0, 0.0); 2 * B],
            }
        };
        let pd_len = ((p.predelay_ms.clamp(0.0, 250.0) * 0.001 * sr) as usize).max(1);
        let mut lo = [Biquad::default(); 2];
        let mut hi = [Biquad::default(); 2];
        for b in lo.iter_mut() {
            b.design(BiquadKind::HighPass, p.low_cut_hz.max(10.0), 0.707, 0.0, sr);
        }
        for b in hi.iter_mut() {
            b.design(BiquadKind::LowPass, p.high_cut_hz.min(sr * 0.45), 0.707, 0.0, sr);
        }
        Self {
            ch: [make(0), make(1)],
            fft,
            ifft,
            scratch: vec![Complex32::new(0.0, 0.0); scratch_len],
            fill: 0,
            predelay: vec![[0.0; 2]; (0.25 * sr) as usize + 2],
            pd_pos: 0,
            pd_len,
            lo,
            hi,
            p,
        }
    }

    /// Run one B-sample block of the FFT tail for one channel.
    fn tail_block(fft: &dyn Fft<f32>, ifft: &dyn Fft<f32>, scratch: &mut [Complex32], c: &mut Channel) {
        if c.parts.is_empty() {
            c.out_block.fill(0.0);
            return;
        }
        for (b, &x) in c.buf.iter_mut().zip(c.in_block.iter()) {
            *b = Complex32::new(x, 0.0);
        }
        fft.process_with_scratch(&mut c.buf, scratch);
        let n = c.fdl.len();
        c.fdl_pos = (c.fdl_pos + n - 1) % n;
        c.fdl[c.fdl_pos].copy_from_slice(&c.buf);
        c.acc.iter_mut().for_each(|a| *a = Complex32::new(0.0, 0.0));
        for (k, part) in c.parts.iter().enumerate() {
            let spec = &c.fdl[(c.fdl_pos + k) % n];
            for ((a, s), h) in c.acc.iter_mut().zip(spec.iter()).zip(part.iter()) {
                *a += s * h;
            }
        }
        ifft.process_with_scratch(&mut c.acc, scratch);
        let norm = 1.0 / (2 * B) as f32;
        for i in 0..B {
            c.out_block[i] = c.acc[B + i].re * norm;
        }
    }
}

impl Effect for Convolution {
    fn process(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        for n in 0..left.len() {
            let dry = [left[n], right[n]];
            // Predelay.
            let pl = self.predelay.len();
            self.predelay[self.pd_pos] = dry;
            let rd = (self.pd_pos + pl - self.pd_len) % pl;
            let x = self.predelay[rd];
            self.pd_pos = (self.pd_pos + 1) % pl;

            let mut wet = [0.0f32; 2];
            for (c, ch) in self.ch.iter_mut().enumerate() {
                // FIR head.
                let h2 = ch.hist.len();
                ch.hist[ch.hpos] = x[c];
                let mut y = 0.0;
                for (k, &h) in ch.head.iter().enumerate() {
                    y += h * ch.hist[(ch.hpos + h2 - k) % h2];
                }
                ch.hpos = (ch.hpos + 1) % h2;
                // FFT tail output (computed at the last block boundary).
                y += ch.out_block[self.fill];
                ch.in_block[B + self.fill] = x[c];
                wet[c] = y;
            }
            self.fill += 1;
            if self.fill == B {
                self.fill = 0;
                for c in 0..2 {
                    Self::tail_block(self.fft.as_ref(), self.ifft.as_ref(), &mut self.scratch, &mut self.ch[c]);
                    let ch = &mut self.ch[c];
                    ch.in_block.copy_within(B.., 0);
                }
            }
            let wl = self.hi[0].process(self.lo[0].process(wet[0]));
            let wr = self.hi[1].process(self.lo[1].process(wet[1]));
            left[n] = mix(dry[0], wl, self.p.dry_wet);
            right[n] = mix(dry[1], wr, self.p.dry_wet);
        }
    }
    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        if i == 0 {
            self.p.dry_wet = v;
        }
    }
    fn name(&self) -> &str {
        "Convolution Reverb"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The partitioned convolution must match direct convolution exactly.
    #[test]
    fn matches_direct_convolution() {
        let sr = 8_000.0;
        let ir_len = 3 * B + 77;
        let mut noise = Noise::with_seed(42);
        let ir: Vec<f32> = (0..ir_len).map(|i| noise.next() * (-(i as f32) / 600.0).exp()).collect();
        let buf = Arc::new(AudioBuffer { sample_rate: sr, channels: vec![ir.clone()] });
        let p = ConvolutionParams { predelay_ms: 0.0, low_cut_hz: 10.0, high_cut_hz: 100_000.0, dry_wet: 1.0, ..Default::default() };
        let mut cv = Convolution::new(p, Some(buf), sr);
        // Bypass the tone filters for an exact comparison.
        cv.lo = [Biquad::default(); 2];
        cv.hi = [Biquad::default(); 2];
        let n = 6 * B;
        let input: Vec<f32> = (0..n).map(|_| noise.next()).collect();
        let (mut l, mut r) = (input.clone(), input.clone());
        let ctx = ProcessContext::new(sr, 120.0);
        for (a, b) in l.chunks_mut(100).zip(r.chunks_mut(100)) {
            cv.process(&ctx, a, b);
        }
        // Predelay of 1 sample (minimum) shifts everything by one.
        for t in 1..n {
            let mut y = 0.0;
            for k in 0..ir_len.min(t) {
                y += ir[k] * input[t - 1 - k];
            }
            assert!((l[t] - y).abs() < 2e-3, "t={t}: {} vs {y}", l[t]);
        }
    }

    #[test]
    fn synthesized_ir_decays() {
        let ir = synth_ir(1.0, 48_000.0);
        let e = |a: usize, b: usize| ir.channels[0][a..b].iter().map(|x| x * x).sum::<f32>();
        assert!(e(0, 4_800) > 20.0 * e(43_200, 48_000));
    }
}
