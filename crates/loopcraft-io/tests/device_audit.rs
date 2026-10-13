//! Render every built-in device inside a real project (offline, through
//! the full engine) and check the output is finite, audible where it
//! should be, and not wildly over full scale.

use loopcraft_core::*;
use loopcraft_engine::NoPlugins;
use loopcraft_io::{RenderMode, RenderOptions, render};

fn phrase() -> Clip {
    let notes = (0..8).map(|i| Note::new([48, 55, 60, 64, 67, 72, 60, 52][i], i as f64 * 0.5, 0.45, 70 + (i as u8) * 6)).collect();
    Clip::midi("phrase", 4.0, notes)
}

fn opts() -> RenderOptions {
    RenderOptions { sample_rate: 48_000, mode: RenderMode::Arrangement { length: Some(8.0) }, tail_seconds: 1.0 }
}

fn check(name: &str, p: &Project, expect_sound: bool) {
    let r = render(p, std::path::Path::new("."), &opts(), &NoPlugins).expect("render");
    assert!(r.left.iter().chain(r.right.iter()).all(|x| x.is_finite()), "{name}: non-finite samples");
    let pk = r.peak();
    assert!(pk < 4.0, "{name}: peak {pk} is far over full scale");
    if expect_sound {
        assert!(pk > 1e-3, "{name}: silent (peak {pk})");
    }
}

#[test]
fn every_device_renders_cleanly() {
    let catalog = DeviceSpec::catalog();
    for spec in &catalog {
        let mut p = Project::default();
        p.add_scene("1");
        match spec.kind() {
            DeviceKind::Instrument => {
                let t = p.add_track(Track::midi("i", spec.clone()));
                let clip = if matches!(spec, DeviceSpec::DrumRack(_)) { Clip::midi("kick", 1.0, vec![Note::new(36, 0.0, 0.25, 110)]) } else { phrase() };
                p.tracks[t].arrangement.push(ArrangementClip::looped(0.0, 8.0, clip));
                // Samplers without files are legitimately silent.
                let needs_files = matches!(spec, DeviceSpec::Sampler(_) | DeviceSpec::MultiSampler(_));
                check(spec.name(), &p, !needs_files && !matches!(spec, DeviceSpec::InstrumentRack(_)));
            }
            DeviceKind::MidiEffect => {
                let t = p.add_track(Track::midi("m", DeviceSpec::PolySynth(Default::default())).with_midi_effect(spec.clone()));
                p.tracks[t].arrangement.push(ArrangementClip::looped(0.0, 8.0, phrase()));
                check(spec.name(), &p, true);
            }
            DeviceKind::AudioEffect => {
                let t = p.add_track(Track::midi("a", DeviceSpec::PolySynth(Default::default())).with_effect(spec.clone()));
                p.tracks[t].arrangement.push(ArrangementClip::looped(0.0, 8.0, phrase()));
                // Gates and vocoders (no carrier/sidechain) may legitimately be quiet.
                let may_be_quiet = matches!(spec, DeviceSpec::Gate(_) | DeviceSpec::Vocoder(_) | DeviceSpec::AudioRack(_));
                check(spec.name(), &p, !may_be_quiet);
            }
        }
    }
}

#[test]
fn demo_song_renders_without_clipping() {
    let dir = std::env::temp_dir().join(format!("lc-demo-audit-{}", std::process::id()));
    let path = loopcraft_io::demo::write_demo(&dir, 48_000).unwrap();
    let p = Project::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let opts = RenderOptions { sample_rate: 48_000, mode: RenderMode::Arrangement { length: None }, tail_seconds: 2.0 };
    let r = render(&p, &dir, &opts, &NoPlugins).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let pk = r.peak();
    assert!(pk > 0.1 && pk <= 1.0, "demo peak {pk}");
    // No sample-to-sample jumps that would be heard as clicks.
    let max_step = r.left.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(max_step < 0.9, "largest sample step {max_step}");
    let _ = std::fs::remove_dir_all(dir);
}
