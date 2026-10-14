//! Loads the example CLAP and VST3 libraries built by `cargo test` and
//! runs them through the host and the engine: audio, notes, parameters,
//! latency, state, instance reuse across rebuilds, and editors (the editor
//! tests need an X11 display and are skipped without one).

use loopcraft_core::*;
use loopcraft_dsp::{Device, Effect, Instrument, NoSamples, ProcessContext, peak};
use loopcraft_engine::{Command, Core, PluginFactory, create};
use loopcraft_plugin::*;
use std::path::PathBuf;
use std::sync::Arc;

fn example_lib(name: &str) -> PathBuf {
    // target/<profile>/deps/<test-binary> -> target/<profile>/examples/
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let file = format!("{}{name}{}", std::env::consts::DLL_PREFIX, std::env::consts::DLL_SUFFIX);
    let p = dir.join(&file);
    assert!(p.exists(), "{} not built (run via `cargo test`)", p.display());
    p
}

fn clap_lib() -> PathBuf {
    example_lib("clap_test_plugins")
}

/// The VST3 example wrapped in a real bundle layout, to exercise bundle lookup.
fn vst3_bundle() -> PathBuf {
    let so = example_lib("vst3_test_plugin");
    let root = std::env::temp_dir().join(format!("lc-vst3-{}", std::process::id()));
    // The per-platform layout from the VST3 SDK's bundle format.
    let (dir, file) = if cfg!(target_os = "macos") {
        ("MacOS", "Test Gain")
    } else if cfg!(target_os = "windows") {
        (if cfg!(target_arch = "aarch64") { "arm64-win" } else { "x86_64-win" }, "Test Gain.vst3")
    } else {
        (if cfg!(target_arch = "aarch64") { "aarch64-linux" } else { "x86_64-linux" }, "Test Gain.so")
    };
    let bin_dir = root.join("Test Gain.vst3/Contents").join(dir);
    std::fs::create_dir_all(&bin_dir).unwrap();
    let dst = bin_dir.join(file);
    if !dst.exists() {
        std::fs::copy(&so, &dst).unwrap();
    }
    root.join("Test Gain.vst3")
}

fn have_display() -> bool {
    cfg!(all(unix, not(target_os = "macos"))) && std::env::var_os("DISPLAY").is_some()
}

fn clap_ref(id: &str, name: &str, instrument: bool, instance: u64) -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        path: clap_lib().display().to_string(),
        id: id.into(),
        name: name.into(),
        is_instrument: instrument,
        state: None,
        params: Vec::new(),
        instance,
    }
}

fn process_effect(d: &mut Device, frames: usize, l: f32) -> Vec<f32> {
    let ctx = ProcessContext::new(48_000.0, 120.0);
    let Device::Effect(fx) = d else { panic!("effect") };
    let (mut a, mut b) = (vec![l; frames], vec![l; frames]);
    fx.process(&ctx, &mut a, &mut b);
    a
}

// ---------------------------------------------------------------------------
// CLAP
// ---------------------------------------------------------------------------

#[test]
fn clap_lists_descriptors() {
    let lib = ClapLibrary::load(&clap_lib()).unwrap();
    let ids: Vec<_> = lib.plugins.iter().map(|p| (p.id.as_str(), p.is_instrument)).collect();
    assert_eq!(ids, vec![("dev.loopcraft.test-gain", false), ("dev.loopcraft.test-sine", true)]);
}

#[test]
fn clap_scanner_finds_files() {
    let dir = std::env::temp_dir().join(format!("lc-scan-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(clap_lib(), dir.join("test.clap")).unwrap();
    let (found, errors) = scan_clap(std::slice::from_ref(&dir));
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].1.len(), 2);
}

#[test]
fn clap_effect_processes_with_latency_and_params() {
    let lib = Arc::new(ClapLibrary::load(&clap_lib()).unwrap());
    let s = ClapShared::new(lib, "dev.loopcraft.test-gain", 48_000.0, 256, None).unwrap();
    let mut fx = ClapDevice::new(s.clone(), &[]);
    let ctx = ProcessContext::new(48_000.0, 120.0);
    // Longer than max_block to exercise internal splitting.
    let (mut l, mut r) = (vec![0.8; 1000], vec![-0.4; 1000]);
    Effect::process(&mut fx, &ctx, &mut l, &mut r);
    assert_eq!(fx.latency(), 16);
    assert!(l[..16].iter().all(|&x| x == 0.0));
    assert!(l[16..].iter().all(|&x| (x - 0.4).abs() < 1e-6));
    assert!(r[16..].iter().all(|&x| (x + 0.2).abs() < 1e-6));
    let params = Effect::params(&fx);
    assert_eq!((params.len(), params[0].name, params[0].max), (1, "Gain", 2.0));
    Effect::set_param(&mut fx, 0, 2.0);
    let (mut l, mut r) = (vec![0.25; 256], vec![0.25; 256]);
    Effect::process(&mut fx, &ctx, &mut l, &mut r);
    assert!((l[100] - 0.5).abs() < 1e-6, "gain 2.0 applied: {}", l[100]);
}

#[test]
fn clap_state_round_trips() {
    let lib = Arc::new(ClapLibrary::load(&clap_lib()).unwrap());
    let s = ClapShared::new(lib.clone(), "dev.loopcraft.test-gain", 48_000.0, 256, None).unwrap();
    let mut fx = ClapDevice::new(s.clone(), &[1.25]);
    process_effect_dev(&mut fx);
    let state = s.save_state().expect("plugin supports state");
    let s2 = ClapShared::new(lib, "dev.loopcraft.test-gain", 48_000.0, 256, Some(&state)).unwrap();
    let mut fx2 = ClapDevice::new(s2, &[]);
    let ctx = ProcessContext::new(48_000.0, 120.0);
    let (mut l, mut r) = (vec![0.4; 256], vec![0.4; 256]);
    Effect::process(&mut fx2, &ctx, &mut l, &mut r);
    assert!((l[100] - 0.5).abs() < 1e-6, "restored gain 1.25: {}", l[100]);
}

fn process_effect_dev(fx: &mut ClapDevice) {
    let ctx = ProcessContext::new(48_000.0, 120.0);
    let (mut l, mut r) = (vec![0.0; 64], vec![0.0; 64]);
    Effect::process(fx, &ctx, &mut l, &mut r);
}

#[test]
fn clap_instrument_responds_to_notes() {
    let lib = Arc::new(ClapLibrary::load(&clap_lib()).unwrap());
    let s = ClapShared::new(lib, "dev.loopcraft.test-sine", 48_000.0, 256, None).unwrap();
    let mut inst = ClapDevice::new(s, &[]);
    let ctx = ProcessContext::new(48_000.0, 120.0);
    let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
    inst.render(&ctx, &mut l, &mut r);
    assert_eq!(peak(&l), 0.0);
    inst.note_on(69, 127);
    inst.render(&ctx, &mut l, &mut r);
    assert!(peak(&l) > 0.4);
}

#[test]
fn engine_hosts_plugins_and_reuses_instances_across_rebuilds() {
    let mut p = Project::default();
    p.add_scene("1");
    let synth = clap_ref("dev.loopcraft.test-sine", "Test Sine", true, 0);
    // Saved parameter value: gain 1.0 instead of the default 0.5.
    let gain = PluginRef { params: vec![1.0], ..clap_ref("dev.loopcraft.test-gain", "Test Gain", false, 0) };
    let t = p.add_track(Track::midi("Plugin", DeviceSpec::Plugin(synth)).with_effect(DeviceSpec::Plugin(gain)));
    p.tracks[t].slots[0] = Some(Clip::midi("a", 4.0, vec![Note::new(69, 0.0, 4.0, 127)]));
    let p = p.normalized();
    let mut ids = Vec::new();
    p.for_each_plugin(&mut |r| ids.push(r.instance));
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().all(|&i| i != 0) && ids[0] != ids[1], "instances get unique ids: {ids:?}");

    let factory = HostFactory::new();
    let core = Core::from_project(&p, 48_000.0, &NoSamples, &factory);
    assert!(factory.errors().is_empty(), "{:?}", factory.errors());
    let first = factory.instance(ids[1]).expect("pooled");
    let (mut e, mut h) = create(core);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let (mut l, mut r) = (vec![0.0; 4800], vec![0.0; 4800]);
    e.process(&mut l, &mut r);
    // Sine at 0.5 amplitude through the plugin at its saved gain of 1.0.
    let pk = peak(&l);
    assert!((pk - 0.5).abs() < 0.01, "peak {pk}");
    // The plugin's latency is reported to the engine's delay compensation.
    assert_eq!(e.core().latency(), 16);

    // A rebuild (any edit) reuses the running instances.
    let core2 = Core::from_project(&p, 48_000.0, &NoSamples, &factory);
    let second = factory.instance(ids[1]).expect("still pooled");
    match (&first, &second) {
        (Instance::Clap(a), Instance::Clap(b)) => assert!(Arc::ptr_eq(a, b), "same instance after rebuild"),
        _ => panic!(),
    }
    // Removing the plugin from the project drops it from the pool.
    let mut q = p.clone();
    q.tracks[0].effects.clear();
    drop(core2);
    let _core3 = Core::from_project(&q, 48_000.0, &NoSamples, &factory);
    assert!(factory.instance(ids[1]).is_none());
}

#[test]
fn host_saves_states_into_the_project() {
    let mut p = Project::default();
    p.add_track(
        Track::audio("A").with_effect(DeviceSpec::Plugin(PluginRef { params: vec![1.75], ..clap_ref("dev.loopcraft.test-gain", "Test Gain", false, 0) })),
    );
    let mut p = p.normalized();
    let factory = HostFactory::new();
    let core = Core::from_project(&p, 48_000.0, &NoSamples, &factory);
    let (mut e, _h) = create(core);
    let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
    e.process(&mut l, &mut r); // the saved parameter reaches the plugin
    factory.save_states(&mut p);
    let DeviceSpec::Plugin(pr) = &p.tracks[0].effects[0] else { panic!() };
    let raw = b64_decode(pr.state.as_deref().expect("state saved")).unwrap();
    assert_eq!(f64::from_le_bytes(raw[..8].try_into().unwrap()), 1.75);
}

#[test]
fn clap_editor_opens_runs_timers_and_reports_edits() {
    if !have_display() {
        eprintln!("skipped: no X11 display");
        return;
    }
    let factory = HostFactory::new();
    let r = clap_ref("dev.loopcraft.test-gain", "Test Gain", false, 42);
    factory.begin_build();
    let mut dev = factory.create(&r, 48_000.0, 256).expect("created");
    factory.end_build();
    assert!(factory.has_editor(42));
    factory.open_editor(42).expect("editor opens embedded in an X11 window");
    assert!(factory.editor_open(42));
    // The plugin registered a 10 ms timer; the host fires it from idle().
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        factory.idle();
    }
    // Its tick "turns the knob"; the change comes out of the next block.
    process_effect(&mut dev, 256, 0.0);
    assert_eq!(factory.take_param_changes(), vec![(42, 0, 1.5)]);
    factory.close_editor(42);
    assert!(!factory.editor_open(42));
}

// ---------------------------------------------------------------------------
// VST3
// ---------------------------------------------------------------------------

fn vst3_ref(lib: &Vst3Library, instance: u64) -> PluginRef {
    let info = &lib.plugins[0];
    PluginRef {
        format: PluginFormat::Vst3,
        path: vst3_bundle().display().to_string(),
        id: info.id.clone(),
        name: info.name.clone(),
        is_instrument: info.is_instrument,
        state: None,
        params: Vec::new(),
        instance,
    }
}

#[test]
fn vst3_bundle_loads_and_scans() {
    let lib = Vst3Library::load(&vst3_bundle()).expect("bundle loads");
    assert_eq!(lib.plugins.len(), 1, "only the Audio Module Class is listed");
    assert_eq!(lib.plugins[0].name, "Test VST3 Gain");
    assert_eq!(lib.plugins[0].id.len(), 32);
    let (found, errors) = scan_vst3(&[vst3_bundle().parent().unwrap().to_path_buf()]);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(found.len(), 1);
}

#[test]
fn vst3_effect_processes_with_latency_params_and_state() {
    let lib = Arc::new(Vst3Library::load(&vst3_bundle()).unwrap());
    let id = lib.plugins[0].id.clone();
    let s = Vst3Shared::new(lib.clone(), &id, 48_000.0, 256, None).unwrap();
    let params = s.params();
    assert_eq!((params.len(), params[0].name), (1, "Gain"));
    let mut d = Device::Effect(Box::new(Vst3Device::new(s.clone(), &[])));
    let out = process_effect(&mut d, 600, 0.8);
    let Device::Effect(fx) = &mut d else { panic!() };
    assert_eq!(fx.latency(), 32);
    assert!(out[..32].iter().all(|&x| x == 0.0), "delayed by its latency");
    assert!((out[100] - 0.4).abs() < 1e-6, "default gain 0.5: {}", out[100]);
    fx.set_param(0, 1.0);
    let out = process_effect(&mut d, 300, 0.3);
    assert!((out[200] - 0.3).abs() < 1e-6, "gain 1.0: {}", out[200]);
    // State: the processor's gain round-trips through a new instance.
    let state = s.save_state().expect("state");
    let s2 = Vst3Shared::new(lib, &id, 48_000.0, 256, Some(&state)).unwrap();
    let mut d2 = Device::Effect(Box::new(Vst3Device::new(s2, &[])));
    let out = process_effect(&mut d2, 300, 0.3);
    assert!((out[200] - 0.3).abs() < 1e-6, "restored gain 1.0: {}", out[200]);
}

#[test]
fn engine_hosts_vst3() {
    let lib = Vst3Library::load(&vst3_bundle()).unwrap();
    let mut p = Project::default();
    p.add_scene("1");
    let t = p.add_track(Track::midi("Synth", DeviceSpec::PolySynth(Default::default())).with_effect(DeviceSpec::Plugin(vst3_ref(&lib, 0))));
    p.tracks[t].slots[0] = Some(Clip::midi("a", 4.0, vec![Note::new(69, 0.0, 4.0, 127)]));
    let factory = HostFactory::new();
    let core = Core::from_project(&p, 48_000.0, &NoSamples, &factory);
    assert!(factory.errors().is_empty(), "{:?}", factory.errors());
    assert_eq!(core.latency(), 32);
    let (mut e, mut h) = create(core);
    h.send(Command::LaunchClip { track: 0, slot: 0 }).ok();
    let (mut l, mut r) = (vec![0.0; 9600], vec![0.0; 9600]);
    e.process(&mut l, &mut r);
    assert!(peak(&l) > 0.01, "synth through the VST3 gain");
}

#[test]
fn vst3_editor_runs_on_the_host_run_loop_and_reports_edits() {
    if !have_display() {
        eprintln!("skipped: no X11 display");
        return;
    }
    let lib = Vst3Library::load(&vst3_bundle()).unwrap();
    let factory = HostFactory::new();
    factory.begin_build();
    let mut dev = factory.create(&vst3_ref(&lib, 7), 48_000.0, 256).expect("created");
    factory.end_build();
    factory.open_editor(7).expect("editor opens");
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        factory.idle();
    }
    // The editor's timer performed an edit: the host records it for the
    // project and passes it to the processor.
    assert_eq!(factory.take_param_changes(), vec![(7, 0, 0.75)]);
    let _ = process_effect(&mut dev, 256, 0.0);
    let out = process_effect(&mut dev, 256, 0.4);
    assert!((out[100] - 0.3).abs() < 1e-6, "processor got 0.75: {}", out[100]);
    factory.close_editor(7);
    assert!(!factory.editor_open(7));
}
