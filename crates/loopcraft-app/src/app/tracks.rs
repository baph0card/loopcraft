//! Track and scene structure: add, delete, duplicate, move, group.

use super::*;

/// Every place a project stores a track index, so inserting, deleting or
/// moving tracks can keep references valid.
fn remap_track_refs(p: &mut Project, map: &dyn Fn(usize) -> Option<usize>) {
    let fix = |v: &mut Option<usize>| *v = v.and_then(map);
    let fix_spec = |s: &mut DeviceSpec| match s {
        DeviceSpec::Compressor(c) => fix(&mut c.sidechain_track),
        DeviceSpec::GlueCompressor(c) => fix(&mut c.sidechain_track),
        DeviceSpec::Gate(c) => fix(&mut c.sidechain_track),
        DeviceSpec::Vocoder(c) => fix(&mut c.sidechain_track),
        _ => {}
    };
    for t in p.tracks.iter_mut().chain(p.returns.iter_mut()).chain(std::iter::once(&mut p.master)) {
        t.effects.iter_mut().for_each(fix_spec);
        fix(&mut t.group);
        if let InputSource::Track(i) = t.input {
            t.input = map(i).map_or(InputSource::None, InputSource::Track);
        }
    }
    p.mappings.retain_mut(|m| {
        let r = |i: usize| map(i);
        let t = match &mut m.target {
            MappingTarget::Volume(i) | MappingTarget::Pan(i) | MappingTarget::StopTrack(i) | MappingTarget::Mute(i) | MappingTarget::Solo(i) | MappingTarget::Arm(i) => i,
            MappingTarget::Send { track, .. } | MappingTarget::DeviceParam { track, .. } | MappingTarget::InstrumentParam { track, .. } | MappingTarget::LaunchClip { track, .. } => track,
            _ => return true,
        };
        match r(*t) {
            Some(n) => {
                *t = n;
                true
            }
            None => false,
        }
    });
}

impl App {
    fn next_color(&self) -> [u8; 3] {
        Self::PALETTE[self.project.tracks.len() % 8]
    }

    fn ensure_scene(&mut self) {
        if self.project.scenes.is_empty() {
            self.project.add_scene("1");
        }
    }

    /// Insert a track at `at`, fixing every stored index.
    pub fn insert_track(&mut self, at: usize, track: Track) -> usize {
        let at = at.min(self.project.tracks.len());
        remap_track_refs(&mut self.project, &|i| Some(if i >= at { i + 1 } else { i }));
        self.project.tracks.insert(at, track);
        self.project = std::mem::take(&mut self.project).normalized();
        let shift = |v: &mut usize| {
            if *v >= at {
                *v += 1
            }
        };
        if let Some(s) = self.selected_track.as_mut() {
            shift(s);
        }
        self.selected_tracks.iter_mut().for_each(shift);
        at
    }

    /// Where a new track goes: after the selected track (inside its group).
    fn insert_point(&self) -> (usize, Option<usize>) {
        match self.selected_track {
            Some(t) if t < self.project.tracks.len() => {
                let tr = &self.project.tracks[t];
                if tr.kind == TrackKind::Group {
                    // After the group's last descendant, inside the group.
                    let mut end = t + 1;
                    while end < self.project.tracks.len() && self.project.is_inside(end, t) {
                        end += 1;
                    }
                    (end, Some(t))
                } else {
                    (t + 1, tr.group)
                }
            }
            _ => (self.project.tracks.len(), None),
        }
    }

    pub fn add_midi_track(&mut self, instrument: DeviceSpec) -> usize {
        self.checkpoint();
        self.ensure_scene();
        let name = format!("{} {}", instrument.name(), self.project.tracks.len() + 1);
        let (at, group) = self.insert_point();
        let mut t = Track::midi(name, instrument).with_color(self.next_color());
        t.group = group;
        let i = self.insert_track(at, t);
        self.selected_track = Some(i);
        self.device_target = TrackRef::Track(i);
        self.rebuild();
        i
    }

    pub fn add_audio_track(&mut self) -> usize {
        self.checkpoint();
        self.ensure_scene();
        let (at, group) = self.insert_point();
        let mut t = Track::audio(format!("Audio {}", self.project.tracks.len() + 1)).with_color(self.next_color());
        t.group = group;
        let i = self.insert_track(at, t);
        self.selected_track = Some(i);
        self.device_target = TrackRef::Track(i);
        self.rebuild();
        i
    }

    pub fn add_return(&mut self) {
        self.checkpoint();
        let letter = (b'A' + self.project.returns.len() as u8) as char;
        self.project.add_return(
            Track::new(format!("{letter} Return"), TrackKind::Return).with_effect(DeviceSpec::Reverb(ReverbParams { dry_wet: 1.0, ..Default::default() })),
        );
        self.rebuild();
    }

    pub fn delete_return(&mut self, r: usize) {
        if r >= self.project.returns.len() {
            return;
        }
        self.checkpoint();
        self.project.returns.remove(r);
        for t in self.project.tracks.iter_mut() {
            if r < t.sends.len() {
                t.sends.remove(r);
            }
        }
        for t in self.project.returns.iter_mut() {
            if r < t.sends.len() {
                t.sends.remove(r);
            }
        }
        self.project.mappings.retain(|m| !matches!(m.target, MappingTarget::Send { ret, .. } if ret == r));
        self.device_target = TrackRef::Master;
        self.rebuild();
    }

    /// Wrap the selected tracks (or the selected one) in a new group.
    pub fn group_selected(&mut self) {
        let mut sel: Vec<usize> = self.selected_tracks.clone();
        if let Some(t) = self.selected_track
            && !sel.contains(&t)
        {
            sel.push(t);
        }
        sel.sort_unstable();
        sel.dedup();
        if sel.is_empty() {
            self.status = "Select tracks to group (Ctrl-click headers to select several)".into();
            return;
        }
        // All selected tracks must share a parent for a clean group.
        let parent = self.project.tracks[sel[0]].group;
        if sel.iter().any(|&t| self.project.tracks[t].group != parent) {
            self.status = "Tracks to group must be at the same level".into();
            return;
        }
        // Move the selection to be contiguous, then insert the group before it.
        self.checkpoint();
        let first = sel[0];
        let mut moved: Vec<Track> = Vec::new();
        let descendants_of = |p: &Project, t: usize| -> Vec<usize> { (t..p.tracks.len()).filter(|&c| c == t || p.is_inside(c, t)).collect() };
        let mut indices: Vec<usize> = sel.iter().flat_map(|&t| descendants_of(&self.project, t)).collect();
        indices.sort_unstable();
        indices.dedup();
        // Build old -> new index map for the reordering.
        let n = self.project.tracks.len();
        let mut order: Vec<usize> = (0..n).filter(|i| !indices.contains(i)).collect();
        let pos = order.iter().position(|&i| i >= first).unwrap_or(order.len());
        for (k, &i) in indices.iter().enumerate() {
            order.insert(pos + k, i);
        }
        let mut new_index = vec![0usize; n];
        for (new, &old) in order.iter().enumerate() {
            new_index[old] = new;
        }
        remap_track_refs(&mut self.project, &|i| new_index.get(i).copied());
        let old = std::mem::take(&mut self.project.tracks);
        let mut slots: Vec<Option<Track>> = old.into_iter().map(Some).collect();
        for &o in &order {
            moved.push(slots[o].take().expect("each track once"));
        }
        self.project.tracks = moved;
        // Insert the group track.
        let mut g = Track::group(format!("Group {}", self.project.tracks.iter().filter(|t| t.kind == TrackKind::Group).count() + 1)).with_color([0x9a, 0x9c, 0xa3]);
        g.group = parent;
        let gi = self.insert_track(pos, g);
        for k in 0..indices.len() {
            let t = gi + 1 + k;
            if self.project.tracks[t].group == parent {
                self.project.tracks[t].group = Some(gi);
            }
        }
        self.selected_track = Some(gi);
        self.selected_tracks.clear();
        self.rebuild();
    }

    /// Remove a group, keeping its tracks (they move up a level).
    pub fn ungroup(&mut self, g: usize) {
        if self.project.tracks.get(g).is_none_or(|t| t.kind != TrackKind::Group) {
            return;
        }
        self.checkpoint();
        let parent = self.project.tracks[g].group;
        for t in self.project.tracks.iter_mut() {
            if t.group == Some(g) {
                t.group = parent;
            }
        }
        self.remove_track_at(g);
        self.rebuild();
    }

    fn remove_track_at(&mut self, t: usize) {
        remap_track_refs(&mut self.project, &|i| if i == t { None } else if i > t { Some(i - 1) } else { Some(i) });
        self.project.tracks.remove(t);
        self.automation_view.clear();
        self.selected_track = None;
        self.selected_tracks.clear();
        self.selected_clip = None;
    }

    pub fn delete_track(&mut self, t: usize) {
        if t >= self.project.tracks.len() {
            return;
        }
        self.checkpoint();
        // Deleting a group deletes everything inside it.
        let mut doomed: Vec<usize> = (t..self.project.tracks.len()).filter(|&c| c == t || self.project.is_inside(c, t)).collect();
        doomed.sort_unstable_by(|a, b| b.cmp(a));
        for d in doomed {
            self.remove_track_at(d);
        }
        self.rebuild();
    }

    pub fn duplicate_track(&mut self, t: usize) {
        let Some(track) = self.project.tracks.get(t).cloned() else { return };
        if track.kind == TrackKind::Group {
            self.status = "Duplicate the tracks inside the group instead".into();
            return;
        }
        self.checkpoint();
        let mut copy = track;
        copy.name = format!("{} copy", copy.name);
        copy.mixer.solo = false;
        copy.armed = false;
        let i = self.insert_track(t + 1, copy);
        self.selected_track = Some(i);
        self.rebuild();
    }

    /// Move a track (and its group contents) one step left/right among its siblings.
    pub fn move_track(&mut self, t: usize, left: bool) {
        let n = self.project.tracks.len();
        if t >= n {
            return;
        }
        let parent = self.project.tracks[t].group;
        let block = |p: &Project, s: usize| -> usize {
            let mut e = s + 1;
            while e < p.tracks.len() && p.is_inside(e, s) {
                e += 1;
            }
            e
        };
        let siblings: Vec<usize> = (0..n).filter(|&i| self.project.tracks[i].group == parent).collect();
        let k = siblings.iter().position(|&s| s == t).unwrap_or(0);
        let other = if left { k.checked_sub(1).map(|j| siblings[j]) } else { siblings.get(k + 1).copied() };
        let Some(o) = other else { return };
        self.checkpoint();
        let (a, b) = if left { (o, t) } else { (t, o) };
        let (a_end, b_end) = (block(&self.project, a), block(&self.project, b));
        // Reorder: [..a) [b..b_end) [a..a_end) [a_end..b) [b_end..]
        let mut order: Vec<usize> = (0..a).collect();
        order.extend(b..b_end);
        order.extend(a..a_end);
        order.extend(a_end..b);
        order.extend(b_end..n);
        let mut new_index = vec![0usize; n];
        for (new, &old) in order.iter().enumerate() {
            new_index[old] = new;
        }
        remap_track_refs(&mut self.project, &|i| new_index.get(i).copied());
        let old = std::mem::take(&mut self.project.tracks);
        let mut slots: Vec<Option<Track>> = old.into_iter().map(Some).collect();
        self.project.tracks = order.iter().map(|&o| slots[o].take().expect("each once")).collect();
        self.selected_track = Some(new_index[t]);
        self.automation_view.clear();
        self.rebuild();
    }

    pub fn add_scene(&mut self) {
        self.checkpoint();
        let n = self.project.scenes.len() + 1;
        self.project.add_scene(format!("{n}"));
        self.rebuild();
    }

    pub fn insert_scene(&mut self, at: usize) {
        self.checkpoint();
        let at = at.min(self.project.scenes.len());
        self.project.scenes.insert(at, Scene { name: format!("{}", at + 1), ..Default::default() });
        for t in self.project.tracks.iter_mut() {
            if at <= t.slots.len() {
                t.slots.insert(at, None);
            }
        }
        self.fix_jump_targets(|s| Some(if s >= at { s + 1 } else { s }));
        self.rebuild();
    }

    pub fn duplicate_scene(&mut self, s: usize) {
        if s >= self.project.scenes.len() {
            return;
        }
        self.checkpoint();
        let mut sc = self.project.scenes[s].clone();
        sc.name = format!("{} copy", sc.name);
        self.project.scenes.insert(s + 1, sc);
        for t in self.project.tracks.iter_mut() {
            let c = t.slots.get(s).cloned().flatten();
            t.slots.insert(s + 1, c);
        }
        self.fix_jump_targets(|x| Some(if x > s { x + 1 } else { x }));
        self.rebuild();
    }

    pub fn delete_scene(&mut self, s: usize) {
        if s >= self.project.scenes.len() {
            return;
        }
        self.checkpoint();
        self.project.scenes.remove(s);
        for t in self.project.tracks.iter_mut() {
            if s < t.slots.len() {
                t.slots.remove(s);
            }
        }
        self.fix_jump_targets(|x| if x == s { None } else if x > s { Some(x - 1) } else { Some(x) });
        self.selected_clip = None;
        self.rebuild();
    }

    /// Keep Jump follow actions pointing at the same slots/scenes.
    fn fix_jump_targets(&mut self, map: impl Fn(usize) -> Option<usize>) {
        let fix = |f: &mut Option<FollowAction>| {
            if let Some(fa) = f.as_mut() {
                for k in [&mut fa.action_a, &mut fa.action_b] {
                    if let FollowKind::Jump(j) = *k {
                        *k = map(j).map_or(FollowKind::NoAction, FollowKind::Jump);
                    }
                }
            }
        };
        for sc in self.project.scenes.iter_mut() {
            fix(&mut sc.follow);
        }
        for t in self.project.tracks.iter_mut() {
            for c in t.slots.iter_mut().flatten() {
                fix(&mut c.follow);
            }
        }
        self.project.mappings.retain_mut(|m| match &mut m.target {
            MappingTarget::LaunchScene(s) | MappingTarget::LaunchClip { slot: s, .. } => match map(*s) {
                Some(n) => {
                    *s = n;
                    true
                }
                None => false,
            },
            _ => true,
        });
    }

    pub fn set_arm(&mut self, t: usize, armed: bool, exclusive: bool) {
        if exclusive {
            for i in 0..self.project.tracks.len() {
                if i != t && self.project.tracks[i].armed {
                    self.project.tracks[i].armed = false;
                    self.send(Command::SetArm { track: i, armed: false });
                }
            }
        }
        if let Some(tr) = self.project.tracks.get_mut(t) {
            tr.armed = armed;
            self.send(Command::SetArm { track: t, armed });
        }
    }

    /// MIDI tracks that should receive live notes: armed ones, else the selected one.
    pub fn live_targets(&self) -> Vec<usize> {
        let armed: Vec<usize> = (0..self.project.tracks.len()).filter(|&i| self.project.tracks[i].armed && self.project.tracks[i].kind == TrackKind::Midi).collect();
        if !armed.is_empty() {
            return armed;
        }
        self.selected_track.filter(|&t| self.project.tracks.get(t).is_some_and(|t| t.kind == TrackKind::Midi)).into_iter().collect()
    }

    /// Is track `t` hidden because a group above it is folded?
    pub fn hidden(&self, t: usize) -> bool {
        let mut cur = self.project.tracks.get(t).and_then(|x| x.group);
        while let Some(g) = cur {
            if self.project.tracks.get(g).is_some_and(|x| x.folded) {
                return true;
            }
            cur = self.project.tracks.get(g).and_then(|x| x.group);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remap_fixes_sidechains_groups_and_inputs() {
        let mut p = Project::default();
        p.add_track(Track::group("G"));
        p.add_track(Track::audio("A").in_group(0));
        let mut b = Track::audio("B").with_effect(DeviceSpec::Compressor(CompressorParams { sidechain_track: Some(1), ..Default::default() }));
        b.input = InputSource::Track(1);
        p.add_track(b);
        // Delete track 0 (the group): A becomes 0, B becomes 1.
        remap_track_refs(&mut p, &|i| if i == 0 { None } else { Some(i - 1) });
        p.tracks.remove(0);
        assert_eq!(p.tracks[0].group, None);
        let DeviceSpec::Compressor(c) = &p.tracks[1].effects[0] else { panic!() };
        assert_eq!(c.sidechain_track, Some(0));
        assert_eq!(p.tracks[1].input, InputSource::Track(0));
    }
}
