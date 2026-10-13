//! Tests for follow actions, groups, MIDI effects, audio input/recording,
//! delay compensation, count-in, tempo automation, crossfader and cue.

use crate::*;
use loopcraft_core::*;
use loopcraft_dsp::{NoSamples, peak, rms};
use std::sync::atomic::Ordering::Relaxed;

const SR: f32 = 48_000.0;

fn beats(b: f64, bpm: f64) -> usize {
    (b * 60.0 / bpm * SR as f64).round() as usize
}

fn sine_synth() -> DeviceSpec {
    DeviceSpec::PolySynth(SynthParams {
        osc1: Waveform::Sine,
        osc2: Waveform::Sine,
        osc_mix: 0.0,
        cutoff_hz: 18_000.0,
        resonance: 0.0,
        filter_env_amount: 0.0,
        amp_env: Adsr { attack: 0.0005, decay: 0.05, sustain: 1.0, release: 0.005 },
        gain_db: -6.0,
        ..Default::default()
    })
}

fn synth_track() -> Track {
    Track::midi("Synth", sine_synth())
}

fn note_clip(pitch: u8, len: Beats) -> Clip {
    Clip::midi("c", len, vec![Note::new(pitch, 0.0, len, 100)])
}

fn engine_for(p: &Project) -> (Engine, EngineHandle) {
    create(Core::from_project(p, SR, &NoSamples, &NoPlugins))
}

fn render(e: &mut Engine, frames: usize) -> Vec<f32> {
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(333);
        e.process(&mut l[done..done + n], &mut r[done..done + n]);
        done += n;
    }
    l
}

fn freq(buf: &[f32]) -> f32 {
    buf.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32 / (buf.len() as f32 / SR)
}

fn first_onset(buf: &[f32]) -> Option<usize> {
    buf.iter().position(|s| s.abs() > 1e-4)
}

#[test]
fn clip_follow_action_moves_to_next_clip_exactly() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_scene("2");
    p.add_track(synth_track());
    p.tracks[0].slots[0] = Some(Clip { follow: Some(FollowAction::next_after(4.0)), ..note_clip(57, 4.0) });
    p.tracks[0].slots[1] = Some(note_clip(69, 4.0));
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let a = render(&mut e, beats(4.0, 120.0) + 1);
    assert_eq!(e.core().playing_slot(0), Some(1), "follow action should have fired at beat 4");
    let b = render(&mut e, beats(2.0, 120.0));
    assert!((freq(&a[4_800..]) - 220.0).abs() < 4.0);
    assert!((freq(&b[4_800..]) - 440.0).abs() < 6.0);
}

#[test]
fn follow_stop_and_linked() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track());
    let f = FollowAction { action_a: FollowKind::Stop, linked: true, ..Default::default() };
    p.tracks[0].slots[0] = Some(Clip { follow: Some(f), ..note_clip(60, 2.0) });
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    render(&mut e, beats(1.9, 120.0));
    assert_eq!(e.core().playing_slot(0), Some(0));
    render(&mut e, beats(0.2, 120.0));
    assert_eq!(e.core().playing_slot(0), None, "linked follow fires after one clip length");
}

#[test]
fn scene_follow_action_launches_next_scene() {
    let mut p = Project::default();
    p.add_scene("A");
    p.add_scene("B");
    p.scenes[0].follow = Some(FollowAction::next_after(8.0));
    p.add_track(synth_track());
    p.tracks[0].slots[0] = Some(note_clip(57, 4.0));
    p.tracks[0].slots[1] = Some(note_clip(69, 4.0));
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchScene { scene: 0 }).ok();
    render(&mut e, beats(7.9, 120.0));
    assert_eq!(e.core().playing_slot(0), Some(0));
    render(&mut e, beats(0.2, 120.0));
    assert_eq!(e.core().playing_slot(0), Some(1));
}

#[test]
fn group_tracks_sum_children_through_their_chain() {
    let mut p = Project::default();
    p.add_scene("1");
    let g = p.add_track(Track::group("Group").with_effect(DeviceSpec::Utility(UtilityParams { gain_db: -35.0, ..Default::default() })));
    let a = p.add_track(synth_track().in_group(g));
    let b = p.add_track(synth_track().in_group(g));
    p.tracks[a].slots[0] = Some(note_clip(60, 4.0));
    p.tracks[b].slots[0] = Some(note_clip(64, 4.0));
    let (mut e, mut h) = engine_for(&p);
    // Group slot launches both children.
    h.send(Command::LaunchClip { track: g, slot: 0 }).ok();
    let out = render(&mut e, 9_600);
    assert_eq!(e.core().playing_slot(a), Some(0));
    assert_eq!(e.core().playing_slot(b), Some(0));
    let level = peak(&out[4_800..]);
    // Two sines at -6 dB each, then -35 dB in the group: well under 0.05.
    assert!(level > 1e-3 && level < 0.05, "{level}");
    // Muting the group silences everything inside it.
    h.send(Command::SetMute { channel: TrackRef::Track(g), mute: true }).ok();
    let out = render(&mut e, 4_800);
    assert!(peak(&out[1_000..]) < 1e-6);
}

#[test]
fn soloing_a_child_keeps_its_group_audible() {
    let mut p = Project::default();
    p.add_scene("1");
    let g = p.add_track(Track::group("Group"));
    let a = p.add_track(synth_track().in_group(g));
    let other = p.add_track(synth_track());
    p.tracks[a].slots[0] = Some(note_clip(60, 4.0));
    p.tracks[other].slots[0] = Some(note_clip(72, 4.0));
    p.tracks[a].mixer.solo = true;
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchScene { scene: 0 }).ok();
    let out = render(&mut e, 24_000);
    assert!((freq(&out[4_800..]) - 261.6).abs() < 5.0, "only the soloed child should sound: {}", freq(&out[4_800..]));
}

#[test]
fn midi_effects_transform_notes_before_the_instrument() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track().with_midi_effect(DeviceSpec::Pitch(PitchParams { semitones: 12, range: None })));
    p.tracks[0].slots[0] = Some(note_clip(57, 4.0));
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let out = render(&mut e, 24_000);
    assert!((freq(&out[4_800..]) - 440.0).abs() < 6.0, "{}", freq(&out[4_800..]));
}

#[test]
fn arpeggiator_runs_from_live_input_even_when_stopped() {
    let mut p = Project::default();
    p.add_track(synth_track().with_midi_effect(DeviceSpec::Arpeggiator(ArpParams { rate: 0.5, gate: 0.25, ..Default::default() })));
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::NoteOn { track: 0, note: 60, velocity: 100 }).ok();
    let out = render(&mut e, beats(2.0, 120.0));
    // Four steps of 1/8 note each (gate 25%), with silence between.
    let step = beats(0.5, 120.0);
    for k in 0..4 {
        assert!(rms(&out[k * step + 100..k * step + 1_000]) > 0.01, "step {k} silent");
        assert!(peak(&out[k * step + step / 2..(k + 1) * step - 100]) < 1e-3, "step {k} didn't gate");
    }
}

fn input_engine(p: &Project) -> (Engine, EngineHandle) {
    engine_for(p)
}

fn process_with_input(e: &mut Engine, input: &[f32], frames: usize) -> Vec<f32> {
    // Stereo in, stereo out, interleaved.
    let mut out = vec![0.0; frames * 2];
    let mut scratch = vec![0.0; 2 * 8192];
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(512);
        let inp = &input[done * 2..(done + n) * 2];
        e.process_io(inp, 2, &mut out[done * 2..(done + n) * 2], 2, &mut scratch);
        done += n;
    }
    out.chunks(2).map(|c| c[0]).collect()
}

fn stereo_sine(hz: f32, frames: usize, amp: f32) -> Vec<f32> {
    (0..frames).flat_map(|i| {
        let s = (std::f32::consts::TAU * hz * i as f32 / SR).sin() * amp;
        [s, s]
    }).collect()
}

#[test]
fn armed_audio_track_monitors_input() {
    let mut p = Project::default();
    p.add_scene("1");
    let t = p.add_track(Track::audio("Mic"));
    p.tracks[t].armed = true;
    let (mut e, _h) = input_engine(&p);
    let input = stereo_sine(330.0, 9_600, 0.3);
    let out = process_with_input(&mut e, &input, 9_600);
    assert!((rms(&out[1_000..]) - 0.212).abs() < 0.02, "monitoring level {}", rms(&out[1_000..]));

    // Unarmed + Auto: silent.
    let mut p2 = p.clone();
    p2.tracks[t].armed = false;
    let (mut e2, _) = input_engine(&p2);
    let out = process_with_input(&mut e2, &input, 9_600);
    assert!(peak(&out) < 1e-6);
}

#[test]
fn audio_recording_streams_the_input() {
    let mut p = Project::default();
    p.add_scene("1");
    p.quantization = Quantization::None;
    let t = p.add_track(Track::audio("Mic"));
    p.tracks[t].armed = true;
    let (mut e, mut h) = input_engine(&p);
    let (tx, mut rx) = record_ring(SR, 30.0);
    let rec = Box::new(AudioRecord { track: t, target: RecTarget::Slot(0), length: Some(2.0), tx, start: 0.0, recorded_length: 0.0, frames: 0, dropped: 0 });
    h.send(Command::RecordAudio(rec, RecStart::Now)).ok();
    let frames = beats(3.0, 120.0);
    let input = stereo_sine(440.0, frames, 0.5);
    process_with_input(&mut e, &input, frames);
    let done = h.poll_all();
    assert_eq!(done.audio.len(), 1);
    let a = &done.audio[0];
    assert_eq!(a.frames as usize, beats(2.0, 120.0), "recorded exactly 2 beats");
    assert_eq!(a.dropped, 0);
    assert!((a.recorded_length - 2.0).abs() < 1e-9);
    let mut samples = Vec::new();
    while let Ok(s) = rx.pop() {
        samples.push(s);
    }
    assert_eq!(samples.len(), beats(2.0, 120.0) * 2);
    // Recorded audio equals the input, sample for sample.
    for i in (0..samples.len()).step_by(997) {
        assert!((samples[i] - input[i]).abs() < 1e-6, "sample {i}");
    }
}

#[test]
fn open_ended_midi_recording_rounds_to_the_bar() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track());
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::Play).ok();
    h.send(Command::RecordMidi(RecordBuffer::with(0, RecTarget::Slot(0), None), RecStart::Now)).ok();
    render(&mut e, beats(1.0, 120.0));
    h.send(Command::NoteOn { track: 0, note: 62, velocity: 90 }).ok();
    render(&mut e, beats(1.0, 120.0));
    h.send(Command::NoteOff { track: 0, note: 62 }).ok();
    render(&mut e, beats(3.0, 120.0)); // now at beat 5
    h.send(Command::StopRecording { track: Some(0) }).ok();
    render(&mut e, beats(4.0, 120.0));
    let rec = h.poll();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].recorded_length, 8.0, "5.x beats rounds up to 2 bars");
    assert_eq!(rec[0].notes.len(), 1);
}

#[test]
fn overdub_records_relative_to_the_playing_clip() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track());
    p.tracks[0].slots[0] = Some(note_clip(48, 4.0));
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    render(&mut e, beats(4.5, 120.0)); // clip loops; now at clip-local 0.5
    h.send(Command::RecordMidi(RecordBuffer::with(0, RecTarget::Slot(0), Some(4.0)), RecStart::Now)).ok();
    render(&mut e, beats(0.5, 120.0));
    h.send(Command::NoteOn { track: 0, note: 67, velocity: 90 }).ok();
    render(&mut e, beats(1.0, 120.0));
    h.send(Command::NoteOff { track: 0, note: 67 }).ok();
    render(&mut e, beats(4.0, 120.0));
    let rec = h.poll();
    assert_eq!(rec.len(), 1);
    assert!(rec[0].overdub.is_some());
    let n = rec[0].notes[0];
    assert!((n.start - 1.0).abs() < 0.02, "clip-local start {}", n.start);
    assert!((n.length - 1.0).abs() < 0.02);
}

#[test]
fn arrangement_recording_captures_notes_at_song_position() {
    let mut p = Project::default();
    p.add_track(synth_track());
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::Seek(8.0)).ok();
    h.send(Command::Play).ok();
    h.send(Command::RecordMidi(RecordBuffer::with(0, RecTarget::Arrangement, None), RecStart::Now)).ok();
    render(&mut e, beats(1.0, 120.0));
    h.send(Command::NoteOn { track: 0, note: 60, velocity: 100 }).ok();
    render(&mut e, beats(0.5, 120.0));
    h.send(Command::NoteOff { track: 0, note: 60 }).ok();
    render(&mut e, beats(0.5, 120.0));
    h.send(Command::Stop).ok();
    render(&mut e, 256);
    let rec = h.poll();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].target, RecTarget::Arrangement);
    assert!((rec[0].start - 8.0).abs() < 1e-9);
    assert!((rec[0].notes[0].start - 1.0).abs() < 0.02);
}

#[test]
fn delay_compensation_aligns_tracks() {
    let drum = || Track::midi("D", DeviceSpec::DrumRack(DrumRackParams::default()));
    let kick = Clip::midi("k", 4.0, vec![Note::new(36, 0.0, 0.25, 127)]);
    let onset_with_solo = |solo: usize| {
        let mut p = Project::default();
        p.add_scene("1");
        p.add_track(drum().with_effect(DeviceSpec::Limiter(LimiterParams { gain_db: 0.0, ceiling_db: 0.0, lookahead_ms: 2.0, ..Default::default() })));
        p.add_track(drum());
        p.tracks[0].slots[0] = Some(kick.clone());
        p.tracks[1].slots[0] = Some(kick.clone());
        p.tracks[solo].mixer.solo = true;
        let (mut e, mut h) = engine_for(&p);
        h.send(Command::LaunchScene { scene: 0 }).ok();
        let out = render(&mut e, 9_600);
        (first_onset(&out).unwrap(), e.core().latency())
    };
    let (a, lat) = onset_with_solo(0);
    let (b, _) = onset_with_solo(1);
    assert_eq!(lat, 96);
    assert_eq!(a, b, "limiter track and plain track must line up");
    assert_eq!(b, lat, "everything is delayed by the total latency");
}

#[test]
fn count_in_delays_the_transport() {
    let p = Project { metronome: true, ..Default::default() };
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::PlayWithCountIn { bars: 1 }).ok();
    let out = render(&mut e, beats(3.5, 120.0));
    assert!(!e.core().is_playing());
    assert_eq!(e.core().beat(), 0.0);
    assert!(peak(&out) > 0.1, "count-in clicks");
    render(&mut e, beats(1.0, 120.0));
    assert!(e.core().is_playing());
    assert!((e.core().beat() - 0.5).abs() < 0.01, "{}", e.core().beat());
}

#[test]
fn tempo_automation_changes_tempo_on_time() {
    let mut p = Project::default();
    p.tempo_automation = vec![(0.0, 120.0), (4.0, 60.0)];
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::Play).ok();
    render(&mut e, beats(4.0, 120.0) - 10);
    assert_eq!(e.core().tempo(), 120.0);
    render(&mut e, 20);
    assert_eq!(e.core().tempo(), 60.0);
    // A manual tempo change overrides automation until Back to Arrangement.
    h.send(Command::SetTempo(100.0)).ok();
    render(&mut e, 256);
    assert_eq!(e.core().tempo(), 100.0);
    h.send(Command::BackToArrangement).ok();
    render(&mut e, 256);
    assert_eq!(e.core().tempo(), 60.0);
}

#[test]
fn crossfader_fades_assigned_tracks() {
    let mut p = Project::default();
    p.add_scene("1");
    let a = p.add_track(synth_track());
    p.tracks[a].crossfade = CrossfadeAssign::A;
    p.tracks[a].slots[0] = Some(note_clip(60, 4.0));
    p.crossfader = 1.0; // all the way to B
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::LaunchScene { scene: 0 }).ok();
    let out = render(&mut e, 4_800);
    assert!(peak(&out[1_000..]) < 1e-6);
    h.send(Command::SetCrossfader(-1.0)).ok();
    let out = render(&mut e, 4_800);
    assert!(rms(&out[1_000..]) > 0.05);
}

#[test]
fn cue_mode_sends_soloed_tracks_to_cue_output() {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_track(synth_track());
    p.add_track(synth_track());
    p.tracks[0].slots[0] = Some(note_clip(60, 4.0));
    p.tracks[1].slots[0] = Some(note_clip(72, 4.0));
    p.tracks[0].mixer.solo = true;
    let (mut e, mut h) = engine_for(&p);
    h.send(Command::SetCueMode(true)).ok();
    h.send(Command::LaunchScene { scene: 0 }).ok();
    let mut out = vec![0.0; 4 * 24_000];
    let mut scratch = vec![0.0; 16_384];
    for c in out.chunks_mut(4 * 512) {
        e.process_io(&[], 0, c, 4, &mut scratch);
    }
    let main: Vec<f32> = out.chunks(4).map(|c| c[0]).collect();
    let cue: Vec<f32> = out.chunks(4).map(|c| c[2]).collect();
    // Main still has both tracks; cue has only the soloed one.
    assert!(rms(&main[4_800..]) > rms(&cue[4_800..]) * 1.2);
    assert!((freq(&cue[4_800..]) - 261.6).abs() < 5.0, "{}", freq(&cue[4_800..]));
    assert!(e.core().state().cue.peak_l.load() > 0.0);
    let _ = Relaxed;
}
