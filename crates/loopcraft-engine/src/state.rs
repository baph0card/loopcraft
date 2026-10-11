//! Lock-free state the audio thread publishes for the UI (playhead, meters,
//! which clips are playing). Readers may see values from slightly different
//! blocks, which is fine for display.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering::Relaxed};

#[derive(Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
}

#[derive(Default)]
pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Relaxed))
    }
    pub fn store(&self, v: f64) {
        self.0.store(v.to_bits(), Relaxed)
    }
}

pub const NONE: i32 = -1;

#[derive(Default)]
pub struct ChannelState {
    pub peak_l: AtomicF32,
    pub peak_r: AtomicF32,
}

impl ChannelState {
    pub fn set_peaks(&self, l: f32, r: f32) {
        // Simple peak-hold with decay: keep the max of the new peak and a
        // decayed version of the old one.
        self.peak_l.store(l.max(self.peak_l.load() * 0.85));
        self.peak_r.store(r.max(self.peak_r.load() * 0.85));
    }
}

#[derive(Default)]
pub struct TrackState {
    pub channel: ChannelState,
    /// Session slot currently playing, or NONE.
    pub playing_slot: AtomicI32,
    /// Slot waiting for its quantize boundary, or NONE.
    pub queued_slot: AtomicI32,
    /// True when a stop is waiting for its boundary.
    pub stop_queued: AtomicBool,
    /// Clip-local playback position in beats (for progress rings).
    pub clip_position: AtomicF64,
    /// Whether the track is following the arrangement timeline.
    pub follows_arrangement: AtomicBool,
    pub recording_slot: AtomicI32,
}

#[derive(Default)]
pub struct EngineState {
    pub playing: AtomicBool,
    pub beat: AtomicF64,
    pub tempo: AtomicF64,
    pub tracks: Vec<TrackState>,
    pub returns: Vec<ChannelState>,
    pub master: ChannelState,
    /// Fraction of the real-time budget used by the last block (0..1+).
    pub cpu_load: AtomicF32,
}

impl EngineState {
    pub fn new(n_tracks: usize, n_returns: usize) -> Self {
        let tracks = (0..n_tracks)
            .map(|_| {
                let t = TrackState::default();
                t.playing_slot.store(NONE, Relaxed);
                t.queued_slot.store(NONE, Relaxed);
                t.recording_slot.store(NONE, Relaxed);
                t.follows_arrangement.store(true, Relaxed);
                t
            })
            .collect();
        Self { tracks, returns: (0..n_returns).map(|_| ChannelState::default()).collect(), ..Default::default() }
    }
}
