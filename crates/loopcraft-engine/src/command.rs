//! Messages from the UI / control threads to the audio thread.

use crate::clip::RtClip;
use crate::engine::Core;
use loopcraft_core::{Beats, Note, Quantization};

/// Addresses a mixer channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackRef {
    Track(usize),
    Return(usize),
    Master,
}

/// Addresses a device on a channel. Index 0 of `Effect` is the first effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRef {
    Instrument,
    Effect(usize),
}

/// Preallocated storage the audio thread fills while recording MIDI, then
/// hands back through the event queue. Allocated on the UI thread so the
/// audio thread never allocates.
pub struct RecordBuffer {
    pub track: usize,
    pub slot: usize,
    pub length: Beats,
    /// Song beat where recording began (filled in by the engine).
    pub start: Beats,
    pub notes: Vec<Note>,
    /// Start beat and velocity of notes currently held, indexed by pitch.
    pub(crate) held: [Option<(Beats, u8)>; 128],
}

impl RecordBuffer {
    pub fn new(track: usize, slot: usize, length: Beats) -> Box<Self> {
        Box::new(Self { track, slot, length, start: 0.0, notes: Vec::with_capacity(4096), held: [None; 128] })
    }
}

pub enum Command {
    Play,
    /// First press stops; the UI usually sends `Seek(0.0)` on a second press.
    Stop,
    Seek(Beats),
    SetTempo(f64),
    SetQuantization(Quantization),
    SetLoop(Option<(Beats, Beats)>),
    SetMetronome(bool),

    LaunchClip {
        track: usize,
        slot: usize,
    },
    /// Launch with clip position 0 aligned to song beat `at` (which may be
    /// slightly in the past), e.g. to loop a just-recorded clip seamlessly.
    LaunchClipAt {
        track: usize,
        slot: usize,
        at: Beats,
    },
    /// Release for clips in Gate launch mode.
    ReleaseClip {
        track: usize,
        slot: usize,
    },
    StopTrack {
        track: usize,
    },
    LaunchScene {
        scene: usize,
    },
    StopAllClips,
    BackToArrangement,

    SetVolume {
        channel: TrackRef,
        db: f32,
    },
    SetPan {
        channel: TrackRef,
        pan: f32,
    },
    SetMute {
        channel: TrackRef,
        mute: bool,
    },
    SetSolo {
        track: usize,
        solo: bool,
    },
    SetSend {
        track: usize,
        ret: usize,
        db: f32,
    },
    SetDeviceParam {
        channel: TrackRef,
        device: DeviceRef,
        param: usize,
        value: f32,
    },
    SetDeviceBypass {
        channel: TrackRef,
        device: DeviceRef,
        bypass: bool,
    },

    /// Live input, e.g. from a MIDI keyboard or the on-screen keys.
    NoteOn {
        track: usize,
        note: u8,
        velocity: u8,
    },
    NoteOff {
        track: usize,
        note: u8,
    },

    /// Record live notes into a clip slot, starting at the next quantize point.
    RecordMidi(Box<RecordBuffer>),

    /// Replace (or clear) a session clip. The old clip comes back as garbage.
    ReplaceClip {
        track: usize,
        slot: usize,
        clip: Option<Box<RtClip>>,
    },
    /// Swap the whole engine graph (after adding tracks, loading a project...).
    /// Transport position and tempo carry over.
    SwapCore(Box<Core>),
}

/// Messages from the audio thread back to the control side.
pub enum EngineEvent {
    /// Finished MIDI recording, ready to be written into the project.
    Recorded(Box<RecordBuffer>),
    /// Something the audio thread replaced; drop it here, off the audio thread.
    Garbage(Garbage),
}

pub enum Garbage {
    Clip(Box<RtClip>),
    Core(Box<Core>),
}
