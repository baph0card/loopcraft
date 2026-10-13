//! Multi-sample instrument: key- and velocity-zoned samples with loops,
//! a filter and an amp envelope per voice.

use crate::util::{Envelope, Svf};
use crate::{AudioBuffer, Instrument, ParamInfo, ProcessContext, SampleLoader};
use loopcraft_core::*;
use std::sync::Arc;

struct Zone {
    spec: SampleZone,
    buf: Option<Arc<AudioBuffer>>,
    gain: f32,
}

#[derive(Clone)]
struct Voice {
    zone: usize,
    note: u8,
    vel: f32,
    pos: f64,
    rate: f64,
    target_rate: f64,
    env: Envelope,
    filt: [Svf; 2],
    age: u64,
    held: bool,
}

pub(crate) const PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Cutoff", 20.0, 20_000.0, "Hz").at("cutoff_hz"),
    ParamInfo::new("Resonance", 0.0, 0.95, "").at("resonance"),
    ParamInfo::new("Attack", 0.001, 2.0, "s").at("amp_env/attack"),
    ParamInfo::new("Release", 0.005, 6.0, "s").at("amp_env/release"),
    ParamInfo::new("Volume", -40.0, 6.0, "dB").at("gain_db"),
];

pub struct MultiSampler {
    p: MultiSamplerParams,
    sr: f32,
    zones: Vec<Zone>,
    voices: Vec<Voice>,
    clock: u64,
    gain: f32,
    last_rate: f64,
}

impl MultiSampler {
    pub fn new(p: MultiSamplerParams, sr: f32, loader: &dyn SampleLoader) -> Self {
        let zones = p.zones.iter().map(|z| Zone { buf: loader.load(&z.file), gain: db_to_gain(z.gain_db), spec: z.clone() }).collect();
        let v = Voice {
            zone: 0,
            note: 0,
            vel: 0.0,
            pos: 0.0,
            rate: 1.0,
            target_rate: 1.0,
            env: Envelope::new(p.amp_env, sr),
            filt: [Svf::default(); 2],
            age: 0,
            held: false,
        };
        Self { voices: vec![v; p.voices.clamp(1, 128)], zones, gain: db_to_gain(p.gain_db), p, sr, clock: 0, last_rate: 0.0 }
    }

    /// Construct with already-decoded buffers (one per zone), for tests.
    pub fn with_buffers(p: MultiSamplerParams, sr: f32, bufs: Vec<Arc<AudioBuffer>>) -> Self {
        struct Fixed(Vec<Arc<AudioBuffer>>, Vec<String>);
        impl SampleLoader for Fixed {
            fn load(&self, path: &str) -> Option<Arc<AudioBuffer>> {
                self.1.iter().position(|f| f == path).and_then(|i| self.0.get(i).cloned())
            }
        }
        let names = p.zones.iter().map(|z| z.file.clone()).collect();
        Self::new(p, sr, &Fixed(bufs, names))
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.env.is_active()).count()
    }

    fn alloc(&mut self) -> usize {
        self.voices
            .iter()
            .position(|v| !v.env.is_active())
            .unwrap_or_else(|| self.voices.iter().enumerate().min_by_key(|(_, v)| (!v.env.is_releasing(), v.age)).map(|(i, _)| i).unwrap_or(0))
    }
}

impl Instrument for MultiSampler {
    fn note_on(&mut self, note: u8, velocity: u8) {
        let n_zones = self.zones.len();
        for z in 0..n_zones {
            let s = &self.zones[z].spec;
            let in_key = (s.key_range.0..=s.key_range.1).contains(&note);
            let in_vel = (s.velocity_range.0..=s.velocity_range.1).contains(&velocity);
            if !(in_key && in_vel) || self.zones[z].buf.is_none() {
                continue;
            }
            let rate = 2f64.powf((note as f64 - s.root_key as f64) / 12.0 + s.tune_cents as f64 / 1200.0);
            let i = self.alloc();
            self.clock += 1;
            let glide = self.p.glide > 0.0 && self.last_rate > 0.0;
            let v = &mut self.voices[i];
            v.zone = z;
            v.note = note;
            v.vel = velocity as f32 / 127.0;
            v.pos = 0.0;
            v.target_rate = rate;
            v.rate = if glide { self.last_rate } else { rate };
            v.age = self.clock;
            v.held = true;
            v.filt = [Svf::default(); 2];
            v.env.set_params(self.p.amp_env);
            v.env.gate_on();
            self.last_rate = rate;
        }
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.note == note && v.held) {
            v.held = false;
            v.env.gate_off();
        }
    }

    fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut() {
            v.held = false;
            v.env.kill();
        }
    }

    fn render(&mut self, _ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sr;
        let filter_on = self.p.cutoff_hz < 19_500.0 || self.p.resonance > 0.01;
        let glide_c = if self.p.glide > 0.0 { (-1.0 / (self.p.glide as f64 * sr as f64)).exp() } else { 0.0 };
        for v in self.voices.iter_mut().filter(|v| v.env.is_active()) {
            let zone = &self.zones[v.zone];
            let Some(buf) = zone.buf.as_deref() else { continue };
            let len = buf.len() as f64;
            let fsr = buf.sample_rate as f64;
            let lp = zone.spec.loop_region.map(|(a, b)| ((a * fsr).max(0.0), (b * fsr).min(len - 1.0))).filter(|(a, b)| b - a > 16.0);
            let stereo = buf.channels.len() > 1;
            if filter_on {
                v.filt[0].set(self.p.cutoff_hz, self.p.resonance, sr);
                v.filt[1].set(self.p.cutoff_hz, self.p.resonance, sr);
            }
            for n in 0..left.len() {
                v.rate = v.target_rate + (v.rate - v.target_rate) * glide_c;
                if let Some((a, b)) = lp {
                    if v.pos >= b {
                        v.pos = a + (v.pos - b) % (b - a);
                    }
                } else if v.pos >= len - 1.0 {
                    v.env.kill();
                }
                let mut l = buf.read(0, v.pos);
                let mut r = if stereo { buf.read(1, v.pos) } else { l };
                if filter_on {
                    l = v.filt[0].process(l, self.p.filter_mode);
                    r = v.filt[1].process(r, self.p.filter_mode);
                }
                let a = v.env.next() * v.vel * zone.gain * self.gain;
                left[n] += l * a;
                right[n] += r * a;
                v.pos += v.rate * fsr / sr as f64;
                if !v.env.is_active() {
                    break;
                }
            }
        }
    }

    fn params(&self) -> &'static [ParamInfo] {
        PARAMS
    }

    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.cutoff_hz = v,
            1 => self.p.resonance = v,
            2 => self.p.amp_env.attack = v,
            3 => self.p.amp_env.release = v,
            4 => {
                self.p.gain_db = v;
                self.gain = db_to_gain(v);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Multi Sampler"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rms;

    fn tone(hz: f32, secs: f32) -> Arc<AudioBuffer> {
        let sr = 48_000.0;
        Arc::new(AudioBuffer {
            sample_rate: sr,
            channels: vec![(0..(sr * secs) as usize).map(|i| (std::f32::consts::TAU * hz * i as f32 / sr).sin() * 0.5).collect()],
        })
    }

    fn freq(buf: &[f32]) -> f32 {
        buf.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32 / (buf.len() as f32 / 48_000.0)
    }

    #[test]
    fn zones_pick_by_key_and_velocity_and_transpose() {
        let p = MultiSamplerParams {
            zones: vec![
                SampleZone { file: "low".into(), root_key: 48, key_range: (0, 59), ..Default::default() },
                SampleZone { file: "soft".into(), root_key: 60, key_range: (60, 127), velocity_range: (1, 80), ..Default::default() },
                SampleZone { file: "loud".into(), root_key: 60, key_range: (60, 127), velocity_range: (81, 127), ..Default::default() },
            ],
            ..Default::default()
        };
        let mut s = MultiSampler::with_buffers(p, 48_000.0, vec![tone(130.81, 1.0), tone(261.63, 1.0), tone(523.25, 1.0)]);
        let ctx = ProcessContext::new(48_000.0, 120.0);
        let render = |s: &mut MultiSampler| {
            let (mut l, mut r) = (vec![0.0; 24_000], vec![0.0; 24_000]);
            s.render(&ctx, &mut l, &mut r);
            l
        };
        s.note_on(60, 60);
        assert!((freq(&render(&mut s)) - 261.6).abs() < 6.0, "soft layer");
        s.all_notes_off();
        render(&mut s);
        s.note_on(60, 120);
        assert!((freq(&render(&mut s)) - 523.3).abs() < 8.0, "loud layer");
        s.all_notes_off();
        render(&mut s);
        // Low zone (root 48) played a fifth up.
        s.note_on(55, 100);
        let l = render(&mut s);
        let expect = 130.81 * 2f32.powf(7.0 / 12.0);
        assert!((freq(&l) - expect).abs() < 6.0, "{} vs {expect}", freq(&l));
    }

    #[test]
    fn looped_zone_sustains() {
        let p = MultiSamplerParams { zones: vec![SampleZone { file: "a".into(), loop_region: Some((0.1, 0.2)), ..Default::default() }], ..Default::default() };
        let mut s = MultiSampler::with_buffers(p, 48_000.0, vec![tone(440.0, 0.25)]);
        s.note_on(60, 100);
        let (mut l, mut r) = (vec![0.0; 96_000], vec![0.0; 96_000]);
        s.render(&ProcessContext::new(48_000.0, 120.0), &mut l, &mut r);
        assert!(rms(&l[90_000..]) > 0.1, "loop stopped");
    }
}
