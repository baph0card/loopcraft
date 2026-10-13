//! Recording: Session slots (with MIDI overdub), the Arrangement, and
//! turning finished takes into clips and audio files.

use super::{App, ClipRef};
use loopcraft_core::*;
use loopcraft_engine::{AudioRecord, Command, RecStart, RecTarget, RecordBuffer, record_ring};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

/// Clip-local position after `t` beats of playback (plays once, then loops
/// `loop_start..len`).
fn local_pos(t: f64, len: f64, loop_start: f64, looping: bool) -> f64 {
    let t = t.max(0.0);
    if !looping || t < len {
        return t.min(len);
    }
    let span = (len - loop_start).max(1e-9);
    loop_start + (t - len) % span
}

/// Split a take that started at song beat `start` and ran `len` beats into
/// passes of the loop brace: (song start, take-clock start, duration).
/// Without a loop (or a take that never wrapped) it's one pass.
pub(crate) fn pass_spans(start: f64, len: f64, loop_region: Option<(f64, f64)>) -> Vec<(f64, f64, f64)> {
    let Some((ls, le)) = loop_region.filter(|&(ls, le)| le > ls && start >= ls - 1e-9 && start < le) else { return vec![(start, 0.0, len)] };
    let first = le - start;
    if len <= first + 1e-9 {
        return vec![(start, 0.0, len)];
    }
    let mut v = vec![(start, 0.0, first)];
    let mut c = first;
    while c < len - 1e-9 {
        let d = (le - ls).min(len - c);
        v.push((ls, c, d));
        c += d;
    }
    v
}

/// The UI end of an audio recording: samples stream in while it runs.
pub struct PendingAudio {
    pub track: usize,
    pub target: RecTarget,
    pub rx: rtrb::Consumer<f32>,
    /// Interleaved stereo received so far.
    pub samples: Vec<f32>,
}

/// Seconds of audio the ring between the audio thread and the UI holds.
/// The UI drains it every frame, so this only needs to cover stalls.
const RING_SECONDS: f32 = 20.0;

impl App {
    fn drain_audio(&mut self) {
        for p in self.pending_audio.iter_mut() {
            let n = p.rx.slots();
            if n == 0 {
                continue;
            }
            if let Ok(c) = p.rx.read_chunk(n) {
                let (a, b) = c.as_slices();
                p.samples.extend_from_slice(a);
                p.samples.extend_from_slice(b);
                c.commit_all();
            }
        }
    }

    /// Collect finished recordings from the engine.
    pub(crate) fn poll_recordings(&mut self) {
        self.drain_audio();
        let polled = self.engine.poll_all();
        for buf in polled.midi {
            self.finish_midi(buf);
        }
        for rec in polled.audio {
            self.drain_audio();
            self.finish_audio(rec);
        }
        for (track, slot, origin, at) in polled.session {
            self.session_changed(track, slot, origin, at);
        }
        if self.arrangement_recording && !self.playing() {
            // The transport stopped: Arrangement Record ends with it.
            let starting = self.engine.state.count_in_left.load() > 0.0;
            if !starting && self.arr_rec_started.elapsed().as_millis() > 300 {
                self.arrangement_recording = false;
                let at = self.engine.state.beat.load();
                self.close_session_regions(at);
            }
        }
    }

    // ------------------------------------------------------------------
    // Session → Arrangement: while Arrangement Record is on, every Session
    // clip that plays is written onto the timeline where it played.
    // ------------------------------------------------------------------

    pub(crate) fn session_changed(&mut self, track: usize, slot: Option<usize>, origin: f64, at: f64) {
        // Armed tracks are recording their input instead.
        if !self.arrangement_recording || self.project.tracks.get(track).is_none_or(|t| t.armed) {
            return;
        }
        self.close_session_region(track, at);
        if let Some(s) = slot
            && let Some(c) = self.project.tracks.get(track).and_then(|t| t.slots.get(s)).and_then(|c| c.as_ref())
        {
            let pos = local_pos(at - origin, c.length, c.loop_start, c.looping);
            self.session_rec.insert(track, (s, pos, at));
        }
    }

    /// Write the open Session region on `track`, ending at `end`.
    fn close_session_region(&mut self, track: usize, end: f64) {
        let Some((slot, offset, start)) = self.session_rec.remove(&track) else { return };
        if end <= start + 1e-6 {
            return;
        }
        let Some(clip) = self.project.tracks.get(track).and_then(|t| t.slots.get(slot)).cloned().flatten() else { return };
        // A one-shot clip can't play past its end.
        let dur = if clip.looping { end - start } else { (end - start).min((clip.length - offset).max(0.0)) };
        if dur <= 1e-6 {
            return;
        }
        self.checkpoint();
        self.clear_range(track, start, start + dur);
        self.project.tracks[track].arrangement.push(ArrangementClip { start, duration: Some(dur), offset, clip });
        self.sort_arrangement(track);
        self.rebuild();
    }

    pub(crate) fn close_session_regions(&mut self, at: f64) {
        let open: Vec<usize> = self.session_rec.keys().copied().collect();
        for t in open {
            self.close_session_region(t, at);
        }
    }

    pub fn is_recording(&self, track: usize) -> bool {
        self.engine.state.tracks.get(track).is_some_and(|t| t.recording.load(Relaxed))
    }

    fn start_count_in(&mut self) {
        if !self.playing() && self.project.count_in_bars > 0 {
            self.send(Command::PlayWithCountIn { bars: self.project.count_in_bars });
        }
    }

    /// Start recording track `t` into `target` (open-ended; stop with
    /// `stop_recording`). Audio tracks need an input; MIDI tracks record
    /// whatever is played into them.
    pub fn start_recording(&mut self, t: usize, target: RecTarget, when: RecStart) {
        let Some(tr) = self.project.tracks.get(t) else { return };
        match tr.kind {
            TrackKind::Midi => {
                self.send(Command::RecordMidi(RecordBuffer::with(t, target, None), when));
            }
            TrackKind::Audio => {
                if tr.input == InputSource::None {
                    self.status = format!("{} has no input selected", tr.name);
                    return;
                }
                let (tx, rx) = record_ring(self.sample_rate, RING_SECONDS);
                let rec = Box::new(AudioRecord { track: t, target, length: None, tx, start: 0.0, recorded_length: 0.0, frames: 0, dropped: 0 });
                self.pending_audio.push(PendingAudio { track: t, target, rx, samples: Vec::with_capacity((self.sample_rate as usize) * 2 * 8) });
                self.send(Command::RecordAudio(rec, when));
            }
            _ => {}
        }
    }

    /// Click on an empty slot of an armed track: record into it.
    pub fn record_slot(&mut self, t: usize, slot: usize) {
        if self.is_recording(t) {
            self.send(Command::StopRecording { track: Some(t) });
            return;
        }
        self.start_count_in();
        self.start_recording(t, RecTarget::Slot(slot), RecStart::Quantized);
        if let Some(tr) = self.project.tracks.get(t) {
            self.status = format!("Recording {} into slot {}", tr.name, slot + 1);
        }
    }

    /// The Session Record button: stop running takes, or start a take on
    /// every armed track (overdubbing MIDI clips that are playing).
    pub fn session_record(&mut self) {
        let recording: Vec<usize> = (0..self.project.tracks.len()).filter(|&t| self.is_recording(t)).collect();
        if !recording.is_empty() {
            for t in recording {
                self.send(Command::StopRecording { track: Some(t) });
            }
            return;
        }
        let armed: Vec<usize> = (0..self.project.tracks.len()).filter(|&t| self.project.tracks[t].armed).collect();
        if armed.is_empty() {
            // Nothing armed: record the selected MIDI track, like the old button.
            match self.selected_track.filter(|&t| self.project.tracks.get(t).is_some_and(|t| t.kind == TrackKind::Midi)) {
                Some(t) => self.set_arm(t, true, true),
                None => {
                    self.status = "Arm a track to record".into();
                    return;
                }
            }
            return self.session_record();
        }
        self.start_count_in();
        let scene = self.selected_scene;
        let mut need_scene = false;
        for &t in &armed {
            let playing = self.engine.state.tracks.get(t).map_or(-1, |s| s.playing_slot.load(Relaxed));
            if playing >= 0 && self.project.tracks[t].kind == TrackKind::Midi {
                // Overdub the playing clip.
                self.start_recording(t, RecTarget::Slot(playing as usize), RecStart::Now);
                continue;
            }
            let free = (scene..self.project.scenes.len()).find(|&s| self.project.tracks[t].slots.get(s).is_some_and(Option::is_none));
            match free {
                Some(s) => self.start_recording(t, RecTarget::Slot(s), RecStart::Quantized),
                None => need_scene = true,
            }
        }
        if need_scene {
            // Add a scene at the end and record the remaining tracks there.
            self.checkpoint();
            let s = self.project.add_scene(format!("{}", self.project.scenes.len() + 1));
            self.rebuild();
            for &t in &armed {
                if !self.is_recording(t) && self.project.tracks[t].kind != TrackKind::Group && !self.pending_audio.iter().any(|p| p.track == t) {
                    self.start_recording(t, RecTarget::Slot(s), RecStart::Quantized);
                }
            }
        }
        self.status = "Session recording".into();
    }

    /// The Arrangement Record button: records armed tracks' input onto the
    /// timeline, and any Session clips launched meanwhile (armed or not).
    pub fn toggle_arrangement_record(&mut self) {
        if self.arrangement_recording {
            self.send(Command::StopRecording { track: None });
            self.arrangement_recording = false;
            let at = self.engine.state.beat.load();
            self.close_session_regions(at);
            return;
        }
        let armed: Vec<usize> = (0..self.project.tracks.len()).filter(|&t| self.project.tracks[t].armed).collect();
        self.start_count_in();
        for &t in &armed {
            self.start_recording(t, RecTarget::Arrangement, RecStart::Now);
        }
        if armed.is_empty() && !self.playing() && self.project.count_in_bars == 0 {
            self.send(Command::Play);
        }
        // Armed tracks go back to the timeline; tracks playing Session clips
        // keep playing them, and those clips are recorded from here on.
        let now = self.engine.state.beat.load();
        for t in 0..self.project.tracks.len() {
            let Some(st) = self.engine.state.tracks.get(t) else { continue };
            let slot = st.playing_slot.load(Relaxed);
            if slot >= 0 && !armed.contains(&t) {
                self.session_rec.insert(t, (slot as usize, st.clip_position.load(), now));
            }
        }
        self.arrangement_recording = true;
        self.arr_rec_started = std::time::Instant::now();
        self.status = if armed.is_empty() { "Recording Session clips into the Arrangement".into() } else { "Recording into the Arrangement".into() };
    }

    /// Capture MIDI: turn what was just played into armed (or the
    /// selected) MIDI tracks into a clip, without having pressed Record.
    pub fn capture_midi(&mut self) {
        let targets = self.live_targets();
        if targets.is_empty() {
            self.status = "Arm or select a MIDI track to capture into".into();
            return;
        }
        for t in targets {
            self.send(Command::CaptureMidi(RecordBuffer::with(t, RecTarget::Capture, None)));
        }
    }

    fn finish_capture(&mut self, buf: Box<RecordBuffer>) {
        let t = buf.track;
        let bar = self.project.time_signature.beats_per_bar();
        // The latest phrase: walk back from the last note until a gap of
        // two bars or more.
        let notes = &buf.notes;
        if notes.is_empty() {
            self.status = "Nothing to capture: play something first".into();
            return;
        }
        let mut first = notes.len() - 1;
        while first > 0 {
            let prev_end = notes[..first].iter().map(|n| n.start + n.length).fold(f64::MIN, f64::max);
            if notes[first].start - prev_end >= 2.0 * bar {
                break;
            }
            first -= 1;
        }
        let phrase = &notes[first..];
        let start = (phrase[0].start / bar).floor() * bar;
        let end = phrase.iter().map(|n| n.start + n.length).fold(start, f64::max);
        let bars = ((end - start) / bar).ceil().max(1.0) as u32;
        let len = bars.next_power_of_two() as f64 * bar;
        let rel: Vec<Note> = phrase.iter().map(|n| Note { start: n.start - start, length: n.length.min(len - (n.start - start)), ..*n }).collect();
        let color = self.project.tracks[t].color;
        self.checkpoint();
        if self.view == super::View::Arrangement && self.playing() {
            let clip = Clip { color, looping: false, ..Clip::midi("Capture", len, rel) };
            self.clear_range(t, start, start + len);
            self.project.tracks[t].arrangement.push(ArrangementClip::new(start, clip));
            self.sort_arrangement(t);
            self.rebuild();
        } else {
            let scene = self.selected_scene;
            let slot = (scene..self.project.scenes.len())
                .find(|&s| self.project.tracks[t].slots[s].is_none())
                .unwrap_or_else(|| self.project.add_scene(format!("{}", self.project.scenes.len() + 1)));
            self.project.tracks[t].slots[slot] = Some(Clip { color, ..Clip::midi("Capture", len, rel) });
            self.rebuild();
            if self.playing() {
                // Keep it in time with what was played.
                self.send(Command::LaunchClipAt { track: t, slot, at: start });
            }
            self.selected_clip = Some(ClipRef::Session { track: t, slot });
        }
        self.status = format!("Captured {} notes ({bars} bar{})", phrase.len(), if bars == 1 { "" } else { "s" });
    }

    pub(crate) fn finish_midi(&mut self, buf: Box<RecordBuffer>) {
        let t = buf.track;
        if t >= self.project.tracks.len() {
            return;
        }
        let len = buf.recorded_length.max(1.0 / 16.0);
        if buf.target == RecTarget::Capture {
            return self.finish_capture(buf);
        }
        match buf.target {
            RecTarget::Slot(s) if s < self.project.scenes.len() => {
                if buf.overdub.is_some() {
                    if buf.notes.is_empty() {
                        return;
                    }
                    self.checkpoint();
                    if let Some(Clip { content: ClipContent::Midi { notes }, .. }) = self.project.tracks[t].slots[s].as_mut() {
                        notes.extend(buf.notes.iter().copied());
                        notes.sort_by(|a, b| a.start.total_cmp(&b.start));
                    }
                    self.push_clip(t, s);
                    self.status = format!("Overdubbed {} notes", buf.notes.len());
                    return;
                }
                self.checkpoint();
                let color = self.project.tracks[t].color;
                let clip = Clip { color, ..Clip::midi("Recording", len, buf.notes.clone()) };
                self.project.tracks[t].slots[s] = Some(clip);
                self.push_clip(t, s);
                // Loop the new clip from its natural start, seamlessly.
                if self.playing() {
                    self.send(Command::LaunchClipAt { track: t, slot: s, at: buf.start });
                }
                self.selected_clip = Some(ClipRef::Session { track: t, slot: s });
                self.status = format!("Recorded {} notes", buf.notes.len());
            }
            RecTarget::Arrangement => {
                if buf.notes.is_empty() {
                    return;
                }
                self.checkpoint();
                let color = self.project.tracks[t].color;
                let notes = buf.notes.clone();
                let passes = pass_spans(buf.start, len, self.project.loop_region);
                let n = passes.len();
                self.place_takes(t, &passes, |cs, dur| {
                    let ns: Vec<Note> = notes
                        .iter()
                        .filter(|n| n.start >= cs - 1e-9 && n.start < cs + dur - 1e-9)
                        .map(|n| Note { start: n.start - cs, length: n.length.min(cs + dur - n.start), ..*n })
                        .collect();
                    Clip { color, looping: false, ..Clip::midi("Recording", dur, ns) }
                });
                self.rebuild();
                self.status = if n > 1 { format!("Recorded {n} takes") } else { format!("Recorded {} notes into the Arrangement", buf.notes.len()) };
            }
            _ => {}
        }
    }

    /// Put recorded passes on track `t`. One pass goes straight onto the
    /// timeline. Several (loop recording) each become a take lane, and the
    /// timeline gets the latest take over each stretch of time.
    fn place_takes(&mut self, t: usize, passes: &[(f64, f64, f64)], make: impl Fn(f64, f64) -> Clip) {
        let several = passes.len() > 1;
        for &(song, cs, dur) in passes {
            let region = ArrangementClip::new(song, make(cs, dur));
            if several {
                let n = self.project.tracks[t].takes.len() + 1;
                self.project.tracks[t].takes.push(TakeLane { name: format!("Take {n}"), clips: vec![region.clone()] });
            }
            self.clear_range(t, song, song + dur);
            self.project.tracks[t].arrangement.push(region);
            self.sort_arrangement(t);
        }
        if several {
            self.project.tracks[t].show_takes = true;
        }
    }

    /// Comp: copy `a..b` of take lane `lane` onto the track's timeline.
    pub fn comp_from_take(&mut self, t: usize, lane: usize, a: f64, b: f64) {
        let Some(take) = self.project.tracks.get(t).and_then(|tr| tr.takes.get(lane)).cloned() else { return };
        let (a, b) = (a.min(b), a.max(b));
        let pieces: Vec<ArrangementClip> = take
            .clips
            .iter()
            .filter(|r| r.start < b && r.end() > a)
            .map(|r| {
                let s = r.start.max(a);
                let e = r.end().min(b);
                ArrangementClip { start: s, duration: Some(e - s), offset: r.offset + (s - r.start), clip: r.clip.clone() }
            })
            .collect();
        if pieces.is_empty() {
            return;
        }
        self.checkpoint();
        for p in pieces {
            self.clear_range(t, p.start, p.end());
            self.project.tracks[t].arrangement.push(p);
        }
        self.sort_arrangement(t);
        self.rebuild();
        self.status = format!("Comped {:.2} beats from {}", b - a, take.name);
    }

    pub fn delete_take_lane(&mut self, t: usize, lane: usize) {
        if self.project.tracks.get(t).is_some_and(|tr| lane < tr.takes.len()) {
            self.checkpoint();
            self.project.tracks[t].takes.remove(lane);
        }
    }

    fn recording_path(&self, track: usize) -> (PathBuf, String) {
        let base = self.default_dir();
        let dir = base.join("Samples").join("Recorded");
        let name: String = self
            .project
            .tracks
            .get(track)
            .map(|t| t.name.clone())
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' })
            .collect();
        let mut n = 1;
        loop {
            let file = format!("{name} {n:03}.wav");
            let p = dir.join(&file);
            if !p.exists() {
                // Relative paths only make sense once the project has a folder.
                let stored =
                    if self.path.is_some() { Path::new("Samples").join("Recorded").join(&file).display().to_string() } else { p.display().to_string() };
                return (p, stored);
            }
            n += 1;
        }
    }

    fn finish_audio(&mut self, rec: Box<AudioRecord>) {
        let Some(i) = self.pending_audio.iter().position(|p| p.track == rec.track && p.target == rec.target) else { return };
        let p = self.pending_audio.remove(i);
        let t = rec.track;
        if t >= self.project.tracks.len() || p.samples.len() < 4 {
            return;
        }
        let (l, r): (Vec<f32>, Vec<f32>) = p.samples.chunks_exact(2).map(|c| (c[0], c[1])).unzip();
        let (file, stored) = self.recording_path(t);
        if let Some(d) = file.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Err(e) = loopcraft_io::write_wav(&file, &l, &r, self.sample_rate as u32, loopcraft_io::BitDepth::Float32) {
            self.status = format!("Couldn't write recording: {e:#}");
            return;
        }
        let len = rec.recorded_length.max(1.0 / 16.0);
        let bpm = self.project.tempo_at(rec.start);
        let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let color = self.project.tracks[t].color;
        let audio = AudioClip { file: stored, warp: WarpMode::Stretch, original_bpm: bpm, ..Default::default() };
        self.checkpoint();
        match rec.target {
            RecTarget::Slot(s) if s < self.project.scenes.len() => {
                self.project.tracks[t].slots[s] = Some(Clip { color, ..Clip::audio(name, len, audio) });
                self.push_clip(t, s);
                if self.playing() {
                    self.send(Command::LaunchClipAt { track: t, slot: s, at: rec.start });
                }
                self.selected_clip = Some(ClipRef::Session { track: t, slot: s });
            }
            RecTarget::Arrangement => {
                // Every pass refers to the same file, starting further in.
                let passes = pass_spans(rec.start, len, self.project.loop_region);
                self.place_takes(t, &passes, |cs, dur| {
                    let a = AudioClip { start_offset: cs, ..audio.clone() };
                    Clip { color, looping: false, ..Clip::audio(name.clone(), dur, a) }
                });
                self.rebuild();
            }
            _ => {}
        }
        let secs = l.len() as f32 / self.sample_rate;
        self.status = if rec.dropped > 0 {
            format!("Recorded {secs:.1}s ({} frames dropped)", rec.dropped)
        } else {
            format!("Recorded {secs:.1}s to {}", file.display())
        };
    }
}
