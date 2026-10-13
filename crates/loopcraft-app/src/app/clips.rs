//! Clip actions: Session slots, Arrangement regions, automation, grooves,
//! locators and note editing helpers.

use super::{App, ClipRef, DetailTab};
use loopcraft_core::*;

impl App {
    pub fn new_midi_clip(&mut self, track: usize, slot: usize) {
        if self.project.tracks.get(track).is_none_or(|t| t.kind != TrackKind::Midi) || slot >= self.project.scenes.len() {
            return;
        }
        self.checkpoint();
        let color = self.project.tracks[track].color;
        let bar = self.project.time_signature.beats_per_bar();
        self.project.tracks[track].slots[slot] = Some(Clip { color, ..Clip::midi("MIDI Clip", bar, Vec::new()) });
        self.push_clip(track, slot);
        self.selected_clip = Some(ClipRef::Session { track, slot });
        self.detail_tab = DetailTab::Clip;
    }

    pub fn delete_clip(&mut self, r: ClipRef) {
        self.checkpoint();
        match r {
            ClipRef::Session { track, slot } => {
                if let Some(s) = self.project.tracks.get_mut(track).and_then(|t| t.slots.get_mut(slot)) {
                    *s = None;
                }
                self.push_clip(track, slot);
            }
            ClipRef::Arrangement { track, index } => {
                if let Some(t) = self.project.tracks.get_mut(track)
                    && index < t.arrangement.len()
                {
                    t.arrangement.remove(index);
                }
                self.rebuild();
            }
        }
        if self.selected_clip == Some(r) {
            self.selected_clip = None;
        }
    }

    pub fn duplicate_clip_down(&mut self, track: usize, slot: usize) {
        let Some(clip) = self.project.tracks.get(track).and_then(|t| t.slots.get(slot)).cloned().flatten() else { return };
        self.checkpoint();
        // Next empty slot below; add a scene if there is none.
        let target = (slot + 1..self.project.scenes.len()).find(|&s| self.project.tracks[track].slots[s].is_none());
        match target {
            Some(s) => {
                self.project.tracks[track].slots[s] = Some(clip);
                self.push_clip(track, s);
                self.selected_clip = Some(ClipRef::Session { track, slot: s });
            }
            None => {
                let s = self.project.add_scene(format!("{}", self.project.scenes.len() + 1));
                self.project.tracks[track].slots[s] = Some(clip);
                self.rebuild();
                self.selected_clip = Some(ClipRef::Session { track, slot: s });
            }
        }
    }

    /// Paste a copied clip into a slot (MIDI clips only onto MIDI tracks, etc.).
    pub fn paste_clip(&mut self, track: usize, slot: usize, clip: Clip) {
        let Some(t) = self.project.tracks.get(track) else { return };
        let ok = match clip.content {
            ClipContent::Midi { .. } => t.kind == TrackKind::Midi,
            ClipContent::Audio(_) => t.kind == TrackKind::Audio,
        };
        if !ok || slot >= self.project.scenes.len() {
            self.status = "Can't paste that clip on this track type".into();
            return;
        }
        self.checkpoint();
        self.project.tracks[track].slots[slot] = Some(clip);
        self.push_clip(track, slot);
        self.selected_clip = Some(ClipRef::Session { track, slot });
    }

    /// Copy a session clip onto the timeline at the playhead (bar-aligned).
    pub fn clip_to_arrangement(&mut self, track: usize, slot: usize) {
        let Some(clip) = self.project.tracks.get(track).and_then(|t| t.slots.get(slot)).cloned().flatten() else { return };
        self.checkpoint();
        let bar = self.project.time_signature.beats_per_bar();
        let at = (self.engine.state.beat.load() / bar).floor() * bar;
        let len = clip.length;
        self.clear_range(track, at, at + len);
        self.project.tracks[track].arrangement.push(ArrangementClip::new(at, clip));
        self.sort_arrangement(track);
        self.rebuild();
        self.status = format!("Copied to Arrangement at bar {}", (at / bar) as i64 + 1);
    }

    pub(crate) fn sort_arrangement(&mut self, track: usize) {
        if let Some(t) = self.project.tracks.get_mut(track) {
            t.arrangement.sort_by(|a, b| a.start.total_cmp(&b.start));
        }
    }

    /// Split an arrangement region at song beat `at`. Both halves share the
    /// clip; the right half starts `offset` beats into it.
    /// Returns the index of the right half. Doesn't checkpoint or rebuild.
    pub(crate) fn split_region(&mut self, track: usize, index: usize, at: Beats) -> Option<usize> {
        let t = self.project.tracks.get_mut(track)?;
        let a = t.arrangement.get(index)?.clone();
        if at <= a.start + 1e-6 || at >= a.end() - 1e-6 {
            return None;
        }
        let end = a.end();
        let mut left = a.clone();
        left.duration = Some(at - a.start);
        let mut right = a;
        right.offset += at - right.start;
        right.start = at;
        right.duration = Some(end - at);
        t.arrangement[index] = left;
        t.arrangement.insert(index + 1, right);
        Some(index + 1)
    }

    pub fn split_arrangement_clip(&mut self, track: usize, index: usize, at: Beats) {
        self.checkpoint();
        if self.split_region(track, index, at).is_some() {
            self.selected_clip = Some(ClipRef::Arrangement { track, index: index + 1 });
            self.rebuild();
            self.status = "Split clip".into();
        }
    }

    /// Remove everything on the track's timeline between `a` and `b`,
    /// splitting regions that cross the edges. Doesn't checkpoint or rebuild.
    pub(crate) fn clear_range(&mut self, track: usize, a: Beats, b: Beats) {
        if b <= a {
            return;
        }
        let mut i = 0;
        while i < self.project.tracks.get(track).map_or(0, |t| t.arrangement.len()) {
            let (s, e) = {
                let r = &self.project.tracks[track].arrangement[i];
                (r.start, r.end())
            };
            if e <= a + 1e-9 || s >= b - 1e-9 {
                i += 1;
                continue;
            }
            if s < a {
                self.split_region(track, i, a);
                i += 1;
                continue;
            }
            if e > b {
                self.split_region(track, i, b);
            }
            self.project.tracks[track].arrangement.remove(i);
        }
    }

    /// Delete the time between `a` and `b` on every track and pull
    /// everything after it left (like "Delete Time").
    pub fn delete_time(&mut self, a: Beats, b: Beats) {
        if b <= a {
            return;
        }
        self.checkpoint();
        let d = b - a;
        for t in 0..self.project.tracks.len() {
            self.clear_range(t, a, b);
            let tr = &mut self.project.tracks[t];
            for r in tr.arrangement.iter_mut().filter(|r| r.start >= b - 1e-9) {
                r.start -= d;
            }
            for lane in tr.automation.iter_mut() {
                lane.points.retain(|p| p.0 < a || p.0 >= b);
                for p in lane.points.iter_mut().filter(|p| p.0 >= b) {
                    p.0 -= d;
                }
            }
        }
        self.project.tempo_automation.retain(|p| p.0 < a || p.0 >= b);
        for p in self.project.tempo_automation.iter_mut().filter(|p| p.0 >= b) {
            p.0 -= d;
        }
        for l in self.project.locators.iter_mut().filter(|l| l.0 >= b) {
            l.0 -= d;
        }
        self.selected_clip = None;
        self.rebuild();
    }

    /// Insert `d` beats of silence at `at` on every track.
    pub fn insert_time(&mut self, at: Beats, d: Beats) {
        if d <= 0.0 {
            return;
        }
        self.checkpoint();
        for t in 0..self.project.tracks.len() {
            let idx: Vec<usize> = (0..self.project.tracks[t].arrangement.len()).collect();
            for i in idx.into_iter().rev() {
                self.split_region(t, i, at);
            }
            let tr = &mut self.project.tracks[t];
            for r in tr.arrangement.iter_mut().filter(|r| r.start >= at - 1e-9) {
                r.start += d;
            }
            for lane in tr.automation.iter_mut() {
                for p in lane.points.iter_mut().filter(|p| p.0 >= at) {
                    p.0 += d;
                }
            }
        }
        for p in self.project.tempo_automation.iter_mut().filter(|p| p.0 >= at) {
            p.0 += d;
        }
        for l in self.project.locators.iter_mut().filter(|l| l.0 >= at) {
            l.0 += d;
        }
        self.rebuild();
    }

    /// Duplicate a region right after itself, replacing what's there.
    pub fn duplicate_arrangement_clip(&mut self, track: usize, index: usize) {
        let Some(a) = self.project.tracks.get(track).and_then(|t| t.arrangement.get(index)).cloned() else { return };
        self.checkpoint();
        let mut copy = a.clone();
        copy.start = a.end();
        self.clear_range(track, copy.start, copy.end());
        self.project.tracks[track].arrangement.push(copy.clone());
        self.sort_arrangement(track);
        let i = self.project.tracks[track].arrangement.iter().position(|r| (r.start - copy.start).abs() < 1e-9).unwrap_or(0);
        self.selected_clip = Some(ClipRef::Arrangement { track, index: i });
        self.rebuild();
    }

    /// Move a region to a new start (and track, if the kinds match),
    /// replacing what it lands on. Returns its new index.
    pub fn move_arrangement_clip(&mut self, track: usize, index: usize, to_track: usize, start: Beats) -> Option<usize> {
        let kind = self.project.tracks.get(track)?.kind;
        let to_track = if self.project.tracks.get(to_track).is_some_and(|t| t.kind == kind) { to_track } else { track };
        let mut a = self.project.tracks[track].arrangement.get(index)?.clone();
        self.project.tracks[track].arrangement.remove(index);
        a.start = start.max(0.0);
        self.clear_range(to_track, a.start, a.end());
        self.project.tracks[to_track].arrangement.push(a.clone());
        self.sort_arrangement(to_track);
        let i = self.project.tracks[to_track].arrangement.iter().position(|r| (r.start - a.start).abs() < 1e-9)?;
        self.selected_clip = Some(ClipRef::Arrangement { track: to_track, index: i });
        Some(i)
    }

    /// Move the left edge of a region (trimming or revealing the clip start).
    pub(crate) fn trim_region_start(&mut self, track: usize, index: usize, new_start: Beats) {
        let Some(a) = self.project.tracks.get_mut(track).and_then(|t| t.arrangement.get_mut(index)) else { return };
        let end = a.end();
        // Can't reveal more than the clip's own start.
        let new_start = new_start.clamp(a.start - a.offset, end - 1.0 / 16.0);
        let d = new_start - a.start;
        a.offset += d;
        a.start = new_start;
        a.duration = Some(end - new_start);
    }

    /// Bake a region into a standalone clip (loops unrolled, offset applied).
    pub fn consolidate_region(&mut self, track: usize, index: usize) {
        let Some(a) = self.project.tracks.get(track).and_then(|t| t.arrangement.get(index)).cloned() else { return };
        let ClipContent::Midi { notes } = &a.clip.content else {
            self.status = "Consolidate works on MIDI clips".into();
            return;
        };
        self.checkpoint();
        let dur = a.duration();
        let c = &a.clip;
        let mut out = Vec::new();
        // Walk passes of the clip from position `offset`.
        let mut song = 0.0; // relative to region start
        let mut pos = a.offset;
        if c.looping && pos >= c.length {
            // An offset past the first pass lands somewhere in the loop.
            let span = (c.length - c.loop_start).max(1e-9);
            pos = c.loop_start + (pos - c.length) % span;
        }
        while song < dur - 1e-9 {
            let pass_end = if c.looping || pos < c.length { c.length } else { break };
            let seg = (pass_end - pos).min(dur - song);
            for n in notes.iter().filter(|n| n.start >= pos - 1e-9 && n.start < pos + seg - 1e-9) {
                let mut m = *n;
                m.start = song + (n.start - pos);
                m.length = m.length.min(dur - m.start);
                out.push(m);
            }
            song += seg;
            if !c.looping {
                break;
            }
            pos = c.loop_start;
        }
        let mut clip = c.clone();
        clip.content = ClipContent::Midi { notes: out };
        clip.length = dur;
        clip.loop_start = 0.0;
        self.project.tracks[track].arrangement[index] = ArrangementClip::new(a.start, clip);
        self.rebuild();
        self.status = "Consolidated".into();
    }

    // ------------------------------------------------------------------
    // Automation
    // ------------------------------------------------------------------

    pub fn lane_mut(&mut self, track: usize, target: AutomationTarget) -> Option<&mut AutomationLane> {
        let t = self.project.tracks.get_mut(track)?;
        if let Some(i) = t.automation.iter().position(|l| l.target == target) {
            return t.automation.get_mut(i);
        }
        t.automation.push(AutomationLane { target, points: Vec::new() });
        t.automation.last_mut()
    }

    /// Add or move a breakpoint. Replaces a point within `snap` beats.
    pub fn set_automation_point(&mut self, track: usize, target: AutomationTarget, beat: Beats, value: f32, snap: Beats) {
        let Some(lane) = self.lane_mut(track, target) else { return };
        lane.points.retain(|p| (p.0 - beat).abs() > snap);
        lane.points.push((beat.max(0.0), value));
        lane.points.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.dirty = true;
    }

    pub fn remove_automation_point(&mut self, track: usize, target: AutomationTarget, index: usize) {
        if let Some(lane) = self.lane_mut(track, target)
            && index < lane.points.len()
        {
            lane.points.remove(index);
        }
        self.dirty = true;
    }

    pub fn clear_automation(&mut self, track: usize, target: AutomationTarget) {
        self.checkpoint();
        if let Some(t) = self.project.tracks.get_mut(track) {
            t.automation.retain(|l| l.target != target);
        }
        self.rebuild();
    }

    /// Natural range of an automation target, for drawing and editing.
    pub fn automation_range(&self, track: usize, target: AutomationTarget) -> (f32, f32, String) {
        match target {
            AutomationTarget::Volume => (-60.0, 6.0, "Volume".into()),
            AutomationTarget::Pan => (-1.0, 1.0, "Pan".into()),
            AutomationTarget::Send(r) => (0.0, 1.0, format!("Send {}", self.project.returns.get(r).map(|r| r.name.as_str()).unwrap_or("?"))),
            AutomationTarget::Instrument { param } => {
                let name = self
                    .project
                    .tracks
                    .get(track)
                    .and_then(|t| t.instrument.as_ref())
                    .and_then(|s| loopcraft_dsp::specs::param_infos(s).get(param).map(|p| format!("{}: {}", s.name(), p.name)))
                    .unwrap_or_else(|| "Instrument".into());
                (0.0, 1.0, name)
            }
            AutomationTarget::Effect { index, param } => {
                let name = self
                    .project
                    .tracks
                    .get(track)
                    .and_then(|t| t.effects.get(index))
                    .and_then(|s| loopcraft_dsp::specs::param_infos(s).get(param).map(|p| format!("{}: {}", s.name(), p.name)))
                    .unwrap_or_else(|| "Effect".into());
                (0.0, 1.0, name)
            }
        }
    }

    /// Every automatable target on a track, for the lane chooser.
    pub fn automation_targets(&self, track: usize) -> Vec<(AutomationTarget, String)> {
        let mut v = vec![(AutomationTarget::Volume, "Mixer: Volume".to_string()), (AutomationTarget::Pan, "Mixer: Pan".to_string())];
        for (r, ret) in self.project.returns.iter().enumerate() {
            v.push((AutomationTarget::Send(r), format!("Mixer: Send {}", ret.name)));
        }
        let Some(t) = self.project.tracks.get(track) else { return v };
        if let Some(inst) = &t.instrument {
            for (i, p) in loopcraft_dsp::specs::param_infos(inst).iter().enumerate() {
                v.push((AutomationTarget::Instrument { param: i }, format!("{}: {}", inst.name(), p.name)));
            }
        }
        for (e, fx) in t.effects.iter().enumerate() {
            for (i, p) in loopcraft_dsp::specs::param_infos(fx).iter().enumerate() {
                v.push((AutomationTarget::Effect { index: e, param: i }, format!("{} {}: {}", e + 1, fx.name(), p.name)));
            }
        }
        v
    }

    pub fn set_tempo_point(&mut self, beat: Beats, bpm: f64, snap: Beats) {
        self.project.tempo_automation.retain(|p| (p.0 - beat).abs() > snap);
        self.project.tempo_automation.push((beat.max(0.0), bpm.clamp(20.0, 999.0)));
        self.project.tempo_automation.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.dirty = true;
    }

    // ------------------------------------------------------------------
    // Locators, grooves
    // ------------------------------------------------------------------

    pub fn add_locator(&mut self, beat: Beats) {
        self.checkpoint();
        let n = self.project.locators.len() + 1;
        self.project.locators.retain(|l| (l.0 - beat).abs() > 1e-6);
        self.project.locators.push((beat, format!("Locator {n}")));
        self.project.locators.sort_by(|a, b| a.0.total_cmp(&b.0));
    }

    /// Jump to the next (or previous) locator from the playhead.
    pub fn jump_locator(&mut self, forward: bool) {
        let now = self.engine.state.beat.load();
        let target = if forward {
            self.project.locators.iter().map(|l| l.0).find(|&b| b > now + 1e-3)
        } else {
            self.project.locators.iter().rev().map(|l| l.0).find(|&b| b < now - 0.25)
        };
        let to = target.unwrap_or(if forward { now } else { 0.0 });
        self.send(loopcraft_engine::Command::Seek(to));
    }

    pub fn add_groove(&mut self, g: Groove) -> usize {
        self.checkpoint();
        self.project.grooves.push(g);
        self.project.grooves.len() - 1
    }

    pub fn delete_groove(&mut self, i: usize) {
        if i >= self.project.grooves.len() {
            return;
        }
        self.checkpoint();
        self.project.grooves.remove(i);
        let fix = |c: &mut Clip| {
            c.groove = match c.groove {
                Some(g) if g == i => None,
                Some(g) if g > i => Some(g - 1),
                o => o,
            }
        };
        for t in self.project.tracks.iter_mut() {
            t.slots.iter_mut().flatten().for_each(fix);
            t.arrangement.iter_mut().for_each(|a| fix(&mut a.clip));
        }
        self.rebuild();
    }

    /// Commit a clip's groove into its notes (and clear the groove).
    pub fn commit_groove(&mut self, r: ClipRef) {
        let Some(c) = self.clip(r) else { return };
        let Some(g) = c.groove.and_then(|g| self.project.grooves.get(g)).cloned() else { return };
        self.checkpoint();
        if let Some(c) = self.clip_mut(r) {
            if let ClipContent::Midi { notes } = &mut c.content {
                for n in notes.iter_mut() {
                    let (off, vm) = g.apply(n.start);
                    n.start = (n.start + off).max(0.0);
                    n.velocity = (n.velocity as f32 * vm).round().clamp(1.0, 127.0) as u8;
                }
            }
            c.groove = None;
        }
        self.commit_clip(r);
    }

    // ------------------------------------------------------------------
    // Note editing helpers
    // ------------------------------------------------------------------

    /// Quantize note starts (and optionally ends) to `grid`, by `amount` 0..1.
    pub fn quantize_notes(&mut self, r: ClipRef, grid: Beats, amount: f64, ends: bool) {
        self.checkpoint();
        if let Some(ClipContent::Midi { notes }) = self.clip_mut(r).map(|c| &mut c.content) {
            for n in notes.iter_mut() {
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

    pub fn transpose_notes(&mut self, r: ClipRef, semis: i32) {
        self.checkpoint();
        if let Some(ClipContent::Midi { notes }) = self.clip_mut(r).map(|c| &mut c.content) {
            for n in notes.iter_mut() {
                n.pitch = (n.pitch as i32 + semis).clamp(0, 127) as u8;
            }
        }
        self.commit_clip(r);
    }

    /// Double a MIDI clip's loop, copying its notes ("Duplicate Loop").
    pub fn duplicate_loop(&mut self, r: ClipRef) {
        self.checkpoint();
        if let Some(c) = self.clip_mut(r) {
            let (ls, len) = (c.loop_start, c.length);
            if let ClipContent::Midi { notes } = &mut c.content {
                let extra: Vec<Note> = notes.iter().filter(|n| n.start >= ls && n.start < len).map(|n| Note { start: n.start + (len - ls), ..*n }).collect();
                notes.extend(extra);
            }
            c.length = len + (len - ls);
        }
        self.commit_clip(r);
    }
}
