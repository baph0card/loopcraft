//! Recording: Session slots (with MIDI overdub), the Arrangement, and
//! turning finished takes into clips and audio files.

use super::{App, ClipRef};
use loopcraft_core::*;
use loopcraft_engine::{AudioRecord, Command, RecStart, RecTarget, RecordBuffer, record_ring};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

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
        if self.arrangement_recording && !self.engine.state.tracks.iter().any(|t| t.recording.load(Relaxed)) && self.pending_audio.is_empty() {
            // Recording ended on its own (transport stopped).
            let starting = self.engine.state.count_in_left.load() > 0.0;
            if !starting && self.arr_rec_started.elapsed().as_millis() > 300 {
                self.arrangement_recording = false;
            }
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

    /// The Arrangement Record button.
    pub fn toggle_arrangement_record(&mut self) {
        if self.arrangement_recording {
            self.send(Command::StopRecording { track: None });
            self.arrangement_recording = false;
            return;
        }
        let armed: Vec<usize> = (0..self.project.tracks.len()).filter(|&t| self.project.tracks[t].armed).collect();
        if armed.is_empty() {
            self.status = "Arm a track to record into the Arrangement".into();
            return;
        }
        self.start_count_in();
        for t in armed {
            self.start_recording(t, RecTarget::Arrangement, RecStart::Now);
        }
        self.send(loopcraft_engine::Command::BackToArrangement);
        self.arrangement_recording = true;
        self.arr_rec_started = std::time::Instant::now();
        self.status = "Recording into the Arrangement".into();
    }

    pub(crate) fn finish_midi(&mut self, buf: Box<RecordBuffer>) {
        let t = buf.track;
        if t >= self.project.tracks.len() {
            return;
        }
        let len = buf.recorded_length.max(1.0 / 16.0);
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
                let clip = Clip { color, looping: false, ..Clip::midi("Recording", len, buf.notes.clone()) };
                self.clear_range(t, buf.start, buf.start + len);
                self.project.tracks[t].arrangement.push(ArrangementClip::new(buf.start, clip));
                self.sort_arrangement(t);
                self.rebuild();
                self.status = format!("Recorded {} notes into the Arrangement", buf.notes.len());
            }
            _ => {}
        }
    }

    fn recording_path(&self, track: usize) -> (PathBuf, String) {
        let base = self.default_dir();
        let dir = base.join("Samples").join("Recorded");
        let name: String = self.project.tracks.get(track).map(|t| t.name.clone()).unwrap_or_default().chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
        let mut n = 1;
        loop {
            let file = format!("{name} {n:03}.wav");
            let p = dir.join(&file);
            if !p.exists() {
                // Relative paths only make sense once the project has a folder.
                let stored = if self.path.is_some() { Path::new("Samples").join("Recorded").join(&file).display().to_string() } else { p.display().to_string() };
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
                self.clear_range(t, rec.start, rec.start + len);
                self.project.tracks[t].arrangement.push(ArrangementClip::new(rec.start, Clip { color, looping: false, ..Clip::audio(name, len, audio) }));
                self.sort_arrangement(t);
                self.rebuild();
            }
            _ => {}
        }
        let secs = l.len() as f32 / self.sample_rate;
        self.status = if rec.dropped > 0 { format!("Recorded {secs:.1}s ({} frames dropped)", rec.dropped) } else { format!("Recorded {secs:.1}s to {}", file.display()) };
    }
}
