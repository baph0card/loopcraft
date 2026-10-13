//! Time-driven events: quantized launches, follow actions, tempo changes,
//! recording punch-in/out.

use super::track::*;
use super::*;
use crate::command::RecTarget;
use crate::state::NONE;
use std::sync::atomic::Ordering::Relaxed;

/// Pick the slot a clip follow action leads to. `group` is the run of
/// filled slots around `slot`, bounded by empty ones.
fn follow_target(kind: FollowKind, slot: usize, slots: &[Option<Box<crate::clip::RtClip>>], rng: &mut loopcraft_dsp::util::Noise) -> Option<Option<usize>> {
    // Some(Some(s)) = play slot s; Some(None) = stop; None = no action.
    let filled = |i: usize| slots.get(i).is_some_and(|c| c.is_some());
    let mut lo = slot;
    while lo > 0 && filled(lo - 1) {
        lo -= 1;
    }
    let mut hi = slot;
    while filled(hi + 1) {
        hi += 1;
    }
    let count = hi - lo + 1;
    Some(Some(match kind {
        FollowKind::NoAction => return None,
        FollowKind::Stop => return Some(None),
        FollowKind::Again => slot,
        FollowKind::Previous => {
            if slot > lo {
                slot - 1
            } else {
                hi
            }
        }
        FollowKind::Next => {
            if slot < hi {
                slot + 1
            } else {
                lo
            }
        }
        FollowKind::First => lo,
        FollowKind::Last => hi,
        FollowKind::Any => lo + (rng.next_u32() as usize) % count,
        FollowKind::Other => {
            if count <= 1 {
                slot
            } else {
                let k = (rng.next_u32() as usize) % (count - 1);
                let s = lo + k;
                if s >= slot { s + 1 } else { s }
            }
        }
        FollowKind::Jump(s) => {
            if filled(s) {
                s
            } else {
                return Some(None);
            }
        }
    }))
}

fn follow_delay(f: &FollowAction, clip_len: Beats) -> Beats {
    if f.linked { clip_len } else { f.after.max(1.0 / 64.0) }
}

impl Core {
    /// Every future musical time the next chunk must stop at.
    pub(crate) fn frames_until_boundary(&self, max: usize) -> usize {
        let bpf = self.beats_per_frame();
        if let Some((done, total)) = self.count_in {
            let left = ((total - done) / bpf).ceil() as usize;
            return left.clamp(1, max);
        }
        if !self.playing {
            return max;
        }
        let horizon = self.beat + max as f64 * bpf;
        let mut next = horizon;
        // Anything within half a sample counts as "now" (handled by
        // `apply_due_events`), which absorbs floating-point drift.
        let half = 0.5 * bpf;
        let mut consider = |t: Beats| {
            if t > self.beat + half && t < next {
                next = t;
            }
        };
        if let Some((at, _)) = self.pending_tempo {
            consider(at);
        }
        if let Some((at, _)) = self.pending_sig {
            consider(at);
        }
        if let Some((at, _)) = self.scene_follow {
            consider(at);
        }
        if let Some((_, end)) = self.loop_region {
            consider(end);
        }
        if self.tempo_auto {
            for &(b, _) in &self.tempo_points {
                if b > self.beat + half {
                    consider(b);
                    break;
                }
            }
        }
        for r in &self.recordings {
            consider(r.start);
            if let Some(e) = r.end {
                consider(e);
            }
        }
        for t in &self.tracks {
            if let Some(p) = t.pending {
                consider(p.at);
            }
            if let Some(f) = t.follow_at {
                consider(f);
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
        (((next - self.beat) / bpf).round() as usize).clamp(1, max)
    }

    pub(crate) fn apply_due_events(&mut self) {
        let now = self.beat + 0.5 * self.beats_per_frame();
        if let Some((at, bpm)) = self.pending_tempo
            && at <= now
        {
            self.tempo = bpm;
            self.pending_tempo = None;
        }
        if let Some((at, sig)) = self.pending_sig
            && at <= now
        {
            self.sig = sig;
            self.pending_sig = None;
        }
        if self.tempo_auto && !self.tempo_points.is_empty() {
            self.tempo = loopcraft_core::tempo_at(&self.tempo_points, self.tempo, self.beat).clamp(20.0, 999.0);
        }

        // Scene follow actions.
        if let Some((at, scene)) = self.scene_follow
            && at <= now
        {
            self.scene_follow = None;
            let f = self.scenes.get(scene).and_then(|s| s.follow);
            if let Some(f) = f {
                let kind = if self.rng.unit() < f.chance_a { f.action_a } else { f.action_b };
                let n = self.scenes.len();
                let target = match kind {
                    FollowKind::NoAction => None,
                    FollowKind::Stop => {
                        for t in self.tracks.iter_mut().filter(|t| t.session.is_some()) {
                            t.pending = Some(Pending { kind: PendingKind::Stop, at });
                        }
                        None
                    }
                    FollowKind::Again => Some(scene),
                    FollowKind::Next => Some((scene + 1) % n),
                    FollowKind::Previous => Some((scene + n - 1) % n),
                    FollowKind::First => Some(0),
                    FollowKind::Last => Some(n - 1),
                    FollowKind::Any => Some((self.rng.next_u32() as usize) % n),
                    FollowKind::Other => Some(if n > 1 { (scene + 1 + (self.rng.next_u32() as usize) % (n - 1)) % n } else { scene }),
                    FollowKind::Jump(s) => Some(s.min(n - 1)),
                };
                if let Some(s) = target {
                    self.launch_scene_at(s, at);
                }
            }
        }

        // Quantized launches / stops.
        let st = self.state.clone();
        for (i, t) in self.tracks.iter_mut().enumerate() {
            if let Some(p) = t.pending
                && p.at <= now
            {
                t.pending = None;
                t.flush_notes = true;
                t.stretch.reset();
                match p.kind {
                    PendingKind::Launch(slot) => {
                        t.session = Some(SessionPlay { slot, start: p.at });
                        t.follows_arrangement = false;
                        t.arr_active = None;
                        t.follow_at = None;
                        if let Some(Some(c)) = t.slots.get(slot) {
                            if let Some(f) = c.follow {
                                t.follow_at = Some(p.at + follow_delay(&f, c.length));
                            }
                            if let crate::clip::RtContent::Audio(a) = &c.content {
                                t.stretch.beats.set_slices(a.slices.clone());
                            }
                        }
                    }
                    PendingKind::Stop => {
                        t.session = None;
                        t.follow_at = None;
                    }
                }
                st.tracks[i].stop_queued.store(false, Relaxed);
            }

            // Clip follow actions fire exactly on time, unquantized.
            if let (Some(at), Some(play)) = (t.follow_at, t.session)
                && at <= now
            {
                t.follow_at = None;
                let Some(Some(clip)) = t.slots.get(play.slot) else { continue };
                let Some(f) = clip.follow else { continue };
                let kind = if t.rng.unit() < f.chance_a { f.action_a } else { f.action_b };
                match follow_target(kind, play.slot, &t.slots, &mut t.rng) {
                    None => {
                        // No action: keep playing, check again after another period.
                        t.follow_at = Some(at + follow_delay(&f, clip.length));
                    }
                    Some(None) => {
                        t.session = None;
                        t.flush_notes = true;
                    }
                    Some(Some(slot)) => {
                        t.flush_notes = true;
                        t.stretch.reset();
                        t.session = Some(SessionPlay { slot, start: at });
                        if let Some(Some(c)) = t.slots.get(slot) {
                            if let Some(nf) = c.follow {
                                t.follow_at = Some(at + follow_delay(&nf, c.length));
                            }
                            if let crate::clip::RtContent::Audio(a) = &c.content {
                                t.stretch.beats.set_slices(a.slices.clone());
                            }
                        }
                    }
                }
            }
        }
        self.finish_due_recordings(now);
    }

    /// Hand back recordings whose end has been reached.
    pub(crate) fn finish_due_recordings(&mut self, now: Beats) {
        let mut i = 0;
        while i < self.recordings.len() {
            let due = self.recordings[i].end.is_some_and(|e| e <= now);
            if !due {
                i += 1;
                continue;
            }
            let r = self.recordings.swap_remove(i);
            let end = r.end.unwrap_or(self.beat);
            let len = (end - r.start).max(0.0);
            let track = r.track;
            match r.kind {
                RecKind::Midi(mut buf) => {
                    let close_at = if buf.overdub.is_some() { f64::NAN } else { len };
                    for pitch in 0..128 {
                        if let Some((start, v)) = buf.held[pitch].take()
                            && buf.notes.len() < buf.notes.capacity()
                        {
                            let l = if close_at.is_nan() { 0.25 } else { (close_at - start).max(1.0 / 64.0) };
                            buf.notes.push(Note::new(pitch as u8, start, l, v));
                        }
                    }
                    buf.notes.sort_unstable_by(|a, b| a.start.total_cmp(&b.start));
                    buf.start = r.start;
                    buf.recorded_length = len;
                    if buf.target == RecTarget::Arrangement
                        && let Some(t) = self.tracks.get_mut(track)
                    {
                        t.arr_recording = false;
                    }
                    self.post(EngineEvent::Recorded(buf));
                }
                RecKind::Audio(mut a) => {
                    a.start = r.start;
                    a.recorded_length = len;
                    if a.target == RecTarget::Arrangement
                        && let Some(t) = self.tracks.get_mut(track)
                    {
                        t.arr_recording = false;
                    }
                    self.post(EngineEvent::AudioRecorded(a));
                }
            }
            if let Some(ts) = self.state.tracks.get(track)
                && !self.recordings.iter().any(|r| r.track == track)
            {
                ts.recording.store(false, Relaxed);
                ts.recording_slot.store(NONE, Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcraft_dsp::util::Noise;

    fn slots(filled: &[bool]) -> Vec<Option<Box<crate::clip::RtClip>>> {
        filled.iter().map(|&f| f.then(|| Box::new(crate::clip::RtClip::from_clip(&Clip::default(), &loopcraft_dsp::NoSamples)))).collect()
    }

    #[test]
    fn follow_targets_respect_groups() {
        // Two groups: slots 0-2 and 4-5.
        let s = slots(&[true, true, true, false, true, true]);
        let mut rng = Noise::default();
        assert_eq!(follow_target(FollowKind::Next, 2, &s, &mut rng), Some(Some(0)), "wraps within group");
        assert_eq!(follow_target(FollowKind::Previous, 4, &s, &mut rng), Some(Some(5)));
        assert_eq!(follow_target(FollowKind::First, 5, &s, &mut rng), Some(Some(4)));
        assert_eq!(follow_target(FollowKind::Last, 0, &s, &mut rng), Some(Some(2)));
        assert_eq!(follow_target(FollowKind::Stop, 0, &s, &mut rng), Some(None));
        assert_eq!(follow_target(FollowKind::NoAction, 0, &s, &mut rng), None);
        for _ in 0..50 {
            let Some(Some(o)) = follow_target(FollowKind::Other, 1, &s, &mut rng) else { panic!() };
            assert!(o == 0 || o == 2);
            let Some(Some(a)) = follow_target(FollowKind::Any, 4, &s, &mut rng) else { panic!() };
            assert!(a == 4 || a == 5);
        }
    }
}
