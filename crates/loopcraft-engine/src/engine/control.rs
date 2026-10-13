//! Applying commands on the audio thread.

use super::track::*;
use super::*;
use crate::command::{DeviceRef, RecStart, RecTarget, TrackRef};
use crate::state::NONE;
use std::sync::atomic::Ordering::Relaxed;

impl Core {
    pub(crate) fn beats_per_frame(&self) -> f64 {
        self.tempo / 60.0 / self.sr as f64
    }

    pub(crate) fn launch_time(&self, q: Option<Quantization>) -> Beats {
        if !self.playing {
            return self.beat;
        }
        q.unwrap_or(self.quant).next_boundary(self.beat, self.sig)
    }

    pub(crate) fn ensure_playing(&mut self) {
        if !self.playing && self.count_in.is_none() {
            self.playing = true;
        }
    }

    pub fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Play => {
                self.count_in = None;
                self.playing = true;
            }
            Command::PlayWithCountIn { bars } => {
                if !self.playing {
                    let total = bars.max(1) as f64 * self.sig.beats_per_bar();
                    self.count_in = Some((0.0, total));
                }
            }
            Command::Stop => self.stop_transport(),
            Command::Seek(b) => {
                self.beat = b.max(0.0);
                for t in self.tracks.iter_mut() {
                    t.flush_notes = true;
                    t.arr_active = None;
                    t.stretch.reset();
                }
            }
            Command::SetTempo(bpm) => {
                self.tempo = bpm.clamp(20.0, 999.0);
                self.tempo_auto = false;
            }
            Command::SetQuantization(q) => self.quant = q,
            Command::SetLoop(r) => self.loop_region = r.filter(|(a, b)| b > a),
            Command::SetMetronome(on) => self.metronome = on,
            Command::LaunchClip { track, slot } => self.launch_clip(track, slot),
            Command::LaunchClipAt { track, slot, at } => {
                if let Some(t) = self.tracks.get_mut(track)
                    && t.slots.get(slot).is_some_and(|c| c.is_some())
                {
                    t.pending = Some(Pending { kind: PendingKind::Launch(slot), at });
                    self.ensure_playing();
                }
            }
            Command::ReleaseClip { track, slot } => {
                if let Some(t) = self.tracks.get(track) {
                    let gate = t.slots.get(slot).and_then(|c| c.as_ref()).map(|c| c.launch_mode == LaunchMode::Gate).unwrap_or(false);
                    if gate && t.session.map(|s| s.slot) == Some(slot) {
                        let at = self.launch_time(None);
                        self.tracks[track].pending = Some(Pending { kind: PendingKind::Stop, at });
                    }
                }
            }
            Command::StopTrack { track } => self.stop_track(track),
            Command::LaunchScene { scene } => {
                let at = self.launch_time(None);
                self.launch_scene_at(scene, at);
            }
            Command::StopAllClips => {
                let at = self.launch_time(None);
                for t in self.tracks.iter_mut().filter(|t| t.session.is_some() || t.pending.is_some()) {
                    t.pending = Some(Pending { kind: PendingKind::Stop, at });
                }
                self.scene_follow = None;
            }
            Command::BackToArrangement => {
                for t in self.tracks.iter_mut() {
                    if t.session.is_some() {
                        t.flush_notes = true;
                    }
                    t.session = None;
                    t.pending = None;
                    t.follow_at = None;
                    t.follows_arrangement = true;
                    t.arr_active = None;
                }
                self.scene_follow = None;
                self.tempo_auto = !self.tempo_points.is_empty();
            }
            Command::SetVolume { channel, db } => {
                if let Some(c) = self.channel_mut(channel) {
                    c.gain_target = db_to_gain(db);
                }
            }
            Command::SetPan { channel, pan } => {
                if let Some(c) = self.channel_mut(channel) {
                    c.pan = pan.clamp(-1.0, 1.0);
                }
            }
            Command::SetMute { channel, mute } => {
                if let Some(c) = self.channel_mut(channel) {
                    c.mute = mute;
                }
            }
            Command::SetSolo { track, solo } => {
                if let Some(t) = self.tracks.get_mut(track) {
                    t.solo = solo;
                }
            }
            Command::SetSend { track, ret, db } => {
                if let Some(s) = self.tracks.get_mut(track).and_then(|t| t.sends.get_mut(ret)) {
                    *s = db_to_gain(db);
                }
            }
            Command::SetArm { track, armed } => {
                if let Some(t) = self.tracks.get_mut(track) {
                    t.armed = armed;
                }
                if let Some(ts) = self.state.tracks.get(track) {
                    ts.armed.store(armed, Relaxed);
                }
            }
            Command::SetMonitor { track, monitor } => {
                if let Some(t) = self.tracks.get_mut(track) {
                    t.monitor = monitor;
                }
            }
            Command::SetCrossfader(x) => self.crossfader = x.clamp(-1.0, 1.0),
            Command::SetCueMode(on) => self.cue_mode = on,
            Command::SetCueVolume(db) => self.cue_gain = db_to_gain(db),
            Command::SetDeviceParam { channel, device, param, value } => self.set_device_param(channel, device, param, value),
            Command::SetDeviceBypass { channel, device, bypass } => match (channel, device) {
                (TrackRef::Track(i), DeviceRef::Instrument) => {
                    if let Some(t) = self.tracks.get_mut(i) {
                        t.instrument_bypass = bypass;
                        if let Some(inst) = t.instrument.as_mut() {
                            inst.all_notes_off();
                        }
                    }
                }
                (TrackRef::Track(i), DeviceRef::MidiEffect(e)) => {
                    if let Some(t) = self.tracks.get_mut(i)
                        && let Some(s) = t.midi_fx.get_mut(e)
                    {
                        s.bypass = bypass;
                        t.flush_notes = true;
                    }
                }
                (ch, DeviceRef::Effect(e)) => {
                    if let Some(s) = self.channel_mut(ch).and_then(|c| c.effects.get_mut(e)) {
                        s.bypass = bypass;
                    }
                }
                _ => {}
            },
            Command::NoteOn { track, note, velocity } => self.live_note(track, true, note, velocity),
            Command::NoteOff { track, note } => self.live_note(track, false, note, 0),
            Command::RecordMidi(mut buf, when) => {
                let track = buf.track;
                if self.recordings.len() >= self.recordings.capacity() || track >= self.tracks.len() {
                    self.post(EngineEvent::Recorded(buf));
                    return;
                }
                let start = self.rec_start(when);
                // Overdub onto the clip already playing in the target slot.
                if let RecTarget::Slot(s) = buf.target
                    && let Some(play) = self.tracks[track].session
                    && play.slot == s
                {
                    buf.overdub = Some(play.start);
                }
                if buf.target == RecTarget::Arrangement {
                    self.tracks[track].arr_recording = true;
                }
                let end = buf.length.map(|l| start + l);
                self.ensure_playing();
                self.recordings.push(Recording { kind: RecKind::Midi(buf), track, start, end });
                self.mark_recording(track);
            }
            Command::RecordAudio(rec, when) => {
                let track = rec.track;
                if self.recordings.len() >= self.recordings.capacity() || track >= self.tracks.len() {
                    self.post(EngineEvent::AudioRecorded(rec));
                    return;
                }
                let start = self.rec_start(when);
                if rec.target == RecTarget::Arrangement {
                    self.tracks[track].arr_recording = true;
                }
                let end = rec.length.map(|l| start + l);
                self.ensure_playing();
                self.recordings.push(Recording { kind: RecKind::Audio(rec), track, start, end });
                self.mark_recording(track);
            }
            Command::StopRecording { track } => match track {
                Some(t) => {
                    let at = self.launch_time(None).max(self.beat);
                    for r in self.recordings.iter_mut().filter(|r| r.track == t && r.end.is_none()) {
                        // Round the take up to whole quantize steps from its start.
                        let q = self.quant.grid(self.sig).unwrap_or(0.0);
                        let len = (at - r.start).max(0.0);
                        let len = if q > 0.0 { (len / q).ceil().max(1.0) * q } else { len.max(1.0 / 16.0) };
                        r.end = Some(r.start + len);
                    }
                }
                None => {
                    let now = self.beat;
                    for r in self.recordings.iter_mut() {
                        r.end = Some(now.max(r.start));
                    }
                }
            },
            Command::ReplaceClip { track, slot, clip } => {
                if let Some(s) = self.tracks.get_mut(track).and_then(|t| t.slots.get_mut(slot)) {
                    let old = std::mem::replace(s, clip);
                    if let Some(old) = old {
                        self.post(EngineEvent::Garbage(crate::command::Garbage::Clip(old)));
                    }
                    let t = &mut self.tracks[track];
                    if t.slots[slot].is_none() && t.session.map(|p| p.slot) == Some(slot) {
                        t.session = None;
                        t.follow_at = None;
                        t.flush_notes = true;
                    } else if t.session.map(|p| p.slot) == Some(slot)
                        && let Some(Some(c)) = t.slots.get(slot)
                        && let crate::clip::RtContent::Audio(a) = &c.content
                    {
                        t.stretch.beats.set_slices(a.slices.clone());
                    }
                }
            }
            Command::SwapCore(_) => unreachable!("handled by Engine"),
        }
    }

    fn rec_start(&self, when: RecStart) -> Beats {
        if self.count_in.is_some() {
            // The transport is frozen during a count-in; recording begins when it ends.
            return self.beat;
        }
        match when {
            RecStart::Now => self.beat,
            RecStart::Quantized => self.launch_time(None),
        }
    }

    fn mark_recording(&self, track: usize) {
        if let Some(ts) = self.state.tracks.get(track) {
            ts.recording.store(true, Relaxed);
            let slot = self.recordings.iter().rev().find(|r| r.track == track).and_then(|r| match &r.kind {
                RecKind::Midi(b) => b.slot(),
                RecKind::Audio(a) => if let RecTarget::Slot(s) = a.target { Some(s) } else { None },
            });
            ts.recording_slot.store(slot.map_or(NONE, |s| s as i32), Relaxed);
        }
    }

    pub(crate) fn channel_mut(&mut self, r: TrackRef) -> Option<&mut Channel> {
        match r {
            TrackRef::Track(i) => self.tracks.get_mut(i).map(|t| &mut t.chan),
            TrackRef::Return(i) => self.returns.get_mut(i),
            TrackRef::Master => Some(&mut self.master),
        }
    }

    fn set_device_param(&mut self, channel: TrackRef, device: DeviceRef, param: usize, value: f32) {
        match (channel, device) {
            (TrackRef::Track(i), DeviceRef::Instrument) => {
                if let Some(inst) = self.tracks.get_mut(i).and_then(|t| t.instrument.as_mut()) {
                    inst.set_param(param, value);
                }
            }
            (TrackRef::Track(i), DeviceRef::MidiEffect(e)) => {
                if let Some(s) = self.tracks.get_mut(i).and_then(|t| t.midi_fx.get_mut(e)) {
                    s.fx.set_param(param, value);
                }
            }
            (ch, DeviceRef::Effect(e)) => {
                if let Some(s) = self.channel_mut(ch).and_then(|c| c.effects.get_mut(e)) {
                    s.fx.set_param(param, value);
                }
            }
            _ => {}
        }
    }

    fn live_note(&mut self, track: usize, on: bool, note: u8, vel: u8) {
        let beat = self.beat;
        if let Some(t) = self.tracks.get_mut(track)
            && t.live_len < t.live.len()
        {
            t.live[t.live_len] = (on, note, vel);
            t.live_len += 1;
        }
        let playing_clip = self.tracks.get(track).and_then(|t| t.session).zip(self.tracks.get(track).and_then(|t| t.session.and_then(|s| t.slots.get(s.slot)).and_then(|c| c.as_ref().map(|c| (c.length, c.loop_start, c.looping)))));
        for rec in self.recordings.iter_mut() {
            if rec.track != track || beat + 1e-9 < rec.start {
                continue;
            }
            let RecKind::Midi(buf) = &mut rec.kind else { continue };
            let pos = match (buf.overdub, playing_clip) {
                (Some(clip_start), Some((_, (len, ls, looping)))) => crate::clip::local_pos(beat - clip_start, len, ls, looping),
                _ => beat - rec.start,
            };
            let n = note as usize & 127;
            if on {
                buf.held[n] = Some((pos, vel));
            } else if let Some((start, v)) = buf.held[n].take()
                && buf.notes.len() < buf.notes.capacity()
            {
                let mut len = pos - start;
                if len <= 0.0 {
                    // Wrapped past the overdub loop point: hold to the clip end.
                    len = playing_clip.map(|(_, (l, _, _))| l - start).unwrap_or(1.0 / 64.0);
                }
                buf.notes.push(Note::new(note, start, len.max(1.0 / 64.0), v));
            }
        }
    }

    fn launch_clip(&mut self, track: usize, slot: usize) {
        let Some(t) = self.tracks.get(track) else { return };
        if t.kind == TrackKind::Group {
            // Group slots launch the children's clips in that row.
            let at = self.launch_time(None);
            self.ensure_playing();
            for c in 0..self.tracks.len() {
                if self.is_descendant(c, track) && self.tracks[c].kind != TrackKind::Group {
                    let has = self.tracks[c].slots.get(slot).is_some_and(|s| s.is_some());
                    let ct = &mut self.tracks[c];
                    if has {
                        ct.pending = Some(Pending { kind: PendingKind::Launch(slot), at });
                    } else if ct.session.is_some() {
                        ct.pending = Some(Pending { kind: PendingKind::Stop, at });
                    }
                }
            }
            return;
        }
        let clip = t.slots.get(slot).and_then(|c| c.as_ref());
        let Some(clip) = clip else {
            // Launching an empty slot acts as that track's stop button.
            self.stop_track(track);
            return;
        };
        let q = clip.quantization;
        let toggle_off = clip.launch_mode == LaunchMode::Toggle && t.session.map(|s| s.slot) == Some(slot);
        let at = self.launch_time(q);
        self.ensure_playing();
        let kind = if toggle_off { PendingKind::Stop } else { PendingKind::Launch(slot) };
        self.tracks[track].pending = Some(Pending { kind, at });
    }

    fn stop_track(&mut self, track: usize) {
        let at = self.launch_time(None);
        let is_group = self.tracks.get(track).is_some_and(|t| t.kind == TrackKind::Group);
        for c in 0..self.tracks.len() {
            if (c == track || (is_group && self.is_descendant(c, track))) && (self.tracks[c].session.is_some() || self.tracks[c].pending.is_some()) {
                self.tracks[c].pending = Some(Pending { kind: PendingKind::Stop, at });
            }
        }
    }

    pub(crate) fn is_descendant(&self, track: usize, group: usize) -> bool {
        let mut cur = self.tracks.get(track).and_then(|t| t.parent);
        while let Some(p) = cur {
            if p == group {
                return true;
            }
            cur = self.tracks.get(p).and_then(|t| t.parent);
        }
        false
    }

    pub(crate) fn launch_scene_at(&mut self, scene: usize, at: Beats) {
        if scene >= self.scenes.len() {
            return;
        }
        self.ensure_playing();
        for t in self.tracks.iter_mut().filter(|t| t.kind != TrackKind::Group) {
            let has = t.slots.get(scene).map(|c| c.is_some()).unwrap_or(false);
            if has {
                t.pending = Some(Pending { kind: PendingKind::Launch(scene), at });
            } else if t.session.is_some() {
                t.pending = Some(Pending { kind: PendingKind::Stop, at });
            }
        }
        if let Some(bpm) = self.scenes[scene].tempo {
            self.pending_tempo = Some((at, bpm));
            self.tempo_auto = false;
        }
        if let Some(sig) = self.scenes[scene].time_signature {
            self.pending_sig = Some((at, sig));
        }
        self.scene_follow = self.scenes[scene].follow.filter(|f| f.enabled && f.after > 0.0).map(|f| (at + f.after, scene));
    }

    pub(crate) fn stop_transport(&mut self) {
        self.playing = false;
        self.count_in = None;
        // Finish any recordings at the current position.
        let now = self.beat;
        for r in self.recordings.iter_mut() {
            r.end = Some(now.max(r.start));
        }
        self.finish_due_recordings(f64::INFINITY);
        for t in self.tracks.iter_mut() {
            t.session = None;
            t.pending = None;
            t.follow_at = None;
            t.arr_active = None;
            t.arr_recording = false;
            t.active_notes = [false; 128];
            t.flush_notes = true;
            if let Some(inst) = t.instrument.as_mut() {
                inst.all_notes_off();
            }
        }
        self.pending_tempo = None;
        self.scene_follow = None;
    }
}
