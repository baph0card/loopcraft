//! Per-track real-time state and source rendering (clips, MIDI effects,
//! instruments, warped audio).

use crate::clip::{ClipStatus, EventList, MidiEvent, Placement, RtAudio, RtClip, RtContent, audio_source_pos, schedule_midi};
use loopcraft_core::*;
use loopcraft_dsp::midi::MidiEffect;
use loopcraft_dsp::stretch::{BeatsStretcher, GrainStretcher, PvStretcher};
use loopcraft_dsp::util::Noise;
use loopcraft_dsp::{Effect, Instrument, ProcessContext};
use std::sync::Arc;

use super::CHUNK;

pub(crate) const EPS: f64 = 1e-9;

/// Exact integer-sample delay (read before write).
pub(crate) struct FixedDelay {
    l: Vec<f32>,
    r: Vec<f32>,
    pos: usize,
}

impl FixedDelay {
    pub fn new(samples: usize) -> Option<Self> {
        (samples > 0).then(|| Self { l: vec![0.0; samples], r: vec![0.0; samples], pos: 0 })
    }

    #[inline]
    pub fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        let n = self.l.len();
        for i in 0..l.len() {
            let (ol, or) = (self.l[self.pos], self.r[self.pos]);
            self.l[self.pos] = l[i];
            self.r[self.pos] = r[i];
            l[i] = ol;
            r[i] = or;
            self.pos = (self.pos + 1) % n;
        }
    }
}

pub(crate) struct FxSlot {
    pub fx: Box<dyn Effect>,
    pub bypass: bool,
}

pub(crate) struct MidiFxSlot {
    pub fx: Box<dyn MidiEffect>,
    pub bypass: bool,
}

/// Fader, pan and insert chain shared by every kind of channel.
pub(crate) struct Channel {
    pub effects: Vec<FxSlot>,
    pub gain_target: f32,
    pub gain_now: f32,
    pub pan: f32,
    pub mute: bool,
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    /// Latency of the insert chain.
    pub latency: usize,
    /// Delay-compensation applied after the chain.
    pub pdc: Option<FixedDelay>,
}

impl Channel {
    pub fn new(effects: Vec<FxSlot>, mixer: &MixerSettings) -> Self {
        let g = db_to_gain(mixer.volume_db);
        // Start the fader where the first block will want it, so a muted or
        // non-soloed channel doesn't leak a ramp-down at startup.
        let now = if mixer.mute { 0.0 } else { g };
        let latency = effects.iter().map(|e| e.fx.latency()).sum();
        Self { effects, gain_target: g, gain_now: now, pan: mixer.pan, mute: mixer.mute, l: vec![0.0; CHUNK], r: vec![0.0; CHUNK], latency, pdc: None }
    }

    pub fn clear(&mut self, n: usize) {
        self.l[..n].fill(0.0);
        self.r[..n].fill(0.0);
    }

    pub fn run_effects(&mut self, ctx: &ProcessContext, n: usize) {
        for slot in self.effects.iter_mut() {
            if slot.bypass {
                continue;
            }
            slot.fx.process(ctx, &mut self.l[..n], &mut self.r[..n]);
        }
        if let Some(d) = self.pdc.as_mut() {
            d.process(&mut self.l[..n], &mut self.r[..n]);
        }
    }

    /// Apply fader (ramped across the chunk) and balance-style pan; returns peaks.
    pub fn apply_fader(&mut self, n: usize, silent: bool, extra_gain: f32) -> (f32, f32) {
        let target = if silent || self.mute { 0.0 } else { self.gain_target * extra_gain };
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
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PendingKind {
    Launch(usize),
    Stop,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Pending {
    pub kind: PendingKind,
    pub at: Beats,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionPlay {
    pub slot: usize,
    pub start: Beats,
}

pub(crate) struct ArrClip {
    pub start: Beats,
    /// Clip position at `start`.
    pub offset: Beats,
    pub end: Beats,
    pub clip: RtClip,
}

/// The three warp engines, preallocated per track.
pub(crate) struct Stretchers {
    pub grain: GrainStretcher,
    pub beats: BeatsStretcher,
    pub pv: Box<PvStretcher>,
}

impl Stretchers {
    pub fn new(sr: f32) -> Self {
        Self { grain: GrainStretcher::new(sr, 60.0), beats: BeatsStretcher::new(sr, Arc::new(vec![0])), pv: Box::new(PvStretcher::new()) }
    }
    pub fn reset(&mut self) {
        self.grain.reset();
        self.beats.reset();
        self.pv.reset();
    }
}

pub(crate) struct Track {
    pub kind: TrackKind,
    pub parent: Option<usize>,
    pub chan: Channel,
    pub midi_fx: Vec<MidiFxSlot>,
    pub instrument: Option<Box<dyn Instrument>>,
    pub instrument_bypass: bool,
    pub slots: Vec<Option<Box<RtClip>>>,
    pub arrangement: Vec<ArrClip>,
    pub automation: Vec<AutomationLane>,
    pub sends: Vec<f32>,
    pub solo: bool,
    pub crossfade: CrossfadeAssign,
    pub input: InputSource,
    pub monitor: Monitor,
    pub armed: bool,

    pub session: Option<SessionPlay>,
    pub pending: Option<Pending>,
    pub follow_at: Option<Beats>,
    pub follows_arrangement: bool,
    pub arr_active: Option<usize>,
    pub flush_notes: bool,
    /// Don't play arrangement clips (being recorded over).
    pub arr_recording: bool,

    pub events: EventList,
    pub ev_b: EventList,
    pub live: [(bool, u8, u8); 64],
    pub live_len: usize,
    pub active_notes: [bool; 128],
    pub rng: Noise,
    pub stretch: Stretchers,
    /// Pre-effects copy for tracks that key another track's sidechain.
    pub is_key_source: bool,
    pub key_l: Vec<f32>,
    pub key_r: Vec<f32>,
    pub clip_pos: f64,
    /// Post-fader output of the last chunk (for resampling inputs).
    pub out_l: Vec<f32>,
    pub out_r: Vec<f32>,
    /// This chunk's input signal (what monitoring and recording hear).
    pub in_l: Vec<f32>,
    pub in_r: Vec<f32>,
    pub has_input: bool,
}

impl Track {
    /// Is a clip (session or arrangement) currently producing sound?
    pub fn clip_playing(&self) -> bool {
        self.session.is_some() || (self.follows_arrangement && self.arr_active.is_some() && !self.arr_recording)
    }

    /// Should the live input be heard on this track right now?
    pub fn monitoring(&self) -> bool {
        match self.monitor {
            Monitor::In => true,
            Monitor::Off => false,
            Monitor::Auto => self.armed && !self.clip_playing(),
        }
    }
}

/// Render one track's clip/instrument output into its channel buffers.
pub(crate) fn render_track_source(t: &mut Track, ctx: &ProcessContext, fx_ctx: &ProcessContext, b0: Beats, b1: Beats, n: usize, playing: bool) {
    t.events.clear();
    let frames = n as u32;

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
                t.clip_pos = clip.local(b0 - place.start);
                match &clip.content {
                    RtContent::Midi(notes) => {
                        if let ClipStatus::Ended(_) = schedule_midi(notes, clip, place, b0, b1, frames, &mut t.events) {
                            t.session = None;
                            t.follow_at = None;
                        }
                    }
                    RtContent::Audio(_) => audio = Some((place, clip as *const RtClip)),
                }
            } else {
                t.session = None;
            }
        } else if t.follows_arrangement && !t.arr_recording {
            let idx = t.arrangement.iter().position(|a| a.start <= b0 + EPS && b0 + EPS < a.end);
            if idx != t.arr_active {
                if t.arr_active.is_some() {
                    for pitch in 0..128u8 {
                        t.events.push(MidiEvent::off(0, pitch));
                    }
                }
                t.arr_active = idx;
                t.stretch.reset();
                if let Some(i) = idx
                    && let RtContent::Audio(a) = &t.arrangement[i].clip.content
                {
                    t.stretch.beats.set_slices(a.slices.clone());
                }
            }
            if let Some(i) = idx {
                let a = &t.arrangement[i];
                let place = Placement { start: a.start - a.offset, end: Some(a.end) };
                t.clip_pos = a.clip.local(b0 - place.start);
                match &a.clip.content {
                    RtContent::Midi(notes) => {
                        schedule_midi(notes, &a.clip, place, b0, b1, frames, &mut t.events);
                    }
                    RtContent::Audio(_) => audio = Some((place, &a.clip as *const RtClip)),
                }
            }
        }
    }

    // Notes: probability rolls, then the MIDI effect chain.
    t.events.resolve(&mut t.active_notes, &mut t.rng);
    for slot in t.midi_fx.iter_mut().filter(|s| !s.bypass) {
        t.ev_b.clear();
        slot.fx.process(fx_ctx, frames, &t.events, &mut t.ev_b);
        std::mem::swap(&mut t.events, &mut t.ev_b);
    }
    // Flushes bypass the chain: they must reach the instrument whatever the effects did.
    if t.flush_notes {
        for slot in t.midi_fx.iter_mut() {
            slot.fx.reset();
        }
        for pitch in 0..128u8 {
            t.events.push(MidiEvent::off(0, pitch));
        }
        t.flush_notes = false;
    }
    t.events.sort();

    // Instruments (MIDI tracks).
    if let (Some(inst), false) = (t.instrument.as_mut(), t.instrument_bypass) {
        let (l, r) = (&mut t.chan.l[..n], &mut t.chan.r[..n]);
        let mut pos = 0usize;
        let bpf = if n > 0 { (b1 - b0) / n as f64 } else { 0.0 };
        for ev in t.events.as_slice() {
            let f = (ev.frame as usize).min(n);
            if f > pos {
                let sub = ProcessContext { beat: fx_ctx.beat + pos as f64 * bpf, ..*fx_ctx };
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
            let sub = ProcessContext { beat: fx_ctx.beat + pos as f64 * bpf, ..*fx_ctx };
            inst.render(&sub, &mut l[pos..], &mut r[pos..]);
        }
    }

    // Audio clips.
    if let Some((place, clip_ptr)) = audio {
        // SAFETY: the clip lives in `t.slots` / `t.arrangement`, which are not
        // modified while rendering this chunk; only `t.chan` and
        // `t.stretch` are borrowed mutably below.
        let clip = unsafe { &*clip_ptr };
        if let RtContent::Audio(a) = &clip.content {
            let ended = render_audio(a, clip, place, b0, b1, n, ctx.tempo, &mut t.stretch, &mut t.chan.l[..n], &mut t.chan.r[..n]);
            if ended && t.session.is_some() {
                t.session = None;
                t.follow_at = None;
            }
        }
    }
}

/// Returns true when a one-shot or region ended inside this chunk.
#[allow(clippy::too_many_arguments)]
fn render_audio(a: &RtAudio, clip: &RtClip, place: Placement, b0: Beats, b1: Beats, n: usize, tempo: f64, st: &mut Stretchers, l: &mut [f32], r: &mut [f32]) -> bool {
    let Some(buf) = a.buf.as_deref() else { return false };
    let bpf = (b1 - b0) / n.max(1) as f64;
    let sr = buf.sample_rate as f64;
    // 2 ms declick at the clip start, plus the clip's own fades.
    let declick = 0.002 * tempo / 60.0;
    let fade_in = a.spec.fade_in.max(declick);
    let fade_out = a.spec.fade_out;
    let region_end = place.end.unwrap_or(place.start + if clip.looping { f64::INFINITY } else { clip.length });
    st.grain.pitch_ratio = a.pitch_ratio;
    st.beats.pitch_ratio = a.pitch_ratio;
    st.pv.pitch_ratio = a.pitch_ratio;
    for i in 0..n {
        let beat = b0 + i as f64 * bpf;
        if beat < place.start {
            continue;
        }
        if beat >= region_end {
            return true;
        }
        let local = clip.local(beat - place.start);
        let pos = audio_source_pos(a, local, tempo, sr);
        let (sl, sr_) = match a.spec.warp {
            WarpMode::Stretch => st.grain.next(buf, pos),
            WarpMode::Beats => st.beats.next(buf, pos),
            WarpMode::Complex => st.pv.next(buf, pos),
            WarpMode::Repitch | WarpMode::Off => {
                let l = buf.read(0, pos);
                (l, if buf.channels.len() > 1 { buf.read(1, pos) } else { l })
            }
        };
        let since = beat - place.start;
        let mut g = a.gain;
        if since < fade_in {
            g *= (since / fade_in) as f32;
        }
        if fade_out > 0.0 && region_end.is_finite() && region_end - beat < fade_out {
            g *= ((region_end - beat) / fade_out) as f32;
        }
        l[i] += sl * g;
        r[i] += sr_ * g;
    }
    false
}

pub(crate) fn apply_automation(t: &mut Track, beat: Beats) {
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
