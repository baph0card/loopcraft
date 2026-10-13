//! Real-time clip representation and MIDI event scheduling.

use loopcraft_core::*;
pub use loopcraft_dsp::midi::{EventList, MidiEvent};
use loopcraft_dsp::stretch::detect_transients;
use loopcraft_dsp::{AudioBuffer, SampleLoader};
use std::sync::Arc;

pub struct RtAudio {
    pub buf: Option<Arc<AudioBuffer>>,
    /// The saved settings (warp markers, offsets, fades...).
    pub spec: AudioClip,
    pub gain: f32,
    pub pitch_ratio: f64,
    /// Transient positions (samples) for Beats mode.
    pub slices: Arc<Vec<usize>>,
}

pub enum RtContent {
    Midi(Vec<Note>),
    Audio(RtAudio),
}

/// A clip prepared for the audio thread (notes sorted, groove applied,
/// audio decoded and analysed).
pub struct RtClip {
    pub name: String,
    pub length: Beats,
    pub loop_start: Beats,
    pub looping: bool,
    pub launch_mode: LaunchMode,
    pub quantization: Option<Quantization>,
    pub follow: Option<FollowAction>,
    pub content: RtContent,
}

impl RtClip {
    pub fn from_clip(c: &Clip, loader: &dyn SampleLoader) -> Self {
        Self::from_clip_with(c, loader, &[])
    }

    /// Build with the project's groove pool, so the clip's groove (if any)
    /// is baked into note timing and velocity.
    pub fn from_clip_with(c: &Clip, loader: &dyn SampleLoader, grooves: &[Groove]) -> Self {
        let content = match &c.content {
            ClipContent::Midi { notes } => {
                let groove = c.groove.and_then(|g| grooves.get(g));
                let mut n: Vec<Note> = notes
                    .iter()
                    .map(|note| {
                        let mut note = *note;
                        if let Some(g) = groove {
                            let (dt, dv) = g.apply(note.start);
                            note.start = (note.start + dt).max(0.0);
                            note.velocity = ((note.velocity as f32 * dv).round() as i32).clamp(1, 127) as u8;
                        }
                        note
                    })
                    .collect();
                n.sort_by(|a, b| a.start.total_cmp(&b.start));
                RtContent::Midi(n)
            }
            ClipContent::Audio(a) => {
                let buf = loader.load(&a.file);
                let slices = match (&buf, a.warp) {
                    (Some(b), WarpMode::Beats) => Arc::new(detect_transients(b, 40.0)),
                    _ => Arc::new(vec![0]),
                };
                RtContent::Audio(RtAudio { buf, gain: db_to_gain(a.gain_db), pitch_ratio: 2f64.powf(a.transpose as f64 / 12.0), slices, spec: a.clone() })
            }
        };
        let length = c.length.max(1e-3);
        Self {
            name: c.name.clone(),
            length,
            loop_start: c.loop_start.clamp(0.0, length - 1e-3),
            looping: c.looping,
            launch_mode: c.launch_mode,
            quantization: c.quantization,
            follow: c.follow.filter(|f| f.enabled),
            content,
        }
    }

    /// Clip-local position for `t` beats after the clip started.
    pub fn local(&self, t: Beats) -> Beats {
        local_pos(t, self.length, self.loop_start, self.looping)
    }
}

/// Map time since clip start to a clip-local position: plays 0..len once,
/// then (when looping) repeats loop_start..len.
#[inline]
pub fn local_pos(t: Beats, len: Beats, loop_start: Beats, looping: bool) -> Beats {
    let t = t.max(0.0);
    if !looping || t < len {
        return t;
    }
    let l = (len - loop_start).max(1e-6);
    loop_start + (t - len) % l
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

/// (song offset of the pass start, clip-local position at pass start, pass end local)
#[inline]
fn pass_at(t: Beats, len: Beats, ls: Beats, looping: bool) -> (Beats, Beats) {
    if !looping || t < len {
        return (0.0, 0.0);
    }
    let l = (len - ls).max(1e-6);
    let k = ((t - len) / l).floor();
    (len + k * l, ls)
}

/// Emit note events for song-beat range `[b0, b1)` covering `frames` samples.
///
/// Note-ons are emitted when their start falls in `[s, e)` of a loop pass;
/// note-offs when their (loop-clamped) end falls in `(s, e]`, so a note that
/// ends exactly on the loop point is released before the loop restarts.
#[allow(clippy::too_many_arguments)]
pub fn schedule_midi(notes: &[Note], clip: &RtClip, place: Placement, b0: Beats, b1: Beats, frames: u32, out: &mut EventList) -> ClipStatus {
    let len = clip.length.max(1e-6);
    let ls = clip.loop_start;
    let looping = clip.looping;
    let bpf = (b1 - b0) / frames.max(1) as f64;
    let frame_of = |beat: Beats| -> u32 { (((beat - b0) / bpf).max(0.0) as u32).min(frames.saturating_sub(1)) };

    let mut end = b1;
    let mut stop_at: Option<Beats> = None;
    if let Some(e) = place.end
        && e <= b1
    {
        end = end.min(e);
        stop_at = Some(e);
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
        let (ps, pl) = pass_at(cur - place.start + TOL, len, ls, looping);
        let pass_song = place.start + ps;
        let pass_end_song = pass_song + (len - pl);
        let seg_end = end.min(pass_end_song);
        let s = pl + (cur - pass_song);
        let e = pl + (seg_end - pass_song);
        let to_song = |x: Beats| pass_song + (x - pl);
        let (s_lo, e_lo) = (if (s - pl) < TOL { pl - TOL } else { s - TOL }, e - TOL);
        let reaches_end = seg_end >= pass_end_song - TOL;
        for n in notes {
            if n.start >= len {
                continue;
            }
            if n.start >= s_lo && n.start < e_lo && n.start >= pl - TOL {
                out.push(MidiEvent { probability: n.probability, ..MidiEvent::on(frame_of(to_song(n.start)), n.pitch, n.velocity) });
            }
            let ne = (n.start + n.length).min(len);
            if ne > s_lo.max(pl + TOL) && ne <= e_lo + if reaches_end { TOL * 2.0 } else { 0.0 } {
                out.push(MidiEvent::off(frame_of(to_song(ne)), n.pitch));
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
    let pos = match a.spec.warp {
        // Free-running: clip time advances in real seconds at the song tempo.
        WarpMode::Off => (a.spec.seconds_at(0.0) + local * 60.0 / song_tempo) * sample_rate,
        // Beat-locked (optionally through warp markers).
        _ => a.spec.seconds_at(local) * sample_rate,
    };
    if a.spec.reverse {
        let len = a.buf.as_ref().map_or(0, |b| b.len()) as f64;
        len - 1.0 - pos
    } else {
        pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcraft_dsp::NoSamples;
    use loopcraft_dsp::util::Noise;

    fn clip(notes: &[Note], len: Beats, looping: bool) -> (Vec<Note>, RtClip) {
        let c = RtClip::from_clip(&Clip { looping, ..Clip::midi("t", len, notes.to_vec()) }, &NoSamples);
        let n = match &c.content {
            RtContent::Midi(n) => n.clone(),
            _ => unreachable!(),
        };
        (n, c)
    }

    fn run(notes: &[Note], len: Beats, looping: bool, place: Placement, b0: Beats, b1: Beats, frames: u32) -> (Vec<MidiEvent>, ClipStatus) {
        let (n, c) = clip(notes, len, looping);
        let mut ev = EventList::new();
        let st = schedule_midi(&n, &c, place, b0, b1, frames, &mut ev);
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

    #[test]
    fn loop_start_skips_the_intro_on_repeats() {
        // 4-beat clip, loop from beat 2: intro note at 0 plays once, loop note at 2 repeats every 2 beats.
        let notes = [Note::new(60, 0.0, 0.5, 100), Note::new(64, 2.0, 0.5, 100)];
        let (n, mut c) = clip(&notes, 4.0, true);
        c.loop_start = 2.0;
        let mut ev = EventList::new();
        // 10 beats at 10 frames per beat.
        schedule_midi(&n, &c, Placement { start: 0.0, end: None }, 0.0, 10.0, 100, &mut ev);
        let ons: Vec<(u32, u8)> = ev.as_slice().iter().filter(|e| e.on).map(|e| (e.frame, e.note)).collect();
        assert_eq!(ons, vec![(0, 60), (20, 64), (40, 64), (60, 64), (80, 64)]);
        assert_eq!(local_pos(5.0, 4.0, 2.0, true), 3.0);
        assert_eq!(local_pos(6.5, 4.0, 2.0, true), 2.5);
    }

    #[test]
    fn groove_shifts_offbeat_sixteenths() {
        let notes = [Note::new(60, 0.0, 0.1, 100), Note::new(60, 0.25, 0.1, 100)];
        let c = Clip { groove: Some(0), ..Clip::midi("g", 1.0, notes.to_vec()) };
        let rt = RtClip::from_clip_with(&c, &NoSamples, &[Groove::swing("s", 0.66)]);
        let RtContent::Midi(n) = &rt.content else { panic!() };
        assert_eq!(n[0].start, 0.0);
        assert!((n[1].start - (0.25 + 0.32 * 0.25)).abs() < 1e-3, "{}", n[1].start);
    }
}
