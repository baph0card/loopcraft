//! App-level tests on a headless App (no audio device, no MIDI).

use super::*;
use loopcraft_engine::{DeviceRef, RecTarget, RecordBuffer};

fn app_with_midi_track() -> App {
    let mut p = Project::default();
    p.add_scene("1");
    p.add_scene("2");
    p.add_track(Track::midi("Synth", DeviceSpec::PolySynth(Default::default())));
    p.add_track(Track::audio("Audio"));
    App::headless(p)
}

fn notes_clip(len: f64, at: &[f64]) -> Clip {
    Clip::midi("c", len, at.iter().map(|&b| Note::new(60, b, 0.25, 100)).collect())
}

#[test]
fn split_clear_and_consolidate_regions() {
    let mut app = app_with_midi_track();
    app.project.tracks[0].arrangement.push(ArrangementClip::looped(0.0, 8.0, notes_clip(4.0, &[0.0, 2.0])));
    app.split_arrangement_clip(0, 0, 6.0);
    let arr = &app.project.tracks[0].arrangement;
    assert_eq!(arr.len(), 2);
    assert_eq!((arr[0].start, arr[0].duration()), (0.0, 6.0));
    assert_eq!((arr[1].start, arr[1].offset, arr[1].duration()), (6.0, 6.0, 2.0));

    // Clearing the middle of the first region splits it around the hole.
    app.clear_range(0, 1.0, 3.0);
    let arr = &app.project.tracks[0].arrangement;
    assert_eq!(arr.len(), 3);
    assert_eq!((arr[0].start, arr[0].end()), (0.0, 1.0));
    assert_eq!((arr[1].start, arr[1].offset, arr[1].end()), (3.0, 3.0, 6.0));

    // Consolidate the right-hand piece: looped clip from offset 6 → beats 6..8
    // of the loop = clip beats 2..4, so one note at local 0.
    app.consolidate_region(0, 2);
    let a = &app.project.tracks[0].arrangement[2];
    assert_eq!((a.start, a.offset, a.duration()), (6.0, 0.0, 2.0));
    let ClipContent::Midi { notes } = &a.clip.content else { panic!() };
    assert_eq!(notes.iter().map(|n| n.start).collect::<Vec<_>>(), vec![0.0]);

    // Consolidating the middle piece (offset 3, 3 beats) unrolls the loop: notes at clip 4→0 = local 1.
    app.consolidate_region(0, 1);
    let ClipContent::Midi { notes } = &app.project.tracks[0].arrangement[1].clip.content else { panic!() };
    assert_eq!(notes.iter().map(|n| n.start).collect::<Vec<_>>(), vec![1.0]);

    app.undo();
    app.undo();
    app.undo();
    assert_eq!(app.project.tracks[0].arrangement.len(), 1, "undo walks back to before the split");
}

#[test]
fn delete_and_insert_time_shift_everything_after() {
    let mut app = app_with_midi_track();
    app.project.tracks[0].arrangement.push(ArrangementClip::new(8.0, notes_clip(4.0, &[0.0])));
    app.project.tracks[0].automation.push(AutomationLane { target: AutomationTarget::Volume, points: vec![(2.0, -6.0), (10.0, 0.0)] });
    app.project.locators.push((12.0, "Drop".into()));
    app.delete_time(4.0, 6.0);
    assert_eq!(app.project.tracks[0].arrangement[0].start, 6.0);
    assert_eq!(app.project.tracks[0].automation[0].points, vec![(2.0, -6.0), (8.0, 0.0)]);
    assert_eq!(app.project.locators[0].0, 10.0);
    app.insert_time(7.0, 4.0);
    // The region 6..10 is split at 7; the right part moves to 11.
    let arr = &app.project.tracks[0].arrangement;
    assert_eq!(arr.len(), 2);
    assert_eq!((arr[0].start, arr[0].end()), (6.0, 7.0));
    assert_eq!((arr[1].start, arr[1].offset), (11.0, 1.0));
}

#[test]
fn device_indices_stay_consistent() {
    let mut app = app_with_midi_track();
    let ch = TrackRef::Track(0);
    app.add_device(ch, DeviceSpec::Delay(Default::default()));
    app.add_device(ch, DeviceSpec::Reverb(Default::default()));
    app.set_bypass(ch, DeviceRef::Effect(1), true);
    app.project.tracks[0].automation.push(AutomationLane { target: AutomationTarget::Effect { index: 1, param: 0 }, points: vec![(0.0, 0.5)] });
    // Insert at the front: the bypassed reverb is now index 2.
    app.add_device_at(ch, DeviceSpec::Eq(Default::default()), Some(0));
    assert_eq!(app.project.tracks[0].bypassed, vec![2]);
    assert!(matches!(app.project.tracks[0].automation[0].target, AutomationTarget::Effect { index: 2, .. }));
    app.move_device(ch, DeviceRef::Effect(2), true);
    assert!(matches!(app.project.tracks[0].effects[1], DeviceSpec::Reverb(_)));
    assert_eq!(app.project.tracks[0].bypassed, vec![1]);
    app.remove_device(ch, DeviceRef::Effect(0));
    assert_eq!(app.project.tracks[0].bypassed, vec![0]);
    assert!(matches!(app.project.tracks[0].automation[0].target, AutomationTarget::Effect { index: 0, .. }));
    // Instruments and MIDI effects only go on MIDI tracks.
    app.add_device(TrackRef::Track(1), DeviceSpec::Arpeggiator(Default::default()));
    assert!(app.project.tracks[1].midi_effects.is_empty());
    app.add_device(ch, DeviceSpec::Arpeggiator(Default::default()));
    assert_eq!(app.project.tracks[0].midi_effects.len(), 1);
}

#[test]
fn racks_wrap_map_and_edit() {
    let mut app = app_with_midi_track();
    let ch = TrackRef::Track(0);
    app.add_device(ch, DeviceSpec::Saturator(Default::default()));
    app.wrap_in_rack(ch, DeviceRef::Effect(0));
    let DeviceSpec::AudioRack(r) = &app.project.tracks[0].effects[0] else { panic!("wrapped") };
    assert!(matches!(r.chains[0].devices[0], DeviceSpec::Saturator(_)));
    app.start_macro_learn(ch, DeviceRef::Effect(0), 2);
    app.map_macro(0, 0, 0);
    let DeviceSpec::AudioRack(r) = &app.project.tracks[0].effects[0] else { panic!() };
    assert_eq!(r.macros[2].mappings.len(), 1);
    // Nested parameter edits land in the spec and schedule a rebuild.
    app.set_nested_param(ch, DeviceRef::Effect(0), &[(0, 0)], 0, 12.0);
    assert!(app.rebuild_pending);
    let DeviceSpec::AudioRack(r) = &app.project.tracks[0].effects[0] else { panic!() };
    assert_eq!(loopcraft_dsp::specs::get_param(&r.chains[0].devices[0], 0), Some(12.0));
    app.add_rack_chain(ch, DeviceRef::Effect(0), &[]);
    app.add_to_rack_chain(ch, DeviceRef::Effect(0), &[], 1, DeviceSpec::Chorus(Default::default()));
    let DeviceSpec::AudioRack(r) = &app.project.tracks[0].effects[0] else { panic!() };
    assert_eq!(r.chains.len(), 2);
    assert_eq!(r.chains[1].devices.len(), 1);
}

#[test]
fn recordings_become_clips_and_overdubs_merge() {
    let mut app = app_with_midi_track();
    let mut buf = RecordBuffer::with(0, RecTarget::Slot(1), None);
    buf.start = 4.0;
    buf.recorded_length = 4.0;
    buf.notes.push(Note::new(64, 0.5, 1.0, 90));
    app.finish_midi(buf);
    let c = app.project.tracks[0].slots[1].as_ref().expect("clip recorded");
    assert_eq!(c.length, 4.0);

    let mut od = RecordBuffer::with(0, RecTarget::Slot(1), None);
    od.overdub = Some(4.0);
    od.notes.push(Note::new(67, 1.0, 0.5, 90));
    app.finish_midi(od);
    let ClipContent::Midi { notes } = &app.project.tracks[0].slots[1].as_ref().unwrap().content else { panic!() };
    assert_eq!(notes.len(), 2, "overdub merges");

    // Arrangement takes overwrite what they cover.
    app.project.tracks[0].arrangement.push(ArrangementClip::new(0.0, notes_clip(8.0, &[0.0, 5.0])));
    let mut arr = RecordBuffer::with(0, RecTarget::Arrangement, None);
    arr.start = 4.0;
    arr.recorded_length = 2.0;
    arr.notes.push(Note::new(60, 0.0, 0.5, 100));
    app.finish_midi(arr);
    let a = &app.project.tracks[0].arrangement;
    assert_eq!(a.iter().map(|r| (r.start, r.end())).collect::<Vec<_>>(), vec![(0.0, 4.0), (4.0, 6.0), (6.0, 8.0)]);
}

#[test]
fn mappings_drive_targets() {
    let mut app = app_with_midi_track();
    app.map_mode = Some(MapMode::Key);
    app.pick_map_target(MappingTarget::Volume(0));
    assert!(app.key_mapping('q'));
    app.map_mode = None;
    assert_eq!(app.project.mappings.len(), 1);
    // A key toggles a continuous target between its ends.
    app.project.tracks[0].mixer.volume_db = -60.0;
    app.key_mapping('q');
    assert_eq!(app.project.tracks[0].mixer.volume_db, 6.0);
    app.key_mapping('q');
    assert_eq!(app.project.tracks[0].mixer.volume_db, -60.0);
    let m = Mapping { source: MappingSource::Cc { channel: 0, cc: 1 }, target: MappingTarget::Pan(0), min: -1.0, max: 1.0 };
    app.apply_mapping(m, 0.75);
    assert!((app.project.tracks[0].mixer.pan - 0.5).abs() < 1e-6);
    app.trigger(MappingTarget::Arm(0));
    assert!(app.project.tracks[0].armed);
}

#[test]
fn grooves_commit_into_notes() {
    let mut app = app_with_midi_track();
    app.project.grooves.push(Groove::swing("s", 0.75));
    app.project.tracks[0].slots[0] = Some(Clip { groove: Some(0), ..notes_clip(1.0, &[0.0, 0.25, 0.5, 0.75]) });
    let r = ClipRef::Session { track: 0, slot: 0 };
    app.commit_groove(r);
    let c = app.clip(r).unwrap();
    assert!(c.groove.is_none());
    let ClipContent::Midi { notes } = &c.content else { panic!() };
    assert_eq!(notes[0].start, 0.0);
    assert!(notes[1].start > 0.25, "off-beat 16ths are late: {}", notes[1].start);
    // Deleting a groove clears references to it.
    app.project.tracks[0].slots[0].as_mut().unwrap().groove = Some(0);
    app.delete_groove(0);
    assert!(app.clip(r).unwrap().groove.is_none());
}

#[test]
fn save_and_reopen_round_trip() {
    let mut app = app_with_midi_track();
    let dir = std::env::temp_dir().join(format!("loopcraft-app-test-{}", std::process::id()));
    let path = dir.join("song.loopcraft");
    app.project.tracks[0].arrangement.push(ArrangementClip { start: 2.0, duration: Some(3.0), offset: 1.0, clip: notes_clip(4.0, &[1.5]) });
    app.save_to(&path);
    assert!(!app.dirty);
    let mut b = App::headless(Project::default());
    b.open(&path);
    assert_eq!(b.project.tracks.len(), 2);
    assert_eq!(b.project.tracks[0].arrangement[0].offset, 1.0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn loop_passes_split_correctly() {
    use super::record::pass_spans;
    assert_eq!(pass_spans(2.0, 3.0, None), vec![(2.0, 0.0, 3.0)]);
    // Brace 0..4, started at 1, ran 10 beats: 1..4, then 0..4, then 0..3.
    assert_eq!(pass_spans(1.0, 10.0, Some((0.0, 4.0))), vec![(1.0, 0.0, 3.0), (0.0, 3.0, 4.0), (0.0, 7.0, 3.0)]);
    // Started outside the brace: no wrapping happened.
    assert_eq!(pass_spans(6.0, 10.0, Some((0.0, 4.0))), vec![(6.0, 0.0, 10.0)]);
}

#[test]
fn loop_recording_makes_takes_and_comping_swaps_them_in() {
    let mut app = app_with_midi_track();
    app.project.loop_region = Some((0.0, 4.0));
    // Three passes over 0..4, a different pitch each pass, at beat 1 of the loop.
    let mut buf = RecordBuffer::with(0, RecTarget::Arrangement, None);
    buf.start = 0.0;
    buf.recorded_length = 12.0;
    for (k, pitch) in [60u8, 62, 64].into_iter().enumerate() {
        buf.notes.push(Note::new(pitch, k as f64 * 4.0 + 1.0, 0.5, 100));
    }
    app.finish_midi(buf);
    let tr = &app.project.tracks[0];
    assert_eq!(tr.takes.len(), 3);
    assert!(tr.show_takes);
    // The timeline plays the latest pass.
    assert_eq!(tr.arrangement.len(), 1);
    let ClipContent::Midi { notes } = &tr.arrangement[0].clip.content else { panic!() };
    assert_eq!(notes.iter().map(|n| (n.pitch, n.start)).collect::<Vec<_>>(), vec![(64, 1.0)]);
    // Comp the first half from take 1.
    app.comp_from_take(0, 0, 0.0, 2.0);
    let arr = &app.project.tracks[0].arrangement;
    assert_eq!(arr.iter().map(|r| (r.start, r.end())).collect::<Vec<_>>(), vec![(0.0, 2.0), (2.0, 4.0)]);
    let ClipContent::Midi { notes } = &arr[0].clip.content else { panic!() };
    assert_eq!(notes[0].pitch, 60);
    app.undo();
    assert_eq!(app.project.tracks[0].arrangement.len(), 1);
}

#[test]
fn session_launches_record_into_the_arrangement() {
    let mut app = app_with_midi_track();
    app.project.tracks[0].slots[0] = Some(Clip::midi("a", 4.0, vec![Note::new(60, 0.0, 1.0, 100)]));
    app.project.tracks[0].slots[1] = Some(Clip { looping: false, ..Clip::midi("b", 2.0, vec![Note::new(62, 0.0, 1.0, 100)]) });
    app.arrangement_recording = true;
    app.session_changed(0, Some(0), 0.0, 0.0);
    // Slot 1 launched at beat 6 (slot 0 was 2 beats into its second loop).
    app.session_changed(0, Some(1), 6.0, 6.0);
    app.session_changed(0, None, 12.0, 12.0);
    let arr = &app.project.tracks[0].arrangement;
    assert_eq!(
        arr.iter().map(|r| (r.start, r.end(), r.offset)).collect::<Vec<_>>(),
        vec![(0.0, 6.0, 0.0), (6.0, 8.0, 0.0)],
        "the one-shot stops at its own end"
    );
    // Launching mid-clip records the right offset.
    app.session_changed(0, Some(0), 10.0, 13.0);
    app.close_session_regions(16.0);
    let r = app.project.tracks[0].arrangement.last().unwrap();
    assert_eq!((r.start, r.offset, r.end()), (13.0, 3.0, 16.0));
}

#[test]
fn capture_takes_the_last_phrase() {
    let mut app = app_with_midi_track();
    let mut buf = RecordBuffer::with(0, RecTarget::Capture, None);
    // An old phrase, a long gap, then three notes starting at beat 41.
    buf.notes.push(Note::new(50, 2.0, 0.5, 100));
    for k in 0..3 {
        buf.notes.push(Note::new(60 + k, 41.0 + k as f64, 0.5, 100));
    }
    app.finish_midi(buf);
    let c = app.project.tracks[0].slots[0].as_ref().expect("captured into the first free slot");
    assert_eq!(c.length, 4.0, "one bar, from the bar line before the first note");
    let ClipContent::Midi { notes } = &c.content else { panic!() };
    assert_eq!(notes.iter().map(|n| n.start).collect::<Vec<_>>(), vec![1.0, 2.0, 3.0]);
}
