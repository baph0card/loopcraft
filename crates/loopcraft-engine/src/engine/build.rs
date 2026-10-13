//! Building the real-time graph from a project (control thread only).

use super::track::*;
use super::*;
use crate::clip::{EventList, RtClip};
use loopcraft_dsp::util::Noise;
use loopcraft_dsp::{SampleLoader, build_device_with};

fn build(spec: &DeviceSpec, sr: f32, loader: &dyn SampleLoader, plugins: &dyn PluginFactory) -> Option<Device> {
    build_device_with(spec, sr, loader, &|p| plugins.create(p, sr, CHUNK))
}

fn build_effects(specs: &[DeviceSpec], bypassed: &[usize], sr: f32, loader: &dyn SampleLoader, plugins: &dyn PluginFactory) -> Vec<FxSlot> {
    // Keep indices aligned with the project even if a device fails to build
    // (e.g. a missing plugin): substitute a pass-through.
    specs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let fx = build(s, sr, loader, plugins).and_then(Device::into_effect).unwrap_or_else(|| Box::new(Passthrough));
            FxSlot { fx, bypass: bypassed.contains(&i) }
        })
        .collect()
}

/// Stand-in for a device that couldn't be created.
struct Passthrough;

impl loopcraft_dsp::Effect for Passthrough {
    fn process(&mut self, _: &loopcraft_dsp::ProcessContext, _: &mut [f32], _: &mut [f32]) {}
    fn name(&self) -> &str {
        "Missing device"
    }
}

impl Core {
    pub fn from_project(project: &Project, sample_rate: f32, loader: &dyn SampleLoader, plugins: &dyn PluginFactory) -> Self {
        let p = project.clone().normalized();
        plugins.begin_build();
        let core = Self::build(&p, sample_rate, loader, plugins);
        plugins.end_build();
        core
    }

    fn build(p: &Project, sample_rate: f32, loader: &dyn SampleLoader, plugins: &dyn PluginFactory) -> Self {
        let sr = sample_rate;
        let n_ret = p.returns.len();
        let sidechain_of = |e: &DeviceSpec| -> Option<usize> {
            match e {
                DeviceSpec::Compressor(c) => c.sidechain_track,
                DeviceSpec::GlueCompressor(c) => c.sidechain_track,
                DeviceSpec::Gate(c) => c.sidechain_track,
                DeviceSpec::Vocoder(c) if c.carrier == VocoderCarrier::External => c.sidechain_track,
                _ => None,
            }
        };
        let key_sources: Vec<usize> =
            p.tracks.iter().chain(p.returns.iter()).chain(std::iter::once(&p.master)).flat_map(|t| t.effects.iter()).filter_map(sidechain_of).collect();

        let mut tracks: Vec<Track> = p
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let instrument = if t.kind == TrackKind::Midi {
                    t.instrument.as_ref().and_then(|s| build(s, sr, loader, plugins)).and_then(Device::into_instrument)
                } else {
                    None
                };
                let midi_fx = t
                    .midi_effects
                    .iter()
                    .enumerate()
                    .filter_map(|(j, s)| {
                        build(s, sr, loader, plugins).and_then(Device::into_midi_effect).map(|fx| MidiFxSlot { fx, bypass: t.midi_bypassed.contains(&j) })
                    })
                    .collect();
                let mut arrangement: Vec<ArrClip> = t
                    .arrangement
                    .iter()
                    .map(|a| ArrClip { start: a.start, offset: a.offset, end: a.end(), clip: RtClip::from_clip_with(&a.clip, loader, &p.grooves) })
                    .collect();
                arrangement.sort_by(|a, b| a.start.total_cmp(&b.start));
                Track {
                    kind: t.kind,
                    parent: t.group,
                    dest: p.destination(i),
                    to_bus: t.output != OutputRoute::Sends,
                    chan: Channel::new(build_effects(&t.effects, &t.bypassed, sr, loader, plugins), &t.mixer),
                    midi_fx,
                    instrument,
                    instrument_bypass: t.instrument_bypassed,
                    slots: if t.kind == TrackKind::Group {
                        t.slots.iter().map(|_| None).collect()
                    } else {
                        t.slots.iter().map(|s| s.as_ref().map(|c| Box::new(RtClip::from_clip_with(c, loader, &p.grooves)))).collect()
                    },
                    arrangement,
                    automation: t.automation.clone(),
                    sends: t.sends.iter().map(|db| db_to_gain(*db)).chain(std::iter::repeat(0.0)).take(n_ret).collect(),
                    solo: t.mixer.solo,
                    crossfade: t.crossfade,
                    input: t.input,
                    monitor: t.monitor,
                    armed: t.armed,
                    session: None,
                    pending: None,
                    follow_at: None,
                    follows_arrangement: true,
                    arr_active: None,
                    flush_notes: false,
                    arr_recording: false,
                    events: EventList::new(),
                    ev_b: EventList::new(),
                    live: [(false, 0, 0); 64],
                    live_len: 0,
                    capture: Box::new([(0.0, false, 0, 0); CAPTURE]),
                    capture_pos: 0,
                    capture_len: 0,
                    active_notes: [false; 128],
                    rng: Noise::with_seed(0xA5A5_0000 ^ (i as u32 + 1)),
                    stretch: Stretchers::new(sr),
                    is_key_source: key_sources.contains(&i),
                    key_l: vec![0.0; CHUNK],
                    key_r: vec![0.0; CHUNK],
                    clip_pos: 0.0,
                    out_l: vec![0.0; CHUNK],
                    out_r: vec![0.0; CHUNK],
                    in_l: vec![0.0; CHUNK],
                    in_r: vec![0.0; CHUNK],
                    has_input: false,
                }
            })
            .collect();
        if tracks.iter().any(|t| t.solo) {
            for t in tracks.iter_mut().filter(|t| !t.solo) {
                t.chan.gain_now = 0.0;
            }
        }

        // Sources before their destinations (groups and routed tracks).
        // Each track has at most one destination and routes are acyclic,
        // so ordering by hops-to-master, deepest first, is a valid order.
        let depth = |i: usize| {
            let (mut d, mut cur) = (0, p.destination(i));
            while let Some(x) = cur {
                d += 1;
                if d > p.tracks.len() {
                    break;
                }
                cur = p.destination(x);
            }
            d
        };
        let mut order: Vec<usize> = (0..tracks.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(depth(i)));

        let mut returns: Vec<Channel> = p.returns.iter().map(|r| Channel::new(build_effects(&r.effects, &r.bypassed, sr, loader, plugins), &r.mixer)).collect();
        let return_sends = p.returns.iter().map(|r| r.sends.iter().map(|db| db_to_gain(*db)).collect()).collect();
        let master = Channel::new(build_effects(&p.master.effects, &p.master.bypassed, sr, loader, plugins), &p.master.mixer);

        // ---- Delay compensation ----
        // Path latency of each track = own chain + (for groups) slowest child.
        let n = tracks.len();
        let mut path = vec![0usize; n];
        for &i in &order {
            let own = tracks[i].chan.latency;
            let child_max = (0..n).filter(|&c| tracks[c].dest == Some(i)).map(|c| path[c]).max().unwrap_or(0);
            path[i] = own + child_max;
        }
        // Align siblings inside each group and at the top level.
        let mut delays = vec![0usize; n];
        for i in 0..n {
            let siblings_max = (0..n).filter(|&c| tracks[c].dest == tracks[i].dest).map(|c| path[c]).max().unwrap_or(0);
            delays[i] = siblings_max - path[i];
        }
        for (t, d) in tracks.iter_mut().zip(delays.iter()) {
            t.chan.pdc = FixedDelay::new(*d);
        }
        let top_max = (0..n).filter(|&i| tracks[i].dest.is_none()).map(|i| path[i]).max().unwrap_or(0);
        let ret_max = returns.iter().map(|r| r.latency).max().unwrap_or(0);
        for r in returns.iter_mut() {
            r.pdc = FixedDelay::new(ret_max - r.latency);
        }
        let latency = top_max + ret_max + master.latency;

        let state = Arc::new(EngineState::new(tracks.len(), n_ret));
        state.tempo.store(p.tempo);
        state.latency.store(latency as u32, std::sync::atomic::Ordering::Relaxed);
        for (t, ts) in tracks.iter().zip(state.tracks.iter()) {
            ts.armed.store(t.armed, std::sync::atomic::Ordering::Relaxed);
        }

        let mut tempo_points = p.tempo_automation.clone();
        tempo_points.sort_by(|a, b| a.0.total_cmp(&b.0));

        Self {
            sr,
            tempo: p.tempo.clamp(20.0, 999.0),
            sig: p.time_signature,
            quant: p.quantization,
            playing: false,
            beat: 0.0,
            free_beat: 0.0,
            loop_region: p.loop_region,
            metronome: p.metronome,
            solo_rel: vec![false; tracks.len()],
            tracks,
            order,
            returns,
            return_sends,
            ret_pre: p.returns.iter().map(|r| r.pre_fader).collect(),
            master,
            bus_l: vec![0.0; CHUNK],
            bus_r: vec![0.0; CHUNK],
            bus_pdc: FixedDelay::new(ret_max),
            cue_l: vec![0.0; CHUNK],
            cue_r: vec![0.0; CHUNK],
            cue_mode: false,
            cue_gain: db_to_gain(p.cue_volume_db),
            crossfader: p.crossfader,
            scenes: p.scenes.clone(),
            pending_tempo: None,
            pending_sig: None,
            scene_follow: None,
            tempo_auto: !tempo_points.is_empty(),
            tempo_points,
            count_in: None,
            click: Click { t: 0, len: 0, freq: 1000.0, phase: 0.0, amp: 0.0 },
            recordings: Vec::with_capacity(64),
            prev_session: vec![None; n],
            state,
            outbox: Vec::with_capacity(128),
            input: vec![vec![0.0; MAX_HOST_FRAMES]; MAX_INPUTS],
            input_frames: 0,
            input_channels: 0,
            master_prev_l: vec![0.0; CHUNK],
            master_prev_r: vec![0.0; CHUNK],
            latency,
            rng: Noise::with_seed(0x5eed_f011),
        }
    }

    /// Move transport, playing clips and recordings from `self` (the old
    /// graph) into `new`, so structural edits never interrupt playback.
    pub(crate) fn carry_over_into(&mut self, new: &mut Core) {
        new.beat = self.beat;
        new.free_beat = self.free_beat;
        new.playing = self.playing;
        new.tempo = self.tempo;
        new.count_in = self.count_in;
        new.pending_tempo = self.pending_tempo;
        new.scene_follow = self.scene_follow;
        new.cue_mode = self.cue_mode;
        new.tempo_auto = self.tempo_auto && !new.tempo_points.is_empty();
        for (n, o) in new.tracks.iter_mut().zip(self.tracks.iter_mut()) {
            n.follows_arrangement = o.follows_arrangement;
            n.session = o.session.filter(|s| n.slots.get(s.slot).is_some_and(|c| c.is_some()));
            n.pending = o.pending;
            n.follow_at = if n.session.is_some() { o.follow_at } else { None };
            n.arr_recording = o.arr_recording;
            // Capture history survives graph rebuilds.
            std::mem::swap(&mut n.capture, &mut o.capture);
            n.capture_pos = o.capture_pos;
            n.capture_len = o.capture_len;
        }
        for (i, p) in new.prev_session.iter_mut().enumerate() {
            *p = self.prev_session.get(i).copied().flatten();
        }
        // Recordings keep going in the new graph (their buffers move over).
        while let Some(r) = self.recordings.pop() {
            if new.recordings.len() < new.recordings.capacity() {
                new.recordings.push(r);
            }
        }
        // Restore Beats-mode slices for clips that keep playing.
        for t in new.tracks.iter_mut() {
            if let Some(s) = t.session
                && let Some(Some(c)) = t.slots.get(s.slot)
                && let crate::clip::RtContent::Audio(a) = &c.content
            {
                t.stretch.beats.set_slices(a.slices.clone());
            }
        }
    }
}
