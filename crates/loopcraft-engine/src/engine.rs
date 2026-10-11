//! The real-time audio graph.
//!
//! Threading model: the UI owns an [`EngineHandle`] and sends [`Command`]s
//! through a wait-free ring buffer; the audio callback owns the [`Engine`]
//! and calls [`Engine::process`]. Everything that allocates (loading files,
//! building devices, building clips) happens on the control side; replaced
//! objects are shipped back as garbage so the audio thread never frees
//! memory either.
//!
//! Each host buffer is cut into chunks of at most `CHUNK` frames, and chunks
//! are additionally split exactly at musical boundaries (quantized launches,
//! scene tempo changes, arrangement clip edges, loop end), so launches are
//! sample-accurate rather than block-accurate.

use crate::clip::{ClipStatus, EventList, MidiEvent, Placement, RtClip, RtContent, audio_source_pos, schedule_midi};
use crate::command::{Command, DeviceRef, EngineEvent, Garbage, RecordBuffer, TrackRef};
use crate::state::{EngineState, NONE};
use loopcraft_core::*;
use loopcraft_dsp::stretch::GrainStretcher;
use loopcraft_dsp::util::Noise;
use loopcraft_dsp::{Device, Effect, Instrument, ProcessContext, SampleLoader, build_device};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

/// Largest chunk rendered in one go. Also the size of every scratch buffer.
pub const CHUNK: usize = 256;
const EPS: f64 = 1e-9;

/// Creates third-party plugin devices. Implemented by `loopcraft-plugin`.
pub trait PluginFactory {
    fn create(&self, plugin: &PluginRef, sample_rate: f32, max_block: usize) -> Option<Device>;
}

pub struct NoPlugins;

impl PluginFactory for NoPlugins {
    fn create(&self, _: &PluginRef, _: f32, _: usize) -> Option<Device> {
        None
    }
}

struct FxSlot {
    fx: Box<dyn Effect>,
    bypass: bool,
}

/// Fader, pan and insert chain shared by every kind of channel.
struct Channel {
    effects: Vec<FxSlot>,
    gain_target: f32,
    gain_now: f32,
    pan: f32,
    mute: bool,
    l: Vec<f32>,
    r: Vec<f32>,
}

impl Channel {
    fn new(effects: Vec<FxSlot>, mixer: &MixerSettings) -> Self {
        let g = db_to_gain(mixer.volume_db);
        Self { effects, gain_target: g, gain_now: g, pan: mixer.pan, mute: mixer.mute, l: vec![0.0; CHUNK], r: vec![0.0; CHUNK] }
    }

    fn clear(&mut self, n: usize) {
        self.l[..n].fill(0.0);
        self.r[..n].fill(0.0);
    }

    fn run_effects(&mut self, ctx: &ProcessContext, n: usize) {
        for slot in self.effects.iter_mut().filter(|s| !s.bypass) {
            slot.fx.process(ctx, &mut self.l[..n], &mut self.r[..n]);
        }
    }

    /// Apply fader (ramped across the chunk) and balance-style pan; returns peaks.
    fn apply_fader(&mut self, n: usize, silent: bool) -> (f32, f32) {
        let target = if silent || self.mute { 0.0 } else { self.gain_target };
        let start = self.gain_now;
        let step = (target - start) / n as f32;
        let (pl, pr) = (if self.pan > 0.0 { 1.0 - self.pan } else { 1.0 }, if self.pan < 0.0 { 1.0 + self.pan } else { 1.0 });
        let (mut pk_l, mut pk_r) = (0.0f32, 0.0f32);
        for i in 0..n {
            let g = start + step * (i + 1) as f32;
            self.l[i] *= g * pl;
            self.r[i] *= g * pr;
            pk_l = pk_l.max(self.l[i].abs());
            pk_r = pk_r.max(self.r[i].abs());
        }
        self.gain_now = target;
        (pk_l, pk_r)
    }

    fn effect_mut(&mut self, i: usize) -> Option<&mut FxSlot> {
        self.effects.get_mut(i)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PendingKind {
    Launch(usize),
    Stop,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    kind: PendingKind,
    at: Beats,
}

#[derive(Clone, Copy, Debug)]
struct SessionPlay {
    slot: usize,
    start: Beats,
}

struct ArrClip {
    start: Beats,
    end: Beats,
    clip: RtClip,
}

struct Track {
    #[allow(dead_code)] // kept for upcoming audio-input / monitoring support
    kind: TrackKind,
    chan: Channel,
    instrument: Option<Box<dyn Instrument>>,
    instrument_bypass: bool,
    slots: Vec<Option<Box<RtClip>>>,
    arrangement: Vec<ArrClip>,
    automation: Vec<AutomationLane>,
    sends: Vec<f32>,
    solo: bool,

    session: Option<SessionPlay>,
    pending: Option<Pending>,
    follows_arrangement: bool,
    arr_active: Option<usize>,
    flush_notes: bool,

    events: EventList,
    live: [(bool, u8, u8); 64],
    live_len: usize,
    active_notes: [bool; 128],
    rng: Noise,
    stretcher: GrainStretcher,
    /// Pre-effects copy for tracks that key another track's sidechain.
    is_key_source: bool,
    key_l: Vec<f32>,
    key_r: Vec<f32>,
    clip_pos: f64,
}

struct Click {
    t: usize,
    len: usize,
    freq: f32,
    phase: f32,
    amp: f32,
}

struct Recording {
    buf: Box<RecordBuffer>,
    start: Beats,
}

/// The audio graph and transport. Built off the audio thread from a
/// [`Project`]; moved into the audio callback via [`Engine`].
pub struct Core {
    sr: f32,
    tempo: f64,
    sig: TimeSignature,
    quant: Quantization,
    playing: bool,
    beat: Beats,
    loop_region: Option<(Beats, Beats)>,
    metronome: bool,
    tracks: Vec<Track>,
    returns: Vec<Channel>,
    master: Channel,
    scenes: Vec<Scene>,
    pending_tempo: Option<(Beats, f64)>,
    click: Click,
    recording: Option<Recording>,
    state: Arc<EngineState>,
    /// Events produced inside a chunk, flushed to the event ring afterwards.
    /// Preallocated; never grows on the audio thread.
    outbox: Vec<EngineEvent>,
}

fn build_effects(specs: &[DeviceSpec], sr: f32, loader: &dyn SampleLoader, plugins: &dyn PluginFactory) -> Vec<FxSlot> {
    specs
        .iter()
        .filter_map(|s| {
            let dev = match s {
                DeviceSpec::Plugin(p) => plugins.create(p, sr, CHUNK),
                other => build_device(other, sr, loader),
            };
            match dev {
                Some(Device::Effect(fx)) => Some(FxSlot { fx, bypass: false }),
                _ => None,
            }
        })
        .collect()
}

impl Core {
    pub fn from_project(project: &Project, sample_rate: f32, loader: &dyn SampleLoader, plugins: &dyn PluginFactory) -> Self {
        let p = project.clone().normalized();
        let n_ret = p.returns.len();
        let key_sources: Vec<usize> =
            p.tracks.iter().flat_map(|t| t.effects.iter()).filter_map(|e| if let DeviceSpec::Compressor(c) = e { c.sidechain_track } else { None }).collect();

        let tracks = p
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let instrument = t.instrument.as_ref().and_then(|spec| {
                    let dev = match spec {
                        DeviceSpec::Plugin(pr) => plugins.create(pr, sample_rate, CHUNK),
                        other => build_device(other, sample_rate, loader),
                    };
                    match dev {
                        Some(Device::Instrument(inst)) => Some(inst),
                        _ => None,
                    }
                });
                let mut arrangement: Vec<ArrClip> =
                    t.arrangement.iter().map(|a| ArrClip { start: a.start, end: a.end(), clip: RtClip::from_clip(&a.clip, loader) }).collect();
                arrangement.sort_by(|a, b| a.start.total_cmp(&b.start));
                Track {
                    kind: t.kind,
                    chan: Channel::new(build_effects(&t.effects, sample_rate, loader, plugins), &t.mixer),
                    instrument,
                    instrument_bypass: false,
                    slots: t.slots.iter().map(|s| s.as_ref().map(|c| Box::new(RtClip::from_clip(c, loader)))).collect(),
                    arrangement,
                    automation: t.automation.clone(),
                    sends: t.sends.iter().map(|db| db_to_gain(*db)).chain(std::iter::repeat(0.0)).take(n_ret).collect(),
                    solo: t.mixer.solo,
                    session: None,
                    pending: None,
                    follows_arrangement: true,
                    arr_active: None,
                    flush_notes: false,
                    events: EventList::new(),
                    live: [(false, 0, 0); 64],
                    live_len: 0,
                    active_notes: [false; 128],
                    rng: Noise::with_seed(0xA5A5_0000 ^ (i as u32 + 1)),
                    stretcher: GrainStretcher::new(sample_rate, 60.0),
                    is_key_source: key_sources.contains(&i),
                    key_l: vec![0.0; CHUNK],
                    key_r: vec![0.0; CHUNK],
                    clip_pos: 0.0,
                }
            })
            .collect::<Vec<_>>();

        let returns = p.returns.iter().map(|r| Channel::new(build_effects(&r.effects, sample_rate, loader, plugins), &r.mixer)).collect();
        let master = Channel::new(build_effects(&p.master.effects, sample_rate, loader, plugins), &p.master.mixer);
        let state = Arc::new(EngineState::new(tracks.len(), n_ret));
        state.tempo.store(p.tempo);

        Self {
            sr: sample_rate,
            tempo: p.tempo.clamp(20.0, 999.0),
            sig: p.time_signature,
            quant: p.quantization,
            playing: false,
            beat: 0.0,
            loop_region: p.loop_region,
            metronome: p.metronome,
            tracks,
            returns,
            master,
            scenes: p.scenes.clone(),
            pending_tempo: None,
            click: Click { t: 0, len: 0, freq: 1000.0, phase: 0.0, amp: 0.0 },
            recording: None,
            state,
            outbox: Vec::with_capacity(64),
        }
    }

    pub fn state(&self) -> Arc<EngineState> {
        self.state.clone()
    }

    pub fn sample_rate(&self) -> f32 {
        self.sr
    }

    pub fn beat(&self) -> Beats {
        self.beat
    }

    pub fn tempo(&self) -> f64 {
        self.tempo
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn playing_slot(&self, track: usize) -> Option<usize> {
        self.tracks.get(track)?.session.map(|s| s.slot)
    }

    fn beats_per_frame(&self) -> f64 {
        self.tempo / 60.0 / self.sr as f64
    }

    fn launch_time(&self, q: Option<Quantization>) -> Beats {
        if !self.playing {
            return self.beat;
        }
        q.unwrap_or(self.quant).next_boundary(self.beat, self.sig)
    }

    fn ensure_playing(&mut self) {
        if !self.playing {
            self.playing = true;
        }
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    pub fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Play => self.playing = true,
            Command::Stop => self.stop_transport(),
            Command::Seek(b) => {
                self.beat = b.max(0.0);
                for t in self.tracks.iter_mut() {
                    t.flush_notes = true;
                    t.arr_active = None;
                    t.stretcher.reset();
                }
            }
            Command::SetTempo(bpm) => self.tempo = bpm.clamp(20.0, 999.0),
            Command::SetQuantization(q) => self.quant = q,
            Command::SetLoop(r) => self.loop_region = r.filter(|(a, b)| b > a),
            Command::SetMetronome(on) => self.metronome = on,
            Command::LaunchClip { track, slot } => self.launch_clip(track, slot),
            Command::LaunchClipAt { track, slot, at } => {
                if let Some(t) = self.tracks.get_mut(track)
                    && t.slots.get(slot).is_some_and(|c| c.is_some())
                {
                    t.pending = Some(Pending { kind: PendingKind::Launch(slot), at });
                    self.playing = true;
                }
            }
            Command::ReleaseClip { track, slot } => {
                if let Some(t) = self.tracks.get(track) {
                    let gate = t.slots.get(slot).and_then(|c| c.as_ref()).map(|c| c.launch_mode == LaunchMode::Gate).unwrap_or(false);
                    if gate && t.session.map(|s| s.slot) == Some(slot) {
                        let at = self.launch_time(None);
                        self.tracks[track].pending = Some(Pending { kind: PendingKind::Stop, at });
                    }
                }
            }
            Command::StopTrack { track } => {
                let at = self.launch_time(None);
                if let Some(t) = self.tracks.get_mut(track)
                    && (t.session.is_some() || t.pending.is_some())
                {
                    t.pending = Some(Pending { kind: PendingKind::Stop, at });
                }
            }
            Command::LaunchScene { scene } => self.launch_scene(scene),
            Command::StopAllClips => {
                let at = self.launch_time(None);
                for t in self.tracks.iter_mut().filter(|t| t.session.is_some() || t.pending.is_some()) {
                    t.pending = Some(Pending { kind: PendingKind::Stop, at });
                }
            }
            Command::BackToArrangement => {
                for t in self.tracks.iter_mut() {
                    if t.session.is_some() {
                        t.flush_notes = true;
                    }
                    t.session = None;
                    t.pending = None;
                    t.follows_arrangement = true;
                    t.arr_active = None;
                }
            }
            Command::SetVolume { channel, db } => {
                if let Some(c) = self.channel_mut(channel) {
                    c.gain_target = db_to_gain(db);
                }
            }
            Command::SetPan { channel, pan } => {
                if let Some(c) = self.channel_mut(channel) {
                    c.pan = pan.clamp(-1.0, 1.0);
                }
            }
            Command::SetMute { channel, mute } => {
                if let Some(c) = self.channel_mut(channel) {
                    c.mute = mute;
                }
            }
            Command::SetSolo { track, solo } => {
                if let Some(t) = self.tracks.get_mut(track) {
                    t.solo = solo;
                }
            }
            Command::SetSend { track, ret, db } => {
                if let Some(s) = self.tracks.get_mut(track).and_then(|t| t.sends.get_mut(ret)) {
                    *s = db_to_gain(db);
                }
            }
            Command::SetDeviceParam { channel, device, param, value } => self.set_device_param(channel, device, param, value),
            Command::SetDeviceBypass { channel, device, bypass } => match (channel, device) {
                (TrackRef::Track(i), DeviceRef::Instrument) => {
                    if let Some(t) = self.tracks.get_mut(i) {
                        t.instrument_bypass = bypass;
                        if let Some(inst) = t.instrument.as_mut() {
                            inst.all_notes_off();
                        }
                    }
                }
                (ch, DeviceRef::Effect(e)) => {
                    if let Some(s) = self.channel_mut(ch).and_then(|c| c.effect_mut(e)) {
                        s.bypass = bypass;
                    }
                }
                _ => {}
            },
            Command::NoteOn { track, note, velocity } => self.live_note(track, true, note, velocity),
            Command::NoteOff { track, note } => self.live_note(track, false, note, 0),
            Command::RecordMidi(buf) => {
                let at = self.launch_time(None);
                if let Some(t) = self.state.tracks.get(buf.track) {
                    t.recording_slot.store(buf.slot as i32, Relaxed);
                }
                self.ensure_playing();
                if let Some(old) = self.recording.replace(Recording { buf, start: at })
                    && let Some(t) = self.state.tracks.get(old.buf.track)
                {
                    t.recording_slot.store(NONE, Relaxed)
                }
            }
            Command::ReplaceClip { track, slot, clip } => {
                if let Some(s) = self.tracks.get_mut(track).and_then(|t| t.slots.get_mut(slot)) {
                    let old = std::mem::replace(s, clip);
                    if let Some(old) = old {
                        self.post(EngineEvent::Garbage(Garbage::Clip(old)));
                    }
                    let t = &mut self.tracks[track];
                    if t.slots[slot].is_none() && t.session.map(|p| p.slot) == Some(slot) {
                        t.session = None;
                        t.flush_notes = true;
                    }
                }
            }
            Command::SwapCore(_) => unreachable!("handled by Engine"),
        }
    }

    fn post(&mut self, ev: EngineEvent) {
        if self.outbox.len() < self.outbox.capacity() {
            self.outbox.push(ev);
        } else {
            std::mem::forget(ev);
        }
    }

    fn channel_mut(&mut self, r: TrackRef) -> Option<&mut Channel> {
        match r {
            TrackRef::Track(i) => self.tracks.get_mut(i).map(|t| &mut t.chan),
            TrackRef::Return(i) => self.returns.get_mut(i),
            TrackRef::Master => Some(&mut self.master),
        }
    }

    fn set_device_param(&mut self, channel: TrackRef, device: DeviceRef, param: usize, value: f32) {
        match (channel, device) {
            (TrackRef::Track(i), DeviceRef::Instrument) => {
                if let Some(inst) = self.tracks.get_mut(i).and_then(|t| t.instrument.as_mut()) {
                    inst.set_param(param, value);
                }
            }
            (ch, DeviceRef::Effect(e)) => {
                if let Some(s) = self.channel_mut(ch).and_then(|c| c.effect_mut(e)) {
                    s.fx.set_param(param, value);
                }
            }
            _ => {}
        }
    }

    fn live_note(&mut self, track: usize, on: bool, note: u8, vel: u8) {
        let beat = self.beat;
        if let Some(t) = self.tracks.get_mut(track)
            && t.live_len < t.live.len()
        {
            t.live[t.live_len] = (on, note, vel);
            t.live_len += 1;
        }
        if let Some(rec) = self.recording.as_mut() {
            if rec.buf.track != track || beat < rec.start {
                return;
            }
            let pos = beat - rec.start;
            let n = note as usize & 127;
            if on {
                rec.buf.held[n] = Some((pos, vel));
            } else if let Some((start, v)) = rec.buf.held[n].take()
                && rec.buf.notes.len() < rec.buf.notes.capacity()
            {
                rec.buf.notes.push(Note::new(note, start, (pos - start).max(1.0 / 64.0), v));
            }
        }
    }

    fn launch_clip(&mut self, track: usize, slot: usize) {
        let Some(t) = self.tracks.get(track) else { return };
        let clip = t.slots.get(slot).and_then(|c| c.as_ref());
        let Some(clip) = clip else {
            // Launching an empty slot acts as that track's stop button.
            self.handle(Command::StopTrack { track });
            return;
        };
        let q = clip.quantization;
        let toggle_off = clip.launch_mode == LaunchMode::Toggle && t.session.map(|s| s.slot) == Some(slot);
        let at = self.launch_time(q);
        self.ensure_playing();
        let kind = if toggle_off { PendingKind::Stop } else { PendingKind::Launch(slot) };
        self.tracks[track].pending = Some(Pending { kind, at });
    }

    fn launch_scene(&mut self, scene: usize) {
        if scene >= self.scenes.len() {
            return;
        }
        let at = self.launch_time(None);
        self.ensure_playing();
        for t in self.tracks.iter_mut() {
            let has = t.slots.get(scene).map(|c| c.is_some()).unwrap_or(false);
            if has {
                t.pending = Some(Pending { kind: PendingKind::Launch(scene), at });
            } else if t.session.is_some() {
                t.pending = Some(Pending { kind: PendingKind::Stop, at });
            }
        }
        if let Some(bpm) = self.scenes[scene].tempo {
            self.pending_tempo = Some((at, bpm));
        }
        if let Some(sig) = self.scenes[scene].time_signature {
            self.sig = sig;
        }
    }

    fn stop_transport(&mut self) {
        self.playing = false;
        for t in self.tracks.iter_mut() {
            t.session = None;
            t.pending = None;
            t.arr_active = None;
            t.active_notes = [false; 128];
            if let Some(inst) = t.instrument.as_mut() {
                inst.all_notes_off();
            }
        }
        self.pending_tempo = None;
        if let Some(rec) = self.recording.take()
            && let Some(t) = self.state.tracks.get(rec.buf.track)
        {
            t.recording_slot.store(NONE, Relaxed)
        }
    }

    // ------------------------------------------------------------------
    // Rendering
    // ------------------------------------------------------------------

    /// Render into separate left/right buffers of equal length.
    pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let total = out_l.len().min(out_r.len());
        let started = std::time::Instant::now();
        let mut done = 0;
        while done < total {
            let max = (total - done).min(CHUNK);
            let n = self.frames_until_boundary(max);
            self.render_chunk(&mut out_l[done..done + n], &mut out_r[done..done + n]);
            done += n;
        }
        let budget = total as f32 / self.sr;
        self.state.cpu_load.store(started.elapsed().as_secs_f32() / budget.max(1e-6));
    }

    /// How many frames (1..=max) until the next musical event that must
    /// land exactly on a chunk start.
    fn frames_until_boundary(&self, max: usize) -> usize {
        if !self.playing {
            return max;
        }
        let bpf = self.beats_per_frame();
        let horizon = self.beat + max as f64 * bpf;
        let mut next = horizon;
        let mut consider = |t: Beats| {
            if t > self.beat + EPS && t < next {
                next = t;
            }
        };
        if let Some((at, _)) = self.pending_tempo {
            consider(at);
        }
        if let Some((_, end)) = self.loop_region {
            consider(end);
        }
        if let Some(rec) = &self.recording {
            consider(rec.start);
            consider(rec.start + rec.buf.length);
        }
        for t in &self.tracks {
            if let Some(p) = t.pending {
                consider(p.at);
            }
            if t.follows_arrangement && t.session.is_none() {
                for a in &t.arrangement {
                    if a.start > horizon {
                        break;
                    }
                    consider(a.start);
                    consider(a.end);
                }
            }
        }
        if next >= horizon {
            return max;
        }
        (((next - self.beat) / bpf).ceil() as usize).clamp(1, max)
    }

    fn apply_due_events(&mut self) {
        let now = self.beat + EPS;
        if let Some((at, bpm)) = self.pending_tempo
            && at <= now
        {
            self.tempo = bpm;
            self.pending_tempo = None;
        }
        let st = &self.state;
        for (i, t) in self.tracks.iter_mut().enumerate() {
            let Some(p) = t.pending else { continue };
            if p.at > now {
                continue;
            }
            t.pending = None;
            t.flush_notes = true;
            t.stretcher.reset();
            match p.kind {
                PendingKind::Launch(slot) => {
                    t.session = Some(SessionPlay { slot, start: p.at });
                    t.follows_arrangement = false;
                    t.arr_active = None;
                }
                PendingKind::Stop => t.session = None,
            }
            st.tracks[i].stop_queued.store(false, Relaxed);
        }
        self.finish_recording_if_due();
    }

    fn finish_recording_if_due(&mut self) {
        let Some(rec) = self.recording.as_ref() else { return };
        if self.beat + EPS < rec.start + rec.buf.length {
            return;
        }
        let mut rec = self.recording.take().expect("checked above");
        let len = rec.buf.length;
        for pitch in 0..128 {
            if let Some((start, v)) = rec.buf.held[pitch].take()
                && rec.buf.notes.len() < rec.buf.notes.capacity()
            {
                rec.buf.notes.push(Note::new(pitch as u8, start, (len - start).max(1.0 / 64.0), v));
            }
        }
        rec.buf.notes.sort_unstable_by(|a, b| a.start.total_cmp(&b.start));
        rec.buf.start = rec.start;
        if let Some(t) = self.state.tracks.get(rec.buf.track) {
            t.recording_slot.store(NONE, Relaxed);
        }
        // Hand the buffer back; the UI turns it into a clip and launches it.
        self.post(EngineEvent::Recorded(rec.buf));
    }

    fn render_chunk(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len();
        if self.playing {
            self.apply_due_events();
        }
        let bpf = self.beats_per_frame();
        let b0 = self.beat;
        let b1 = if self.playing { b0 + n as f64 * bpf } else { b0 };
        let ctx = ProcessContext { sample_rate: self.sr, tempo: self.tempo, beat: b0, playing: self.playing };

        // Phase A: clip sources and instruments.
        for t in self.tracks.iter_mut() {
            t.chan.clear(n);
            render_track_source(t, &ctx, b0, b1, n, self.playing);
            if self.playing && t.follows_arrangement && t.session.is_none() {
                apply_automation(t, b0);
            }
            if t.is_key_source {
                t.key_l[..n].copy_from_slice(&t.chan.l[..n]);
                t.key_r[..n].copy_from_slice(&t.chan.r[..n]);
            }
        }

        // Phase B: insert effects (with sidechain keys from phase A).
        for i in 0..self.tracks.len() {
            for e in 0..self.tracks[i].chan.effects.len() {
                if let Some(src) = self.tracks[i].chan.effects[e].fx.wants_sidechain()
                    && src < self.tracks.len()
                    && self.tracks[src].is_key_source
                {
                    let (kl, kr) = (self.tracks[src].key_l.as_ptr(), self.tracks[src].key_r.as_ptr());
                    // SAFETY: key buffers are never resized after construction,
                    // and `src` may equal `i` only through key_* (not chan.*),
                    // so these slices never alias the effect being borrowed.
                    let (kl, kr) = unsafe { (std::slice::from_raw_parts(kl, n), std::slice::from_raw_parts(kr, n)) };
                    self.tracks[i].chan.effects[e].fx.sidechain(kl, kr);
                }
            }
            self.tracks[i].chan.run_effects(&ctx, n);
        }

        // Phase C: faders, sends, returns, master.
        for r in self.returns.iter_mut() {
            r.clear(n);
        }
        self.master.clear(n);
        let any_solo = self.tracks.iter().any(|t| t.solo);
        for (i, t) in self.tracks.iter_mut().enumerate() {
            let silent = any_solo && !t.solo;
            let (pl, pr) = t.chan.apply_fader(n, silent);
            self.state.tracks[i].channel.set_peaks(pl, pr);
            for (ri, ret) in self.returns.iter_mut().enumerate() {
                let g = t.sends.get(ri).copied().unwrap_or(0.0);
                if g > 0.0 {
                    for k in 0..n {
                        ret.l[k] += t.chan.l[k] * g;
                        ret.r[k] += t.chan.r[k] * g;
                    }
                }
            }
            for k in 0..n {
                self.master.l[k] += t.chan.l[k];
                self.master.r[k] += t.chan.r[k];
            }
        }
        for (ri, ret) in self.returns.iter_mut().enumerate() {
            ret.run_effects(&ctx, n);
            let (pl, pr) = ret.apply_fader(n, false);
            self.state.returns[ri].set_peaks(pl, pr);
            for k in 0..n {
                self.master.l[k] += ret.l[k];
                self.master.r[k] += ret.r[k];
            }
        }
        if self.metronome && self.playing {
            self.render_click(b0, bpf, n);
        }
        self.master.run_effects(&ctx, n);
        let (pl, pr) = self.master.apply_fader(n, false);
        self.state.master.set_peaks(pl, pr);
        out_l.copy_from_slice(&self.master.l[..n]);
        out_r.copy_from_slice(&self.master.r[..n]);

        // Advance the transport.
        if self.playing {
            self.beat = b1;
            if let Some((ls, le)) = self.loop_region
                && self.beat >= le - EPS
            {
                self.beat = ls + (self.beat - le).max(0.0);
                for t in self.tracks.iter_mut().filter(|t| t.follows_arrangement && t.session.is_none()) {
                    t.flush_notes = true;
                    t.arr_active = None;
                }
            }
        }
        self.publish();
    }

    fn render_click(&mut self, b0: Beats, bpf: f64, n: usize) {
        let bar = self.sig.beats_per_bar();
        let mut k = b0.ceil();
        // Split the chunk at each beat line and trigger a click there.
        let mut next_click = if (k - b0) / bpf < n as f64 { Some(((k - b0) / bpf) as usize) } else { None };
        for i in 0..n {
            if Some(i) == next_click {
                let accent = (k / bar).fract().abs() < 1e-6;
                self.click = Click {
                    t: 0,
                    len: (0.03 * self.sr) as usize,
                    freq: if accent { 1_600.0 } else { 1_000.0 },
                    phase: 0.0,
                    amp: if accent { 0.5 } else { 0.3 },
                };
                k += 1.0;
                next_click = if (k - b0) / bpf < n as f64 { Some(((k - b0) / bpf) as usize) } else { None };
            }
            let c = &mut self.click;
            if c.t < c.len {
                let env = 1.0 - c.t as f32 / c.len as f32;
                let s = (c.phase * std::f32::consts::TAU).sin() * c.amp * env * env;
                c.phase = (c.phase + c.freq / self.sr) % 1.0;
                c.t += 1;
                self.master.l[i] += s;
                self.master.r[i] += s;
            }
        }
    }

    fn publish(&self) {
        let st = &self.state;
        st.playing.store(self.playing, Relaxed);
        st.beat.store(self.beat);
        st.tempo.store(self.tempo);
        for (t, ts) in self.tracks.iter().zip(st.tracks.iter()) {
            ts.playing_slot.store(t.session.map_or(NONE, |s| s.slot as i32), Relaxed);
            match t.pending {
                Some(Pending { kind: PendingKind::Launch(s), .. }) => {
                    ts.queued_slot.store(s as i32, Relaxed);
                    ts.stop_queued.store(false, Relaxed);
                }
                Some(Pending { kind: PendingKind::Stop, .. }) => {
                    ts.queued_slot.store(NONE, Relaxed);
                    ts.stop_queued.store(true, Relaxed);
                }
                None => {
                    ts.queued_slot.store(NONE, Relaxed);
                    ts.stop_queued.store(false, Relaxed);
                }
            }
            ts.clip_position.store(t.clip_pos);
            ts.follows_arrangement.store(t.follows_arrangement, Relaxed);
        }
    }
}

/// Render one track's clip/instrument output into its channel buffers.
fn render_track_source(t: &mut Track, ctx: &ProcessContext, b0: Beats, b1: Beats, n: usize, playing: bool) {
    t.events.clear();
    let frames = n as u32;

    // Flush first so new notes at frame 0 sort after these releases.
    if t.flush_notes {
        for pitch in 0..128u8 {
            t.events.push(MidiEvent::off(0, pitch));
        }
        t.flush_notes = false;
    }
    for k in 0..t.live_len {
        let (on, note, vel) = t.live[k];
        t.events.push(if on { MidiEvent::on(0, note, vel) } else { MidiEvent::off(0, note) });
    }
    t.live_len = 0;

    let mut audio: Option<(Placement, *const RtClip)> = None;
    if playing {
        if let Some(play) = t.session {
            if let Some(clip) = t.slots.get(play.slot).and_then(|c| c.as_deref()) {
                let place = Placement { start: play.start, end: None };
                t.clip_pos = clip_local(b0, place.start, clip.length, clip.looping);
                match &clip.content {
                    RtContent::Midi(notes) => {
                        if let ClipStatus::Ended(_) = schedule_midi(notes, clip.length, clip.looping, place, b0, b1, frames, &mut t.events) {
                            t.session = None;
                        }
                    }
                    RtContent::Audio(_) => audio = Some((place, clip as *const RtClip)),
                }
            } else {
                t.session = None;
            }
        } else if t.follows_arrangement {
            let idx = t.arrangement.iter().position(|a| a.start <= b0 + EPS && b0 + EPS < a.end);
            if idx != t.arr_active {
                if t.arr_active.is_some() {
                    for pitch in 0..128u8 {
                        t.events.push(MidiEvent::off(0, pitch));
                    }
                }
                t.arr_active = idx;
                t.stretcher.reset();
            }
            if let Some(i) = idx {
                let a = &t.arrangement[i];
                let place = Placement { start: a.start, end: Some(a.end) };
                t.clip_pos = clip_local(b0, a.start, a.clip.length, a.clip.looping);
                match &a.clip.content {
                    RtContent::Midi(notes) => {
                        schedule_midi(notes, a.clip.length, a.clip.looping, place, b0, b1, frames, &mut t.events);
                    }
                    RtContent::Audio(_) => audio = Some((place, &a.clip as *const RtClip)),
                }
            }
        }
    }

    // Instruments (MIDI tracks).
    t.events.resolve(&mut t.active_notes, &mut t.rng);
    if let (Some(inst), false) = (t.instrument.as_mut(), t.instrument_bypass) {
        let (l, r) = (&mut t.chan.l[..n], &mut t.chan.r[..n]);
        let mut pos = 0usize;
        let bpf = if n > 0 { (b1 - b0) / n as f64 } else { 0.0 };
        for ev in t.events.as_slice() {
            let f = ev.frame as usize;
            if f > pos {
                let sub = ProcessContext { beat: b0 + pos as f64 * bpf, ..*ctx };
                inst.render(&sub, &mut l[pos..f], &mut r[pos..f]);
                pos = f;
            }
            if ev.on {
                inst.note_on(ev.note, ev.velocity);
            } else {
                inst.note_off(ev.note);
            }
        }
        if pos < n {
            let sub = ProcessContext { beat: b0 + pos as f64 * bpf, ..*ctx };
            inst.render(&sub, &mut l[pos..], &mut r[pos..]);
        }
    }

    // Audio clips (audio tracks, or audio clips on any track).
    if let Some((place, clip_ptr)) = audio {
        // SAFETY: the clip lives in `t.slots` / `t.arrangement`, which are not
        // modified while rendering this chunk; only `t.chan` and
        // `t.stretcher` are borrowed mutably below.
        let clip = unsafe { &*clip_ptr };
        if let RtContent::Audio(a) = &clip.content {
            let ended = render_audio(a, clip, place, b0, b1, n, ctx.tempo, &mut t.stretcher, &mut t.chan.l[..n], &mut t.chan.r[..n]);
            if ended && t.session.is_some() {
                t.session = None;
            }
        }
    }
}

fn clip_local(beat: Beats, start: Beats, len: Beats, looping: bool) -> f64 {
    let local = (beat - start).max(0.0);
    if looping { local % len.max(1e-6) } else { local }
}

/// Returns true when a one-shot or region ended inside this chunk.
#[allow(clippy::too_many_arguments)]
fn render_audio(
    a: &crate::clip::RtAudio,
    clip: &RtClip,
    place: Placement,
    b0: Beats,
    b1: Beats,
    n: usize,
    tempo: f64,
    st: &mut GrainStretcher,
    l: &mut [f32],
    r: &mut [f32],
) -> bool {
    let Some(buf) = a.buf.as_deref() else { return false };
    let bpf = (b1 - b0) / n.max(1) as f64;
    let sr = buf.sample_rate as f64;
    // 2 ms fades at the clip start avoid clicks on launch.
    let fade_beats = 0.002 * tempo / 60.0;
    for i in 0..n {
        let beat = b0 + i as f64 * bpf;
        if beat < place.start {
            continue;
        }
        if let Some(e) = place.end
            && beat >= e
        {
            return true;
        }
        let mut local = beat - place.start;
        if !clip.looping && local >= clip.length {
            return true;
        }
        if clip.looping {
            local %= clip.length;
        }
        let pos = audio_source_pos(a, local, tempo, sr);
        let (sl, sr_) = match a.warp {
            WarpMode::Stretch => st.next(buf, pos),
            _ => (buf.read(0, pos), buf.read(1, pos)),
        };
        let since_start = beat - place.start;
        let fade = if since_start < fade_beats { (since_start / fade_beats) as f32 } else { 1.0 };
        l[i] += sl * a.gain * fade;
        r[i] += sr_ * a.gain * fade;
    }
    false
}

fn apply_automation(t: &mut Track, beat: Beats) {
    for lane in &t.automation {
        let Some(v) = lane.value_at(beat) else { continue };
        match lane.target {
            AutomationTarget::Volume => t.chan.gain_target = db_to_gain(v),
            AutomationTarget::Pan => t.chan.pan = v.clamp(-1.0, 1.0),
            AutomationTarget::Send(i) => {
                if let Some(s) = t.sends.get_mut(i) {
                    *s = db_to_gain(v);
                }
            }
            AutomationTarget::Instrument { param } => {
                if let Some(inst) = t.instrument.as_mut()
                    && let Some(info) = inst.params().get(param)
                {
                    let value = info.denormalize(v);
                    inst.set_param(param, value);
                }
            }
            AutomationTarget::Effect { index, param } => {
                if let Some(slot) = t.chan.effects.get_mut(index)
                    && let Some(info) = slot.fx.params().get(param)
                {
                    let value = info.denormalize(v);
                    slot.fx.set_param(param, value);
                }
            }
        }
    }
}

// ----------------------------------------------------------------------
// Engine / handle split
// ----------------------------------------------------------------------

/// Owned by the audio callback.
pub struct Engine {
    core: Box<Core>,
    commands: Consumer<Command>,
    events: Producer<EngineEvent>,
}

/// Owned by the UI / control thread.
pub struct EngineHandle {
    commands: Producer<Command>,
    events: Consumer<EngineEvent>,
    pub state: Arc<EngineState>,
}

impl EngineHandle {
    /// Send a command; returns it back if the queue is full.
    pub fn send(&mut self, cmd: Command) -> Result<(), Command> {
        if let Command::SwapCore(core) = &cmd {
            self.state = core.state();
        }
        self.commands.push(cmd).map_err(|rtrb::PushError::Full(c)| c)
    }

    /// Drain events from the audio thread. Garbage is dropped here; recorded
    /// clips are returned for the caller to store.
    pub fn poll(&mut self) -> Vec<Box<RecordBuffer>> {
        let mut recorded = Vec::new();
        while let Ok(ev) = self.events.pop() {
            match ev {
                EngineEvent::Recorded(r) => recorded.push(r),
                EngineEvent::Garbage(_) => {}
            }
        }
        recorded
    }
}

pub fn create(core: Core) -> (Engine, EngineHandle) {
    let (ctx, crx) = RingBuffer::new(1024);
    let (etx, erx) = RingBuffer::new(256);
    let state = core.state();
    (Engine { core: Box::new(core), commands: crx, events: etx }, EngineHandle { commands: ctx, events: erx, state })
}

impl Engine {
    pub fn core(&self) -> &Core {
        &self.core
    }

    /// Apply queued commands. Called automatically by `process`.
    pub fn drain_commands(&mut self) {
        while let Ok(cmd) = self.commands.pop() {
            match cmd {
                Command::SwapCore(mut new) => {
                    new.beat = self.core.beat;
                    new.playing = self.core.playing;
                    new.tempo = self.core.tempo;
                    // Keep clips playing across structural edits (same track
                    // index, slot still filled).
                    for (n, o) in new.tracks.iter_mut().zip(self.core.tracks.iter()) {
                        n.follows_arrangement = o.follows_arrangement;
                        n.session = o.session.filter(|s| n.slots.get(s.slot).is_some_and(|c| c.is_some()));
                        n.pending = o.pending;
                    }
                    let old = std::mem::replace(&mut self.core, new);
                    self.dispose(EngineEvent::Garbage(Garbage::Core(old)));
                }
                other => self.core.handle(other),
            }
        }
    }

    fn dispose(&mut self, ev: EngineEvent) {
        if let Err(rtrb::PushError::Full(ev)) = self.events.push(ev) {
            // Queue full: leak rather than free on the audio thread.
            std::mem::forget(ev);
        }
    }

    /// Render a block of planar stereo audio.
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        self.drain_commands();
        self.core.render(out_l, out_r);
        while let Some(ev) = self.core.outbox.pop() {
            self.dispose(ev);
        }
    }

    /// Render interleaved audio with `channels` channels (stereo duplicated /
    /// mixed as needed). `scratch` must hold at least 2 * frames samples.
    pub fn process_interleaved(&mut self, out: &mut [f32], channels: usize, scratch: &mut [f32]) {
        let ch = channels.max(1);
        let frames = out.len() / ch;
        let (l, rest) = scratch.split_at_mut(frames);
        let r = &mut rest[..frames];
        self.process(l, r);
        for f in 0..frames {
            let o = &mut out[f * ch..f * ch + ch];
            if ch == 1 {
                o[0] = (l[f] + r[f]) * 0.5;
            } else {
                o[0] = l[f];
                o[1] = r[f];
                o[2..].fill(0.0);
            }
        }
    }
}
