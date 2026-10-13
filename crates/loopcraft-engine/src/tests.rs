//! Engine integration tests: these render real audio and check timing.

use crate::*;
use loopcraft_core::*;
use loopcraft_dsp::{AudioBuffer, NoSamples, SampleLoader, peak, rms};
use std::collections::HashMap;
use std::sync::Arc;

const SR: f32 = 48_000.0;

fn synth_track() -> Track {
    // Fast attack/release so onsets and offsets are easy to locate.
    let p = SynthParams { amp_env: Adsr { attack: 0.0005, decay: 0.05, sustain: 1.0, release: 0.005 }, ..Default::default() };
    Track::midi("Synth", DeviceSpec::PolySynth(p))
}

fn one_note_clip(len: Beats) -> Clip {
    Clip::midi("c", len, vec![Note::new(60, 0.0, len, 100)])
}

fn render(engine: &mut Engine, frames: usize) -> Vec<f32> {
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    // Use odd host block sizes to exercise chunk splitting.
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(333);
        engine.process(&mut l[done..done + n], &mut r[done..done + n]);
        done += n;
    }
    l
}

fn first_onset(buf: &[f32]) -> Option<usize> {
    buf.iter().position(|s| s.abs() > 1e-4)
}

fn beats(b: f64, bpm: f64) -> usize {
    (b * 60.0 / bpm * SR as f64).round() as usize
}

fn engine_for(p: &Project) -> (Engine, EngineHandle) {
    create(Core::from_project(p, SR, &NoSamples, &NoPlugins))
}

#[test]
fn clip_launch_is_quantized_to_the_bar() {
    let mut p = Project::default();
    p.add_scene("1");
    let t = p.add_track(synth_track());
    p.tracks[t].slots[0] = Some(one_note_clip(4.0));
    let (mut e, mut h) = engine_for(&p);

    h.send(Command::Play).ok();
    let pre = render(&mut e, beats(0.5, 120.0));
    assert_eq!(peak(&pre), 0.0);

    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let out = render(&mut e, beats(4.0, 120.0));
    let onset = first_onset(&out).expect("clip never started") + pre.len();
    assert_eq!(onset, beats(4.0, 120.0), "launch should land exactly on beat 4");
    assert_eq!(h.state.tracks[0].playing_slot.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[test]
fn launching_while_stopped_starts_immediately() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track());
    p.tracks[0].slots[0] = Some(one_note_clip(4.0));
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let out = render(&mut e, 2048);
    assert_eq!(first_onset(&out), Some(0));
    assert!(e.core().is_playing());
}

#[test]
fn scene_launch_changes_tempo_on_the_boundary() {
    let mut p = Project::default();
    p.add_scene("A");
    p.scenes[0].tempo = Some(90.0);
    p.add_track(synth_track());
    p.tracks[0].slots[0] = Some(one_note_clip(4.0));
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::Play).ok();
    render(&mut e, 1000);
    h.send(Command::LaunchScene { scene: 0 }).ok();
    render(&mut e, beats(1.0, 120.0));
    assert_eq!(e.core().tempo(), 120.0, "tempo must wait for the bar line");
    render(&mut e, beats(4.0, 120.0));
    assert_eq!(e.core().tempo(), 90.0);
}

#[test]
fn stop_track_is_quantized_and_releases_notes() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track());
    p.tracks[0].slots[0] = Some(one_note_clip(4.0));
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    render(&mut e, beats(1.0, 120.0));
    h.send(Command::StopTrack { track: 0 }).ok();
    let until_bar = render(&mut e, beats(2.9, 120.0));
    assert!(rms(&until_bar[until_bar.len() - 1000..]) > 0.01, "should keep playing until the bar");
    let after = render(&mut e, beats(1.0, 120.0));
    assert!(peak(&after[3500..]) < 1e-4, "notes must be released after the stop");
    assert_eq!(e.core().playing_slot(0), None);
}

#[test]
fn toggle_mode_stops_on_second_press() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track());
    let mut clip = one_note_clip(4.0);
    clip.launch_mode = LaunchMode::Toggle;
    clip.quantization = Some(Quantization::None);
    p.tracks[0].slots[0] = Some(clip);
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    render(&mut e, 1000);
    assert_eq!(e.core().playing_slot(0), Some(0));
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    render(&mut e, 1000);
    assert_eq!(e.core().playing_slot(0), None);
}

#[test]
fn arrangement_plays_regions_and_back_to_arrangement_works() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track());
    p.tracks[0].arrangement.push(ArrangementClip::looped(4.0, 4.0, one_note_clip(1.0)));
    p.tracks[0].slots[0] = Some(one_note_clip(4.0));
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::Play).ok();
    let out = render(&mut e, beats(10.0, 120.0));
    assert_eq!(first_onset(&out), Some(beats(4.0, 120.0)));
    assert!(peak(&out[beats(8.2, 120.0)..]) < 1e-4, "region end must silence the track");

    // A session clip takes the track over; Back to Arrangement returns it.
    h.send(Command::Seek(0.0)).ok();
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    render(&mut e, 1000);
    assert!(!h.state.tracks[0].follows_arrangement.load(std::sync::atomic::Ordering::Relaxed));
    h.send(Command::BackToArrangement).ok();
    render(&mut e, 256);
    assert!(h.state.tracks[0].follows_arrangement.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn loop_region_wraps_the_playhead() {
    let mut p = Project::default();
    p.loop_region = Some((0.0, 2.0));
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::Play).ok();
    render(&mut e, beats(3.0, 120.0));
    assert!((e.core().beat() - 1.0).abs() < 1e-3, "beat = {}", e.core().beat());
}

#[test]
fn solo_silences_other_tracks() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track());
    p.add_track(synth_track());
    p.tracks[0].slots[0] = Some(one_note_clip(4.0));
    p.tracks[1].mixer.solo = true;
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let out = render(&mut e, 4800);
    assert!(peak(&out[1000..]) < 1e-4);
}

#[test]
fn sends_feed_return_tracks() {
    let mut p = Project::default();
    p.add_scene("1");
    let mut ret = Track::new("A", TrackKind::Return);
    ret.mixer.volume_db = 0.0;
    p.add_return(ret);
    let t = p.add_track(synth_track());
    p.tracks[t].mixer.mute = true; // only the (post-fader) send... is muted too
    p.tracks[t].slots[0] = Some(one_note_clip(4.0));
    p.tracks[t].sends[0] = 0.0;
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let muted = render(&mut e, 4800);
    assert!(peak(&muted[500..]) < 1e-4, "sends are post-fader, so mute silences them");
    h.send(Command::SetMute { channel: TrackRef::Track(0), mute: false }).ok();
    h.send(Command::SetVolume { channel: TrackRef::Track(0), db: -70.0 }).ok();
    let quiet = render(&mut e, 4800);
    assert!(peak(&quiet[500..]) < 1e-4);
}

struct MapLoader(HashMap<String, Arc<AudioBuffer>>);

impl SampleLoader for MapLoader {
    fn load(&self, path: &str) -> Option<Arc<AudioBuffer>> {
        self.0.get(path).cloned()
    }
}

fn zero_crossing_hz(buf: &[f32]) -> f32 {
    let z = buf.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
    z as f32 / (buf.len() as f32 / SR)
}

fn warp_test(mode: WarpMode) -> f32 {
    warp_test_t(mode, 0.0)
}

fn warp_test_t(mode: WarpMode, transpose: f32) -> f32 {
    let tone: Vec<f32> = (0..(SR as usize * 8)).map(|i| (std::f32::consts::TAU * 220.0 * i as f32 / SR).sin() * 0.5).collect();
    let loader = MapLoader(HashMap::from([("tone.wav".to_string(), Arc::new(AudioBuffer { sample_rate: SR, channels: vec![tone] }))]));
    let mut p = Project::default();
    p.tempo = 120.0;
    p.add_scene("1");
    p.add_track(Track::audio("Audio"));
    let audio = AudioClip { file: "tone.wav".into(), warp: mode, original_bpm: 60.0, transpose, ..Default::default() };
    p.tracks[0].slots[0] = Some(Clip::audio("tone", 8.0, audio));
    let (mut e, mut h) = create(Core::from_project(&p, SR, &loader, &NoPlugins));
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let out = render(&mut e, SR as usize);
    zero_crossing_hz(&out[4800..])
}

#[test]
fn repitch_warp_follows_tempo_with_pitch() {
    // Material at 60 bpm played at 120 bpm: twice as fast, an octave up.
    let hz = warp_test(WarpMode::Repitch);
    assert!((hz - 440.0).abs() < 10.0, "{hz}");
}

#[test]
fn stretch_warp_keeps_pitch() {
    let hz = warp_test(WarpMode::Stretch);
    assert!((hz - 220.0).abs() < 10.0, "{hz}");
}

#[test]
fn warp_off_plays_at_native_speed() {
    let hz = warp_test(WarpMode::Off);
    assert!((hz - 220.0).abs() < 10.0, "{hz}");
}

#[test]
fn midi_recording_captures_live_notes() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track());
    p.quantization = Quantization::None;
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::RecordMidi(RecordBuffer::new(0, 0, 4.0), RecStart::Quantized)).ok();
    render(&mut e, beats(1.0, 120.0));
    h.send(Command::NoteOn { track: 0, note: 64, velocity: 90 }).ok();
    render(&mut e, beats(1.0, 120.0));
    h.send(Command::NoteOff { track: 0, note: 64 }).ok();
    render(&mut e, beats(2.5, 120.0));
    let rec = h.poll();
    assert_eq!(rec.len(), 1);
    let notes = &rec[0].notes;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].pitch, 64);
    assert!((notes[0].start - 1.0).abs() < 0.02, "{}", notes[0].start);
    assert!((notes[0].length - 1.0).abs() < 0.02, "{}", notes[0].length);
}

#[test]
fn automation_moves_the_fader() {
    let mut p = Project::default();
    p.add_track(synth_track());
    p.tracks[0].arrangement.push(ArrangementClip::looped(0.0, 8.0, one_note_clip(8.0)));
    p.tracks[0].automation.push(AutomationLane { target: AutomationTarget::Volume, points: vec![(0.0, 0.0), (4.0, -70.0)] });
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::Play).ok();
    let out = render(&mut e, beats(6.0, 120.0));
    assert!(rms(&out[2000..10_000]) > 0.01);
    assert!(peak(&out[beats(4.2, 120.0)..]) < 1e-4);
}

#[test]
fn swap_core_keeps_transport() {
    let mut p = Project::default();
    p.add_track(synth_track());
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::Play).ok();
    render(&mut e, 24_000);
    let beat = e.core().beat();
    p.add_track(synth_track());
    h.send(Command::SwapCore(Box::new(Core::from_project(&p, SR, &NoSamples, &NoPlugins)))).ok();
    render(&mut e, 1);
    assert_eq!(h.state.tracks.len(), 2);
    assert!(e.core().beat() > beat);
    assert!(e.core().is_playing());
    h.poll();
}

#[test]
fn complex_and_beats_warp_keep_pitch() {
    for mode in [WarpMode::Complex, WarpMode::Beats] {
        let hz = warp_test(mode);
        assert!((hz - 220.0).abs() < 10.0, "{mode:?}: {hz}");
    }
}

#[test]
fn transpose_shifts_pitch_in_stretch_and_complex() {
    for mode in [WarpMode::Stretch, WarpMode::Complex] {
        let hz = warp_test_t(mode, 12.0);
        assert!((hz - 440.0).abs() < 15.0, "{mode:?}: {hz}");
    }
}
