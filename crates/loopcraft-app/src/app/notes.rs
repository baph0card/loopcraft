//! Piano-roll editing on a selection of notes: select (all / by
//! rectangle), delete, copy and paste, duplicate, nudge, transpose,
//! quantize, velocity.

use super::{App, ClipRef};
use loopcraft_core::*;
use std::collections::BTreeSet;

/// What a drag in the piano roll is doing.
#[derive(Clone, Debug)]
pub enum PianoDragMode {
    /// Moving the selected notes; holds their starting state.
    Move,
    /// Dragging the selected notes' right edges.
    Resize,
    /// Rubber-band selection from the press point.
    Marquee { additive: bool },
}

#[derive(Clone, Debug)]
pub struct PianoDrag {
    pub mode: PianoDragMode,
    /// (note index, note before the drag) for every selected note.
    pub origs: Vec<(usize, Note)>,
    pub beat0: f64,
    pub pitch0: i32,
}

impl App {
    pub(crate) fn notes(&self, r: ClipRef) -> Option<&Vec<Note>> {
        match &self.clip(r)?.content {
            ClipContent::Midi { notes } => Some(notes),
            _ => None,
        }
    }

    pub(crate) fn notes_mut(&mut self, r: ClipRef) -> Option<&mut Vec<Note>> {
        match &mut self.clip_mut(r)?.content {
            ClipContent::Midi { notes } => Some(notes),
            _ => None,
        }
    }

    /// Selected note indices that still exist.
    pub fn selected_notes(&self, r: ClipRef) -> Vec<usize> {
        let n = self.notes(r).map_or(0, |v| v.len());
        self.piano_sel.iter().copied().filter(|&i| i < n).collect()
    }

    /// The selection, or every note when nothing is selected.
    fn targets(&self, r: ClipRef) -> Vec<usize> {
        let s = self.selected_notes(r);
        if s.is_empty() { (0..self.notes(r).map_or(0, |v| v.len())).collect() } else { s }
    }

    pub fn select_all_notes(&mut self, r: ClipRef) {
        self.piano_sel = (0..self.notes(r).map_or(0, |v| v.len())).collect();
    }

    /// Select notes overlapping beats `b0..b1` and pitches `p0..=p1`.
    pub fn select_notes_in(&mut self, r: ClipRef, b0: f64, b1: f64, p0: i32, p1: i32, additive: bool) {
        let (b0, b1) = (b0.min(b1), b0.max(b1));
        let (p0, p1) = (p0.min(p1), p0.max(p1));
        let hit: BTreeSet<usize> = self
            .notes(r)
            .map(|ns| {
                ns.iter()
                    .enumerate()
                    .filter(|(_, n)| (n.pitch as i32) >= p0 && (n.pitch as i32) <= p1 && n.start < b1 && n.start + n.length > b0)
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
        if additive {
            self.piano_sel.extend(hit);
        } else {
            self.piano_sel = hit;
        }
    }

    pub fn delete_selected_notes(&mut self, r: ClipRef) {
        let mut sel = self.selected_notes(r);
        if sel.is_empty() {
            return;
        }
        self.checkpoint();
        sel.sort_unstable_by(|a, b| b.cmp(a));
        if let Some(ns) = self.notes_mut(r) {
            for i in sel {
                ns.remove(i);
            }
        }
        self.piano_sel.clear();
        self.commit_clip(r);
    }

    /// Copy the selected notes, relative to the earliest one.
    pub fn copy_notes(&mut self, r: ClipRef) -> bool {
        let sel = self.selected_notes(r);
        let Some(ns) = self.notes(r) else { return false };
        if sel.is_empty() {
            return false;
        }
        let first = sel.iter().map(|&i| ns[i].start).fold(f64::INFINITY, f64::min);
        let copied: Vec<Note> = sel.iter().map(|&i| Note { start: ns[i].start - first, ..ns[i] }).collect();
        self.status = format!("Copied {} notes", copied.len());
        self.note_clipboard = copied;
        true
    }

    /// Paste copied notes starting at `at` (beats into the clip); the
    /// pasted notes become the selection. Extends the clip if needed.
    pub fn paste_notes(&mut self, r: ClipRef, at: f64) -> bool {
        if self.note_clipboard.is_empty() || self.notes(r).is_none() {
            return false;
        }
        self.checkpoint();
        let clip = self.note_clipboard.clone();
        let mut new = BTreeSet::new();
        let mut end: f64 = 0.0;
        if let Some(ns) = self.notes_mut(r) {
            for n in clip {
                let m = Note { start: (n.start + at).max(0.0), ..n };
                end = end.max(m.start + m.length);
                ns.push(m);
                new.insert(ns.len() - 1);
            }
        }
        if let Some(c) = self.clip_mut(r)
            && end > c.length
        {
            c.length = (end / 1.0).ceil();
        }
        self.piano_sel = new;
        self.commit_clip(r);
        true
    }

    /// Duplicate the selection right after itself (by the selection's span,
    /// rounded up to the grid).
    pub fn duplicate_selected_notes(&mut self, r: ClipRef, grid: f64) {
        let sel = self.selected_notes(r);
        let Some(ns) = self.notes(r) else { return };
        if sel.is_empty() {
            return;
        }
        let first = sel.iter().map(|&i| ns[i].start).fold(f64::INFINITY, f64::min);
        let last = sel.iter().map(|&i| ns[i].start + ns[i].length).fold(0.0, f64::max);
        let span = ((last - first) / grid).ceil().max(1.0) * grid;
        if !self.copy_notes(r) {
            return;
        }
        self.paste_notes(r, first + span);
    }

    /// Move (beats) and transpose (semitones) the selection, or every note.
    pub fn nudge_notes(&mut self, r: ClipRef, beats: f64, semis: i32) {
        let t = self.targets(r);
        if t.is_empty() {
            return;
        }
        self.checkpoint();
        if let Some(ns) = self.notes_mut(r) {
            // Keep the group's shape: clamp the whole move, not each note.
            let min_start = t.iter().map(|&i| ns[i].start).fold(f64::INFINITY, f64::min);
            let beats = beats.max(-min_start);
            let (lo, hi) = t.iter().fold((127i32, 0i32), |(lo, hi), &i| (lo.min(ns[i].pitch as i32), hi.max(ns[i].pitch as i32)));
            let semis = semis.clamp(-lo, 127 - hi);
            for &i in &t {
                ns[i].start += beats;
                ns[i].pitch = (ns[i].pitch as i32 + semis) as u8;
            }
        }
        self.commit_clip(r);
    }

    pub fn transpose_notes(&mut self, r: ClipRef, semis: i32) {
        self.nudge_notes(r, 0.0, semis);
    }

    /// Quantize note starts (and optionally ends) to `grid`, by `amount` 0..1.
    pub fn quantize_notes(&mut self, r: ClipRef, grid: Beats, amount: f64, ends: bool) {
        let t = self.targets(r);
        self.checkpoint();
        if let Some(ns) = self.notes_mut(r) {
            for &i in &t {
                let n = &mut ns[i];
                let q = (n.start / grid).round() * grid;
                let end = n.start + n.length;
                n.start += (q - n.start) * amount;
                if ends {
                    let qe = ((end / grid).round() * grid).max(n.start + grid);
                    n.length = (end + (qe - end) * amount - n.start).max(1.0 / 64.0);
                } else {
                    n.length = (end - n.start).max(1.0 / 64.0);
                }
            }
        }
        self.commit_clip(r);
    }

    /// Set velocity (and chance) on the selection.
    pub fn set_selected_velocity(&mut self, r: ClipRef, velocity: Option<u8>, chance: Option<f32>) {
        let t = self.selected_notes(r);
        if let Some(ns) = self.notes_mut(r) {
            for &i in &t {
                if let Some(v) = velocity {
                    ns[i].velocity = v.clamp(1, 127);
                }
                if let Some(c) = chance {
                    ns[i].probability = c.clamp(0.0, 1.0);
                }
            }
        }
        self.dirty = true;
    }

    /// Apply a drag in progress: move or resize every selected note by the
    /// same delta from where it started.
    pub fn apply_piano_drag(&mut self, r: ClipRef, d: &PianoDrag, dbeat: f64, dpitch: i32, grid: f64, len: f64) {
        let Some(ns) = self.notes_mut(r) else { return };
        match d.mode {
            PianoDragMode::Move => {
                let min_start = d.origs.iter().map(|(_, n)| n.start).fold(f64::INFINITY, f64::min);
                let max_start = d.origs.iter().map(|(_, n)| n.start).fold(0.0, f64::max);
                let dbeat = dbeat.clamp(-min_start, (len - grid - max_start).max(-min_start));
                let (lo, hi) = d.origs.iter().fold((127i32, 0i32), |(lo, hi), (_, n)| (lo.min(n.pitch as i32), hi.max(n.pitch as i32)));
                let dpitch = dpitch.clamp(-lo, 127 - hi);
                for (i, o) in &d.origs {
                    if let Some(n) = ns.get_mut(*i) {
                        n.start = o.start + dbeat;
                        n.pitch = (o.pitch as i32 + dpitch) as u8;
                    }
                }
            }
            PianoDragMode::Resize => {
                for (i, o) in &d.origs {
                    if let Some(n) = ns.get_mut(*i) {
                        n.length = (o.length + dbeat).max(grid.min(o.length));
                    }
                }
            }
            PianoDragMode::Marquee { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (App, ClipRef) {
        let mut p = Project::default();
        p.add_scene("1");
        let t = p.add_track(Track::midi("m", DeviceSpec::PolySynth(Default::default())));
        p.tracks[t].slots[0] = Some(Clip::midi("c", 4.0, vec![Note::new(60, 0.0, 0.5, 100), Note::new(64, 1.0, 0.5, 100), Note::new(67, 2.0, 0.5, 100)]));
        (App::headless(p), ClipRef::Session { track: 0, slot: 0 })
    }

    #[test]
    fn marquee_copy_paste_duplicate_delete() {
        let (mut a, r) = app();
        a.select_notes_in(r, 0.9, 2.2, 63, 70, false);
        assert_eq!(a.selected_notes(r), vec![1, 2]);
        a.duplicate_selected_notes(r, 0.25);
        // Span 1.0..2.5 rounded to the grid is 1.5, so copies land at 2.5 and 3.5.
        let starts: Vec<f64> = a.notes(r).unwrap().iter().map(|n| n.start).collect();
        assert_eq!(starts, vec![0.0, 1.0, 2.0, 2.5, 3.5]);
        assert_eq!(a.selected_notes(r), vec![3, 4], "the copies become the selection");
        a.delete_selected_notes(r);
        assert_eq!(a.notes(r).unwrap().len(), 3);
        a.undo();
        assert_eq!(a.notes(r).unwrap().len(), 5);
    }

    #[test]
    fn nudge_keeps_shape_at_the_edges() {
        let (mut a, r) = app();
        a.select_all_notes(r);
        a.nudge_notes(r, -5.0, 100);
        let ns = a.notes(r).unwrap();
        // Clamped as a group: the earliest note stops at 0, the highest at 127.
        assert_eq!(ns.iter().map(|n| n.start).collect::<Vec<_>>(), vec![0.0, 1.0, 2.0]);
        assert_eq!(ns.iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![120, 124, 127]);
    }

    #[test]
    fn edits_apply_to_the_selection_only() {
        let (mut a, r) = app();
        a.piano_sel = [0].into_iter().collect();
        a.transpose_notes(r, 12);
        a.set_selected_velocity(r, Some(40), Some(0.5));
        let ns = a.notes(r).unwrap();
        assert_eq!((ns[0].pitch, ns[0].velocity, ns[0].probability), (72, 40, 0.5));
        assert_eq!((ns[1].pitch, ns[1].velocity), (64, 100));
        // Nothing selected: the whole clip.
        a.piano_sel.clear();
        a.transpose_notes(r, -1);
        assert_eq!(a.notes(r).unwrap().iter().map(|n| n.pitch).collect::<Vec<_>>(), vec![71, 63, 66]);
    }

    #[test]
    fn dragging_moves_the_group() {
        let (mut a, r) = app();
        a.piano_sel = [0, 1].into_iter().collect();
        let ns = a.notes(r).unwrap().clone();
        let d = PianoDrag { mode: PianoDragMode::Move, origs: vec![(0, ns[0]), (1, ns[1])], beat0: 0.0, pitch0: 60 };
        a.apply_piano_drag(r, &d, 0.5, 2, 0.25, 4.0);
        let ns = a.notes(r).unwrap();
        assert_eq!((ns[0].start, ns[0].pitch, ns[1].start, ns[1].pitch, ns[2].start), (0.5, 62, 1.5, 66, 2.0));
    }
}
