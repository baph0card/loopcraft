//! Messages between the control thread and the audio thread.

use crate::clip::RtClip;
use crate::engine::Core;
use loopcraft_core::{Beats, Note, Quantization};
use rtrb::Producer;

/// Addresses a mixer channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackRef {
    Track(usize),
    Return(usize),
    Master,
}

/// Addresses a device on a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRef {
    Instrument,
    Effect(usize),
    MidiEffect(usize),
}

/// Where a recording ends up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecTarget {
    Slot(usize),
    Arrangement,
}

/// When a recording starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecStart {
    /// At the next launch-quantization boundary (immediately if stopped).
    Quantized,
    /// Right now.
    Now,
}

/// Preallocated storage the audio thread fills while recording MIDI, then
/// hands back through the event queue. Allocated on the UI thread so the
/// audio thread never allocates.
pub struct RecordBuffer {
    pub track: usize,
    pub target: RecTarget,
    /// Fixed length, or `None` to record until `StopRecording`.
    pub length: Option<Beats>,
    /// Record on top of the clip already playing in the slot: note times
    /// are relative to that clip's start, wrapped to its length.
    pub overdub: Option<Beats>,
    /// Song beat where recording began (filled in by the engine).
    pub start: Beats,
    /// Final length (filled in by the engine).
    pub recorded_length: Beats,
    pub notes: Vec<Note>,
    /// Start beat and velocity of notes currently held, indexed by pitch.
    pub(crate) held: [Option<(Beats, u8)>; 128],
}

impl RecordBuffer {
    pub fn new(track: usize, slot: usize, length: Beats) -> Box<Self> {
        Self::with(track, RecTarget::Slot(slot), Some(length))
    }

    pub fn with(track: usize, target: RecTarget, length: Option<Beats>) -> Box<Self> {
        Box::new(Self { track, target, length, overdub: None, start: 0.0, recorded_length: 0.0, notes: Vec::with_capacity(8192), held: [None; 128] })
    }

    /// Back-compat accessor for slot recordings.
    pub fn slot(&self) -> Option<usize> {
        if let RecTarget::Slot(s) = self.target { Some(s) } else { None }
    }
}

/// An audio recording in progress. Samples (stereo interleaved) stream to
/// the UI through `tx`; the UI keeps the consumer end and writes the file.
pub struct AudioRecord {
    pub track: usize,
    pub target: RecTarget,
    pub length: Option<Beats>,
    pub tx: Producer<f32>,
    /// Filled in by the engine.
    pub start: Beats,
    pub recorded_length: Beats,
    pub frames: u64,
    /// Samples that didn't fit in the ring (UI fell behind).
    pub dropped: u64,
}

pub enum Command {
    Play,
    /// Start playback after the project's count-in.
    PlayWithCountIn {
        bars: u32,
    },
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
    SetArm {
        track: usize,
        armed: bool,
    },
    SetMonitor {
        track: usize,
        monitor: loopcraft_core::Monitor,
    },
    SetCrossfader(f32),
    /// Solo buttons route to the cue output instead of muting other tracks.
    SetCueMode(bool),
    SetCueVolume(f32),
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

    /// Record live notes, starting at `start`.
    RecordMidi(Box<RecordBuffer>, RecStart),
    /// Record a track's input, starting at `start`.
    RecordAudio(Box<AudioRecord>, RecStart),
    /// Finish open-ended recordings on a track at the next quantize point
    /// (`None` = all tracks, immediately).
    StopRecording {
        track: Option<usize>,
    },

    /// Replace (or clear) a session clip. The old clip comes back as garbage.
    ReplaceClip {
        track: usize,
        slot: usize,
        clip: Option<Box<RtClip>>,
    },
    /// Swap the whole engine graph (after adding tracks, loading a project...).
    /// Transport position, tempo and playing clips carry over.
    SwapCore(Box<Core>),
}

/// Messages from the audio thread back to the control side.
pub enum EngineEvent {
    /// Finished MIDI recording, ready to be written into the project.
    Recorded(Box<RecordBuffer>),
    /// Finished audio recording; its samples were streamed through `tx`.
    AudioRecorded(Box<AudioRecord>),
    /// Something the audio thread replaced; drop it here, off the audio thread.
    Garbage(Garbage),
}

pub enum Garbage {
    Clip(Box<RtClip>),
    Core(Box<Core>),
}
