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
//! Transient-aware ("beats") and phase-vocoder ("complex") modes are on the
//! roadmap.

use crate::AudioBuffer;

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
        if self.counter % self.hop == 0 {
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
