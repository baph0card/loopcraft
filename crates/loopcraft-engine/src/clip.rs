//! Real-time clip representation and MIDI event scheduling.

use loopcraft_core::{Beats, Clip, ClipContent, LaunchMode, Note, Quantization, WarpMode, db_to_gain};
use loopcraft_dsp::util::Noise;
use loopcraft_dsp::{AudioBuffer, SampleLoader};
use std::sync::Arc;

pub struct RtAudio {
    pub buf: Option<Arc<AudioBuffer>>,
    pub warp: WarpMode,
    pub original_bpm: f64,
    pub start_offset: Beats,
    pub gain: f32,
}

pub enum RtContent {
    Midi(Vec<Note>),
    Audio(RtAudio),
}

/// A clip prepared for the audio thread (notes sorted, audio decoded).
pub struct RtClip {
    pub name: String,
    pub length: Beats,
    pub looping: bool,
    pub launch_mode: LaunchMode,
    pub quantization: Option<Quantization>,
    pub content: RtContent,
}

impl RtClip {
    pub fn from_clip(c: &Clip, loader: &dyn SampleLoader) -> Self {
        let content = match &c.content {
            ClipContent::Midi { notes } => {
                let mut n = notes.clone();
                n.sort_by(|a, b| a.start.total_cmp(&b.start));
                RtContent::Midi(n)
            }
            ClipContent::Audio(a) => RtContent::Audio(RtAudio {
                buf: loader.load(&a.file),
                warp: a.warp,
                original_bpm: a.original_bpm.max(1.0),
                start_offset: a.start_offset,
                gain: db_to_gain(a.gain_db),
            }),
        };
        Self { name: c.name.clone(), length: c.length.max(1e-3), looping: c.looping, launch_mode: c.launch_mode, quantization: c.quantization, content }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MidiEvent {
    pub frame: u32,
    pub on: bool,
    pub note: u8,
    pub velocity: u8,
    /// Chance the note-on survives `EventList::resolve` (1.0 = always).
    pub probability: f32,
}

impl MidiEvent {
    pub fn on(frame: u32, note: u8, velocity: u8) -> Self {
        Self { frame, on: true, note, velocity, probability: 1.0 }
    }
    pub fn off(frame: u32, note: u8) -> Self {
        Self { frame, on: false, note, velocity: 0, probability: 1.0 }
    }

    /// Sort key: by frame, with note-offs before note-ons at the same frame
    /// so a note that ends where it restarts is retriggered cleanly.
    pub fn key(&self) -> (u32, bool) {
        (self.frame, self.on)
    }
}

/// Fixed-capacity event list. Pushing past capacity drops events rather
/// than allocating on the audio thread.
pub struct EventList {
    events: Vec<MidiEvent>,
}

impl EventList {
    pub const CAPACITY: usize = 1024;

    pub fn new() -> Self {
        Self { events: Vec::with_capacity(Self::CAPACITY) }
    }

    #[inline]
    pub fn push(&mut self, e: MidiEvent) {
        if self.events.len() < Self::CAPACITY {
            self.events.push(e);
        }
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    pub fn sort(&mut self) {
        self.events.sort_unstable_by_key(MidiEvent::key);
    }

    pub fn as_slice(&self) -> &[MidiEvent] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Drop note-offs for notes that aren't sounding and note-ons that lose
    /// their probability roll, updating `active` as we go.
    pub fn resolve(&mut self, active: &mut [bool; 128], rng: &mut Noise) {
        self.sort();
        self.events.retain(|e| {
            let n = e.note as usize & 127;
            if e.on {
                let p = e.probability;
                if p < 1.0 && rng.unit() >= p {
                    return false;
                }
                active[n] = true;
                true
            } else if active[n] {
                active[n] = false;
                true
            } else {
                false
            }
        });
    }
}

impl Default for EventList {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a clip sits in song time.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    /// Song beat at which clip position 0 occurs.
    pub start: Beats,
    /// Song beat where playback must end (arrangement region end), if any.
    pub end: Option<Beats>,
}

/// Result of scheduling one block of a clip.
#[derive(Debug, PartialEq, Eq)]
pub enum ClipStatus {
    Playing,
    /// The clip finished (one-shot end or region end) at this frame.
    Ended(u32),
}

/// Emit note events for song-beat range `[b0, b1)` covering `frames` samples.
///
/// Note-ons are emitted when their start falls in `[s, e)` of a loop pass;
/// note-offs when their (loop-clamped) end falls in `(s, e]`, so a note that
/// ends exactly on the loop point is released before the loop restarts.
#[allow(clippy::too_many_arguments)]
pub fn schedule_midi(notes: &[Note], length: Beats, looping: bool, place: Placement, b0: Beats, b1: Beats, frames: u32, out: &mut EventList) -> ClipStatus {
    let len = length.max(1e-6);
    let bpf = (b1 - b0) / frames.max(1) as f64;
    let frame_of = |beat: Beats| -> u32 { (((beat - b0) / bpf).max(0.0) as u32).min(frames.saturating_sub(1)) };

    let mut end = b1;
    let mut stop_at: Option<Beats> = None;
    if let Some(e) = place.end {
        if e <= b1 {
            end = end.min(e);
            stop_at = Some(e);
        }
    }
    if !looping {
        let one_shot_end = place.start + len;
        if one_shot_end <= b1 {
            end = end.min(one_shot_end);
            stop_at = Some(stop_at.map_or(one_shot_end, |s: Beats| s.min(one_shot_end)));
        }
    }

    // Boundaries are compared with a small tolerance so that a note sitting
    // exactly on a chunk / loop / launch boundary is never lost to
    // floating-point noise. Consecutive chunks share the same boundary value,
    // so the shifted half-open ranges still tile without gaps or overlaps.
    const TOL: f64 = 1e-7;
    let mut cur = if b0 < place.start + TOL { place.start } else { b0 };
    while cur < end - TOL {
        let local = cur - place.start;
        let pass = if looping { ((local + TOL) / len).floor() } else { 0.0 };
        let pass_start = place.start + pass * len;
        let seg_end = end.min(pass_start + len);
        let s = cur - pass_start;
        let e = seg_end - pass_start;
        let (s_lo, e_lo) = (if s < TOL { -TOL } else { s - TOL }, e - TOL);
        for n in notes {
            if n.start >= len {
                continue;
            }
            if n.start >= s_lo && n.start < e_lo {
                out.push(MidiEvent { probability: n.probability, ..MidiEvent::on(frame_of(pass_start + n.start), n.pitch, n.velocity) });
            }
            let ne = (n.start + n.length).min(len);
            if ne > s_lo.max(TOL) && ne <= e_lo + if seg_end >= pass_start + len - TOL { TOL * 2.0 } else { 0.0 } {
                out.push(MidiEvent::off(frame_of(pass_start + ne), n.pitch));
            }
        }
        if seg_end <= cur {
            break;
        }
        cur = seg_end;
    }

    match stop_at {
        Some(at) => {
            let f = frame_of(at);
            // Release anything still held; `resolve` filters inactive notes.
            for pitch in 0..128u8 {
                out.push(MidiEvent::off(f, pitch));
            }
            ClipStatus::Ended(f)
        }
        None => ClipStatus::Playing,
    }
}

/// Map a clip-local beat to a source sample position for audio clips.
#[inline]
pub fn audio_source_pos(a: &RtAudio, local: Beats, song_tempo: f64, sample_rate: f64) -> f64 {
    match a.warp {
        // Beat-locked: one clip beat is one beat of the original recording.
        WarpMode::Repitch | WarpMode::Stretch => (a.start_offset + local) * 60.0 / a.original_bpm * sample_rate,
        // Free-running: clip time advances in real seconds at the song tempo.
        WarpMode::Off => (a.start_offset * 60.0 / a.original_bpm + local * 60.0 / song_tempo) * sample_rate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(notes: &[Note], len: Beats, looping: bool, place: Placement, b0: Beats, b1: Beats, frames: u32) -> (Vec<MidiEvent>, ClipStatus) {
        let mut ev = EventList::new();
        let st = schedule_midi(notes, len, looping, place, b0, b1, frames, &mut ev);
        let mut active = [false; 128];
        ev.resolve(&mut active, &mut Noise::default());
        (ev.as_slice().to_vec(), st)
    }

    #[test]
    fn note_on_and_off_positions() {
        let notes = [Note::new(60, 0.5, 0.25, 100)];
        let place = Placement { start: 0.0, end: None };
        // 1 beat over 100 frames.
        let (ev, st) = run(&notes, 4.0, true, place, 0.0, 1.0, 100);
        assert_eq!(st, ClipStatus::Playing);
        assert_eq!(ev, vec![MidiEvent::on(50, 60, 100), MidiEvent::off(75, 60),]);
    }

    #[test]
    fn loop_wraps_and_releases_held_note() {
        // Note fills the whole 1-beat loop; it must be released and
        // retriggered at the loop point (frame 50 of a 2-beat block).
        let notes = [Note::new(36, 0.0, 1.0, 100)];
        let place = Placement { start: 0.0, end: None };
        let (ev, _) = run(&notes, 1.0, true, place, 0.0, 2.0, 100);
        let kinds: Vec<(u32, bool)> = ev.iter().map(|e| (e.frame, e.on)).collect();
        assert_eq!(kinds, vec![(0, true), (50, false), (50, true), (99, false)]);
    }

    #[test]
    fn launch_offset_aligns_clip_start() {
        let notes = [Note::new(60, 0.0, 0.5, 100)];
        // Clip launched at beat 4; block covers beats 3.5..4.5.
        let place = Placement { start: 4.0, end: None };
        let (ev, _) = run(&notes, 4.0, true, place, 3.5, 4.5, 100);
        assert_eq!(ev[0].frame, 50);
        assert!(ev[0].on);
    }

    #[test]
    fn one_shot_ends() {
        let notes = [Note::new(60, 0.0, 4.0, 100)];
        let place = Placement { start: 0.0, end: None };
        let (ev, st) = run(&notes, 2.0, false, place, 1.0, 3.0, 100);
        assert_eq!(st, ClipStatus::Ended(50));
        // Only the release of the note that was started earlier... which this
        // block never saw start, so `resolve` drops it: no events.
        assert!(ev.is_empty());
    }

    #[test]
    fn region_end_cuts_notes() {
        let notes = [Note::new(60, 0.0, 4.0, 100)];
        let place = Placement { start: 0.0, end: Some(1.0) };
        let (ev, st) = run(&notes, 4.0, true, place, 0.0, 2.0, 100);
        assert_eq!(st, ClipStatus::Ended(50));
        assert_eq!(ev.iter().map(|e| (e.frame, e.on)).collect::<Vec<_>>(), vec![(0, true), (50, false)]);
    }
}
