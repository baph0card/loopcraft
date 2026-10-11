//! Loads the example CLAP library built by `cargo test` and runs both of
//! its plugins through the host and the engine.

use loopcraft_core::*;
use loopcraft_dsp::{Effect, Instrument, NoSamples, ProcessContext, peak};
use loopcraft_engine::{Command, Core, create};
use loopcraft_plugin::{ClapInstance, ClapLibrary, HostFactory, scan_clap};
use std::path::PathBuf;
use std::sync::Arc;

fn plugin_lib() -> PathBuf {
    // target/<profile>/deps/<test-binary> -> target/<profile>/examples/
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let name = format!("{}clap_test_plugins{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
    let p = dir.join(&name);
    assert!(p.exists(), "{} not built (run via `cargo test`)", p.display());
    p
}

#[test]
fn lists_descriptors() {
    let lib = ClapLibrary::load(&plugin_lib()).unwrap();
    let ids: Vec<_> = lib.plugins.iter().map(|p| (p.id.as_str(), p.is_instrument)).collect();
    assert_eq!(ids, vec![("dev.loopcraft.test-gain", false), ("dev.loopcraft.test-sine", true)]);
}

#[test]
fn scanner_finds_clap_files() {
    let dir = std::env::temp_dir().join(format!("lc-scan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(plugin_lib(), dir.join("Test.clap")).unwrap();
    let (found, errors) = scan_clap(&[dir]);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1.len(), 2);
}

#[test]
fn effect_processes_audio() {
    let lib = Arc::new(ClapLibrary::load(&plugin_lib()).unwrap());
    let mut fx = ClapInstance::new(lib, "dev.loopcraft.test-gain", 48_000.0, 256).unwrap();
    let ctx = ProcessContext::new(48_000.0, 120.0);
    // Longer than max_block to exercise internal splitting.
    let mut l = vec![0.8; 1000];
    let mut r = vec![-0.4; 1000];
    Effect::process(&mut fx, &ctx, &mut l, &mut r);
    assert!(l.iter().all(|&x| (x - 0.4).abs() < 1e-6));
    assert!(r.iter().all(|&x| (x + 0.2).abs() < 1e-6));
}

#[test]
fn instrument_responds_to_notes() {
    let lib = Arc::new(ClapLibrary::load(&plugin_lib()).unwrap());
    let mut inst = ClapInstance::new(lib, "dev.loopcraft.test-sine", 48_000.0, 256).unwrap();
    let ctx = ProcessContext::new(48_000.0, 120.0);
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    inst.render(&ctx, &mut l, &mut r);
    assert_eq!(peak(&l), 0.0);
    inst.note_on(69, 127);
    inst.render(&ctx, &mut l, &mut r);
    assert!(peak(&l) > 0.4);
}

#[test]
fn engine_hosts_plugins_from_a_project() {
    let path = plugin_lib().to_string_lossy().into_owned();
    let mut p = Project::default();
    p.add_scene("1");
    let synth = PluginRef { format: PluginFormat::Clap, path: path.clone(), id: "dev.loopcraft.test-sine".into(), name: "Test Sine".into(), is_instrument: true, state: None };
    let gain = PluginRef { format: PluginFormat::Clap, path, id: "dev.loopcraft.test-gain".into(), name: "Test Gain".into(), is_instrument: false, state: None };
    let t = p.add_track(Track::midi("Plugin", DeviceSpec::Plugin(synth)).with_effect(DeviceSpec::Plugin(gain)));
    p.tracks[t].slots[0] = Some(Clip::midi("a", 4.0, vec![Note::new(69, 0.0, 4.0, 127)]));

    let factory = HostFactory::new();
    let core = Core::from_project(&p, 48_000.0, &NoSamples, &factory);
    assert!(factory.errors().is_empty(), "{:?}", factory.errors());
    let (mut e, mut h) = create(core);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let (mut l, mut r) = (vec![0.0; 4800], vec![0.0; 4800]);
    e.process(&mut l, &mut r);
    // Sine at 0.5 amplitude through the 0.5 gain plugin.
    let pk = peak(&l);
    assert!((pk - 0.25).abs() < 0.01, "peak {pk}");
}
