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
//! follow actions, tempo changes, arrangement clip edges, loop end,
//! recording punch points), so all of these are sample-accurate.
//!
//! Signal flow per chunk: sources (clips, instruments, live input) ->
//! insert chains, children before their group -> faders, crossfader, sends
//! -> group / master / cue buses -> returns -> master chain.

mod build;
mod control;
mod follow;
mod render;
pub(crate) mod track;

use crate::command::{Command, EngineEvent, Garbage};
use crate::state::EngineState;
use loopcraft_core::*;
use loopcraft_dsp::Device;
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use track::{Channel, FixedDelay, Track};

/// Largest chunk rendered in one go. Also the size of every scratch buffer.
pub const CHUNK: usize = 256;
/// Most input channels the engine accepts from the audio interface.
pub const MAX_INPUTS: usize = 16;
/// Largest host buffer `process_io` accepts in one call (larger ones are split).
pub const MAX_HOST_FRAMES: usize = 8192;

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

pub(crate) struct Click {
    pub t: usize,
    pub len: usize,
    pub freq: f32,
    pub phase: f32,
    pub amp: f32,
}

pub(crate) enum RecKind {
    Midi(Box<crate::command::RecordBuffer>),
    Audio(Box<crate::command::AudioRecord>),
}

pub(crate) struct Recording {
    pub kind: RecKind,
    pub track: usize,
    pub start: Beats,
    pub end: Option<Beats>,
}

/// The audio graph and transport. Built off the audio thread from a
/// [`Project`]; moved into the audio callback via [`Engine`].
pub struct Core {
    pub(crate) sr: f32,
    pub(crate) tempo: f64,
    pub(crate) sig: TimeSignature,
    pub(crate) quant: Quantization,
    pub(crate) playing: bool,
    pub(crate) beat: Beats,
    /// Free-running beat clock for devices while the transport is stopped.
    pub(crate) free_beat: Beats,
    pub(crate) loop_region: Option<(Beats, Beats)>,
    pub(crate) metronome: bool,
    pub(crate) tracks: Vec<Track>,
    /// Processing order: children before their groups.
    pub(crate) order: Vec<usize>,
    pub(crate) solo_rel: Vec<bool>,
    pub(crate) returns: Vec<Channel>,
    pub(crate) return_sends: Vec<Vec<f32>>,
    pub(crate) master: Channel,
    /// Sum of tracks before master; delayed to line up with return latency.
    pub(crate) bus_l: Vec<f32>,
    pub(crate) bus_r: Vec<f32>,
    pub(crate) bus_pdc: Option<FixedDelay>,
    pub(crate) cue_l: Vec<f32>,
    pub(crate) cue_r: Vec<f32>,
    pub(crate) cue_mode: bool,
    pub(crate) cue_gain: f32,
    pub(crate) crossfader: f32,
    pub(crate) scenes: Vec<Scene>,
    pub(crate) pending_tempo: Option<(Beats, f64)>,
    pub(crate) pending_sig: Option<(Beats, TimeSignature)>,
    pub(crate) scene_follow: Option<(Beats, usize)>,
    pub(crate) tempo_points: Vec<(Beats, f64)>,
    pub(crate) tempo_auto: bool,
    pub(crate) count_in: Option<(Beats, Beats)>,
    pub(crate) click: Click,
    pub(crate) recordings: Vec<Recording>,
    pub(crate) state: Arc<EngineState>,
    /// Events produced inside a chunk, flushed to the event ring afterwards.
    /// Preallocated; never grows on the audio thread.
    pub(crate) outbox: Vec<EngineEvent>,
    /// Planar input of the current host buffer.
    pub(crate) input: Vec<Vec<f32>>,
    pub(crate) input_frames: usize,
    pub(crate) input_channels: usize,
    pub(crate) master_prev_l: Vec<f32>,
    pub(crate) master_prev_r: Vec<f32>,
    pub(crate) latency: usize,
    pub(crate) rng: loopcraft_dsp::util::Noise,
}

impl Core {
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

    /// Output delay introduced by plugin/device delay compensation, in samples.
    pub fn latency(&self) -> usize {
        self.latency
    }

    pub(crate) fn post(&mut self, ev: EngineEvent) {
        if self.outbox.len() < self.outbox.capacity() {
            self.outbox.push(ev);
        } else {
            std::mem::forget(ev);
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
    cue_l: Vec<f32>,
    cue_r: Vec<f32>,
}

/// What `EngineHandle::poll` collected.
#[derive(Default)]
pub struct Polled {
    pub midi: Vec<Box<crate::command::RecordBuffer>>,
    pub audio: Vec<Box<crate::command::AudioRecord>>,
}

/// Owned by the UI / control thread.
pub struct EngineHandle {
    commands: Producer<Command>,
    events: Consumer<EngineEvent>,
    pub state: Arc<EngineState>,
    audio_done: Vec<Box<crate::command::AudioRecord>>,
}

impl EngineHandle {
    /// Send a command; returns it back if the queue is full.
    pub fn send(&mut self, cmd: Command) -> Result<(), Command> {
        if let Command::SwapCore(core) = &cmd {
            self.state = core.state();
        }
        self.commands.push(cmd).map_err(|rtrb::PushError::Full(c)| c)
    }

    /// Drain events from the audio thread. Garbage is dropped here; finished
    /// MIDI recordings are returned for the caller to store.
    pub fn poll(&mut self) -> Vec<Box<crate::command::RecordBuffer>> {
        let p = self.poll_all();
        self.audio_done.extend(p.audio);
        p.midi
    }

    /// Drain events, returning both MIDI and audio recordings.
    pub fn poll_all(&mut self) -> Polled {
        let mut out = Polled::default();
        out.audio.append(&mut self.audio_done);
        while let Ok(ev) = self.events.pop() {
            match ev {
                EngineEvent::Recorded(r) => out.midi.push(r),
                EngineEvent::AudioRecorded(a) => out.audio.push(a),
                EngineEvent::Garbage(_) => {}
            }
        }
        out
    }
}

/// Make an SPSC ring for streaming a recording (`seconds` of stereo at `sr`).
pub fn record_ring(sr: f32, seconds: f32) -> (Producer<f32>, Consumer<f32>) {
    RingBuffer::new(((sr * seconds) as usize * 2).max(4096))
}

pub fn create(core: Core) -> (Engine, EngineHandle) {
    let (ctx, crx) = RingBuffer::new(1024);
    let (etx, erx) = RingBuffer::new(256);
    let state = core.state();
    (
        Engine { core: Box::new(core), commands: crx, events: etx, cue_l: vec![0.0; MAX_HOST_FRAMES], cue_r: vec![0.0; MAX_HOST_FRAMES] },
        EngineHandle { commands: ctx, events: erx, state, audio_done: Vec::new() },
    )
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
                    self.core.carry_over_into(&mut new);
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

    fn flush_outbox(&mut self) {
        while let Some(ev) = self.core.outbox.pop() {
            self.dispose(ev);
        }
    }

    /// Render a block of planar stereo audio (no input, cue discarded).
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        self.drain_commands();
        let n = out_l.len().min(out_r.len());
        let mut done = 0;
        while done < n {
            let m = (n - done).min(MAX_HOST_FRAMES);
            self.core.input_frames = 0;
            let (cl, cr) = (&mut self.cue_l[..m], &mut self.cue_r[..m]);
            self.core.render(&mut out_l[done..done + m], &mut out_r[done..done + m], cl, cr, 0);
            done += m;
        }
        self.flush_outbox();
    }

    /// Render with audio input. `input` is interleaved with `in_channels`
    /// channels; `out` is interleaved with `out_channels` channels. With four
    /// or more output channels, channels 3/4 carry the cue (pre-listen) bus.
    /// `scratch` must hold at least 2 * frames samples.
    pub fn process_io(&mut self, input: &[f32], in_channels: usize, out: &mut [f32], out_channels: usize, scratch: &mut [f32]) {
        self.drain_commands();
        let och = out_channels.max(1);
        let frames = out.len() / och;
        let ich = in_channels.min(MAX_INPUTS);
        let mut done = 0;
        while done < frames {
            let m = (frames - done).min(MAX_HOST_FRAMES).min(scratch.len() / 2);
            if m == 0 {
                break;
            }
            // Deinterleave input.
            for c in 0..ich {
                for f in 0..m {
                    let idx = (done + f) * in_channels + c;
                    self.core.input[c][f] = input.get(idx).copied().unwrap_or(0.0);
                }
            }
            self.core.input_frames = if ich > 0 && !input.is_empty() { m } else { 0 };
            self.core.input_channels = ich;
            let (l, rest) = scratch.split_at_mut(m);
            let r = &mut rest[..m];
            let (cl, cr) = (&mut self.cue_l[..m], &mut self.cue_r[..m]);
            self.core.render(l, r, cl, cr, 0);
            for f in 0..m {
                let o = &mut out[(done + f) * och..(done + f + 1) * och];
                if och == 1 {
                    o[0] = (l[f] + r[f]) * 0.5;
                } else {
                    o[0] = l[f];
                    o[1] = r[f];
                    if och >= 4 {
                        o[2] = cl[f];
                        o[3] = cr[f];
                        o[4..].fill(0.0);
                    } else {
                        o[2..].fill(0.0);
                    }
                }
            }
            done += m;
        }
        self.flush_outbox();
    }

    /// Render interleaved audio with `channels` channels (no input).
    /// `scratch` must hold at least 2 * frames samples.
    pub fn process_interleaved(&mut self, out: &mut [f32], channels: usize, scratch: &mut [f32]) {
        self.process_io(&[], 0, out, channels, scratch);
    }
}
