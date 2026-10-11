//! A demo song that exercises most of the engine: Session clips and
//! scenes (with a scene tempo change), a warped audio loop, sidechain
//! compression, sends/returns, an Arrangement with automation, and a master
//! chain. Everything is synthesized, so it needs no sample library.

use crate::audio::{BitDepth, write_wav};
use anyhow::Result;
use loopcraft_core::*;
use loopcraft_dsp::NoSamples;
use loopcraft_engine::{Command, Core, NoPlugins, create};
use std::path::Path;

pub const BREAK_FILE: &str = "break_100bpm.wav";

fn steps(pitch: u8, positions: &[f64], len: Beats, vel: u8) -> Vec<Note> {
    positions.iter().map(|&p| Note::new(pitch, p, len, vel)).collect()
}

fn every(step: f64, offset: f64, until: f64) -> Vec<f64> {
    let mut v = Vec::new();
    let mut t = offset;
    while t < until - 1e-9 {
        v.push(t);
        t += step;
    }
    v
}

fn drum_clip(name: &str, open_hats: bool, len: Beats) -> Clip {
    let mut n = steps(36, &every(1.0, 0.0, len), 0.1, 120);
    n.extend(steps(39, &every(2.0, 1.0, len), 0.1, 105));
    if open_hats {
        n.extend(steps(46, &every(1.0, 0.5, len), 0.1, 90));
        n.extend(steps(42, &every(1.0, 0.25, len), 0.05, 60));
        n.extend(steps(42, &every(1.0, 0.75, len), 0.05, 70));
    } else {
        n.extend(steps(42, &every(0.5, 0.5, len), 0.05, 80));
    }
    // A little humanizing ghost snare that only sometimes plays.
    n.push(Note { probability: 0.5, ..Note::new(38, len - 0.25, 0.1, 50) });
    Clip { color: [0xf0, 0x80, 0x50], ..Clip::midi(name, len, n) }
}

const PROGRESSION: [[u8; 3]; 4] = [[57, 60, 64], [53, 57, 60], [55, 60, 64], [55, 59, 62]];
const ROOTS: [u8; 4] = [33, 29, 36, 31];

fn chords_clip() -> Clip {
    let mut notes = Vec::new();
    for (bar, chord) in PROGRESSION.iter().enumerate() {
        for &p in chord {
            notes.push(Note::new(p, bar as f64 * 4.0, 3.9, 80));
        }
    }
    Clip { color: [0x70, 0xa0, 0xf0], ..Clip::midi("Am F C G", 16.0, notes) }
}

fn bass_clip(name: &str, busy: bool) -> Clip {
    let mut notes = Vec::new();
    for (bar, &root) in ROOTS.iter().enumerate() {
        let b = bar as f64 * 4.0;
        let pos: Vec<f64> = if busy { every(0.5, 0.5, 4.0) } else { vec![0.5, 1.5, 2.5, 3.5] };
        notes.push(Note::new(root, b, 0.4, 115));
        for p in pos {
            notes.push(Note::new(root + if busy && (p * 2.0) as i32 % 4 == 3 { 12 } else { 0 }, b + p, 0.35, 100));
        }
    }
    Clip { color: [0x90, 0xe0, 0x70], ..Clip::midi(name, 16.0, notes) }
}

fn lead_clip() -> Clip {
    // A minor pentatonic phrase.
    let phrase: [(u8, f64, f64); 10] =
        [(76, 0.0, 0.75), (79, 0.75, 0.75), (81, 1.5, 1.0), (79, 2.5, 0.5), (76, 3.0, 1.0), (74, 4.5, 0.5), (72, 5.0, 0.5), (74, 5.5, 0.5), (76, 6.0, 1.5), (69, 7.5, 0.5)];
    let notes = phrase.iter().map(|&(p, s, l)| Note::new(p, s, l, 95)).collect();
    Clip { color: [0xe0, 0x60, 0xc0], ..Clip::midi("Hook", 8.0, notes) }
}

/// Render the drum break used by the audio track, using the engine itself.
pub fn render_break(dir: &Path, sample_rate: u32) -> Result<()> {
    let mut p = Project { tempo: 100.0, ..Default::default() };
    p.add_scene("1");
    let rack = DrumRackParams {
        pads: DrumRackParams::default().pads.into_iter().map(|mut pad| {
            pad.tune_semitones = 2.0;
            pad
        }).collect(),
        ..Default::default()
    };
    let t = p.add_track(Track::midi("Break", DeviceSpec::DrumRack(rack)));
    let mut n = Vec::new();
    for bar in 0..2 {
        let b = bar as f64 * 4.0;
        n.extend(steps(36, &[b, b + 1.75, b + 2.5], 0.1, 120));
        n.extend(steps(38, &[b + 1.0, b + 3.0, b + 3.75], 0.1, 110));
        n.extend(steps(42, &every(0.25, b, b + 4.0), 0.05, 70));
        n.push(Note::new(37, b + 2.25, 0.1, 90));
    }
    p.tracks[t].slots[0] = Some(Clip::midi("break", 8.0, n));
    p.tracks[t].effects.push(DeviceSpec::Saturator(SaturatorParams { drive_db: 9.0, output_db: -4.0, dry_wet: 0.6 }));
    let sr = sample_rate as f32;
    let (mut engine, mut h) = create(Core::from_project(&p, sr, &NoSamples, &NoPlugins));
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let frames = (8.0 * 60.0 / 100.0 * sr) as usize;
    let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
    engine.process(&mut l, &mut r);
    write_wav(&dir.join(BREAK_FILE), &l, &r, sample_rate, BitDepth::Int24)
}

pub fn demo_project() -> Project {
    let mut p = Project { name: "Night Drive".into(), tempo: 124.0, ..Default::default() };

    // Returns first so every track gets a send for each.
    p.add_return(Track::new("A Reverb", TrackKind::Return).with_effect(DeviceSpec::Reverb(ReverbParams { size: 0.82, dry_wet: 1.0, ..Default::default() })));
    p.add_return(Track::new("B Delay", TrackKind::Return).with_effect(DeviceSpec::Delay(DelayParams { dry_wet: 1.0, feedback: 0.45, time_beats: 0.75, ..Default::default() })));

    for name in ["Intro", "Groove", "Peak", "Breakdown"] {
        p.add_scene(name);
    }
    p.scenes[3].tempo = Some(118.0);

    // 0: Drums
    let drums = p.add_track(
        Track::midi("Drums", DeviceSpec::DrumRack(DrumRackParams::default()))
            .with_color([0xf0, 0x80, 0x50])
            .with_effect(DeviceSpec::Eq(EqParams::default())),
    );
    p.tracks[drums].slots[0] = Some(drum_clip("Beat", false, 4.0));
    p.tracks[drums].slots[1] = Some(drum_clip("Beat", false, 4.0));
    p.tracks[drums].slots[2] = Some(drum_clip("Beat + Hats", true, 4.0));
    p.tracks[drums].sends[0] = -22.0;

    // 1: Bass, ducked by the kick via sidechain compression.
    let bass_synth = SynthParams {
        osc1: Waveform::Saw,
        osc2: Waveform::Square,
        osc2_semitones: -12.0,
        osc_mix: 0.5,
        cutoff_hz: 380.0,
        resonance: 0.35,
        filter_env_amount: 2.5,
        amp_env: Adsr { attack: 0.002, decay: 0.15, sustain: 0.6, release: 0.06 },
        filter_env: Adsr { attack: 0.001, decay: 0.12, sustain: 0.0, release: 0.1 },
        voices: 4,
        gain_db: -7.0,
        ..Default::default()
    };
    let bass = p.add_track(
        Track::midi("Bass", DeviceSpec::PolySynth(bass_synth))
            .with_color([0x90, 0xe0, 0x70])
            .with_effect(DeviceSpec::Saturator(SaturatorParams { drive_db: 4.0, output_db: -2.0, dry_wet: 0.5 }))
            .with_effect(DeviceSpec::Compressor(CompressorParams {
                threshold_db: -30.0,
                ratio: 8.0,
                attack_ms: 1.0,
                release_ms: 140.0,
                knee_db: 3.0,
                sidechain_track: Some(drums),
                ..Default::default()
            })),
    );
    p.tracks[bass].slots[1] = Some(bass_clip("Pump", false));
    p.tracks[bass].slots[2] = Some(bass_clip("Rolling", true));

    // 2: Chords through an auto filter, sent to the reverb.
    let pad = SynthParams {
        osc1: Waveform::Saw,
        osc2: Waveform::Saw,
        osc2_detune_cents: 12.0,
        osc_mix: 0.5,
        cutoff_hz: 1_400.0,
        resonance: 0.15,
        filter_env_amount: 0.8,
        amp_env: Adsr { attack: 0.35, decay: 0.8, sustain: 0.8, release: 0.9 },
        filter_env: Adsr { attack: 0.6, decay: 1.2, sustain: 0.3, release: 1.0 },
        voices: 12,
        gain_db: -14.0,
        ..Default::default()
    };
    let chords = p.add_track(
        Track::midi("Chords", DeviceSpec::PolySynth(pad))
            .with_color([0x70, 0xa0, 0xf0])
            .with_effect(DeviceSpec::AutoFilter(AutoFilterParams { cutoff_hz: 1_500.0, lfo_beats: 8.0, lfo_amount: 1.2, ..Default::default() }))
            .with_effect(DeviceSpec::Utility(UtilityParams { width: 1.4, ..Default::default() })),
    );
    for s in 0..4 {
        p.tracks[chords].slots[s] = Some(chords_clip());
    }
    p.tracks[chords].sends[0] = -6.0;
    p.tracks[chords].mixer.volume_db = -3.0;

    // 3: Lead with the delay return.
    let lead_synth = SynthParams {
        osc1: Waveform::Square,
        osc2: Waveform::Triangle,
        osc2_semitones: 12.0,
        osc_mix: 0.3,
        cutoff_hz: 2_600.0,
        resonance: 0.3,
        filter_env_amount: 1.5,
        amp_env: Adsr { attack: 0.004, decay: 0.25, sustain: 0.5, release: 0.2 },
        voices: 2,
        glide: 0.03,
        gain_db: -15.0,
        ..Default::default()
    };
    let lead = p.add_track(Track::midi("Lead", DeviceSpec::PolySynth(lead_synth)).with_color([0xe0, 0x60, 0xc0]));
    p.tracks[lead].slots[2] = Some(lead_clip());
    p.tracks[lead].slots[3] = Some(lead_clip());
    p.tracks[lead].sends = vec![-12.0, -8.0];
    p.tracks[lead].mixer.pan = 0.15;

    // 4: Warped audio loop: a 100 bpm break stretched to song tempo.
    let brk = p.add_track(Track::audio("Break").with_color([0xf0, 0xd0, 0x50]).with_effect(DeviceSpec::Eq(EqParams {
        bands: vec![EqBand { kind: EqBandKind::LowCut, freq_hz: 180.0, gain_db: 0.0, q: 0.71, enabled: true }],
        output_db: -3.0,
    })));
    let audio = AudioClip { file: BREAK_FILE.into(), warp: WarpMode::Stretch, original_bpm: 100.0, ..Default::default() };
    p.tracks[brk].slots[2] = Some(Clip { color: [0xf0, 0xd0, 0x50], ..Clip::audio("Break (warped)", 8.0, audio) });
    p.tracks[brk].mixer.volume_db = -9.0;

    // Master chain: tidy low end, gentle glue compression, safety trim.
    p.master.effects = vec![
        DeviceSpec::Eq(EqParams {
            bands: vec![EqBand { kind: EqBandKind::LowCut, freq_hz: 25.0, gain_db: 0.0, q: 0.71, enabled: true }],
            output_db: 0.0,
        }),
        DeviceSpec::Compressor(CompressorParams { threshold_db: -10.0, ratio: 2.0, attack_ms: 20.0, release_ms: 200.0, makeup_db: 1.0, ..Default::default() }),
        DeviceSpec::Saturator(SaturatorParams { drive_db: 0.0, output_db: -1.0, dry_wet: 1.0 }),
    ];
    p.master.mixer.volume_db = -2.0;

    build_arrangement(&mut p, drums, bass, chords, lead, brk);
    p
}

/// Lay the same material out on the timeline: 8-bar sections with a filter
/// sweep and a fade-in automated on the chords.
fn build_arrangement(p: &mut Project, drums: usize, bass: usize, chords: usize, lead: usize, brk: usize) {
    let bar = 4.0;
    let take = |p: &Project, t: usize, s: usize| p.tracks[t].slots[s].clone().expect("slot filled above");
    let sections = [(0.0, 8.0), (8.0 * bar, 8.0), (16.0 * bar, 8.0), (24.0 * bar, 8.0)];

    let c = take(p, chords, 0);
    p.tracks[chords].arrangement.push(ArrangementClip::looped(0.0, 32.0 * bar, c));
    p.tracks[chords].automation.push(AutomationLane { target: AutomationTarget::Volume, points: vec![(0.0, -14.0), (8.0 * bar, -3.0)] });
    // Effect 0 = Auto Filter, param 0 = cutoff (normalized 0..1 over 20 Hz..18 kHz).
    p.tracks[chords].automation.push(AutomationLane {
        target: AutomationTarget::Effect { index: 0, param: 0 },
        points: vec![(0.0, 0.05), (16.0 * bar, 0.12), (24.0 * bar, 0.35), (28.0 * bar, 0.06)],
    });

    let (s1, s2, s3) = (sections[1], sections[2], sections[3]);
    let c = take(p, drums, 0);
    p.tracks[drums].arrangement.push(ArrangementClip::looped(s1.0 - 4.0 * bar, 4.0 * bar, c));
    let c = take(p, drums, 1);
    p.tracks[drums].arrangement.push(ArrangementClip::looped(s1.0, s1.1 * bar, c));
    let c = take(p, drums, 2);
    p.tracks[drums].arrangement.push(ArrangementClip::looped(s2.0, s2.1 * bar, c));
    let c = take(p, bass, 1);
    p.tracks[bass].arrangement.push(ArrangementClip::looped(s1.0, s1.1 * bar, c));
    let c = take(p, bass, 2);
    p.tracks[bass].arrangement.push(ArrangementClip::looped(s2.0, s2.1 * bar, c));
    let c = take(p, lead, 2);
    p.tracks[lead].arrangement.push(ArrangementClip::looped(s2.0, s2.1 * bar, c));
    let c = take(p, lead, 3);
    p.tracks[lead].arrangement.push(ArrangementClip::looped(s3.0, 4.0 * bar, c));
    let c = take(p, brk, 2);
    p.tracks[brk].arrangement.push(ArrangementClip::looped(s2.0 + 4.0 * bar, 4.0 * bar, c));
}

/// Write the demo project and its audio into `dir`. Returns the project path.
pub fn write_demo(dir: &Path, sample_rate: u32) -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    render_break(dir, sample_rate)?;
    let path = dir.join("night-drive.loopcraft");
    std::fs::write(&path, demo_project().to_json())?;
    Ok(path)
}
