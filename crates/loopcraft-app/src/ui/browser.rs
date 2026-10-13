//! Browser: instruments, effects, presets, plugins and samples.

use super::theme;
use crate::app::App;
use eframe::egui::{self, RichText};
use loopcraft_core::*;
use loopcraft_engine::TrackRef;
use loopcraft_plugin::{default_clap_paths, scan_clap};

fn item(ui: &mut egui::Ui, label: &str, hint: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(label).size(12.0)).frame(false)).on_hover_text(hint)
}

fn device_hint(spec: &DeviceSpec) -> &'static str {
    use DeviceSpec::*;
    match spec {
        PolySynth(_) => "Two-oscillator subtractive synth",
        Wavetable(_) => "Two wavetable oscillators with position morphing, unison and modulation",
        FmSynth(_) => "Four-operator FM synth with 8 algorithms",
        Sampler(_) => "Plays one sample across the keyboard",
        MultiSampler(_) => "Key/velocity-mapped sample zones",
        DrumRack(_) => "Pads of synthesized drums or samples",
        InstrumentRack(_) => "Parallel instrument chains with key/velocity zones and macros",
        AudioRack(_) => "Parallel effect chains with macros",
        Arpeggiator(_) => "Plays held notes as patterns",
        Chord(_) => "Adds intervals to each note",
        Scale(_) => "Snaps notes to a scale",
        RandomNote(_) => "Randomizes pitches",
        NoteLength(_) => "Fixes note lengths",
        Velocity(_) => "Shapes velocities",
        Pitch(_) => "Transposes notes",
        Eq(_) => "Parametric EQ",
        Compressor(_) => "Compressor with sidechain",
        GlueCompressor(_) => "Bus compressor",
        Limiter(_) => "Lookahead brickwall limiter",
        Gate(_) => "Noise gate",
        MultibandDynamics(_) => "Three-band compressor / expander",
        Delay(_) => "Stereo delay with ping-pong and sync",
        Reverb(_) => "Algorithmic reverb",
        ConvolutionReverb(_) => "Impulse-response reverb",
        Saturator(_) => "Waveshaping distortion",
        DrumBuss(_) => "Drum bus: drive, transients, boom",
        Amp(_) => "Guitar amp and cabinet",
        Redux(_) => "Bit and sample-rate reduction",
        Chorus(_) => "Chorus / ensemble",
        Flanger(_) => "Flanger",
        Phaser(_) => "Phaser",
        AutoPan(_) => "LFO panning and tremolo",
        Utility(_) => "Gain, width, phase, mono",
        AutoFilter(_) => "Filter with LFO and envelope follower",
        BeatRepeat(_) => "Rhythmic stutter / repeat",
        GrainDelay(_) => "Granular pitch-shifting delay",
        Vocoder(_) => "Vocoder with internal or sidechain carrier",
        Plugin(_) => "CLAP plugin",
    }
}

/// Put a device where the user most likely wants it.
pub fn place(app: &mut App, spec: DeviceSpec) {
    if let Some((ch, dev, chain)) = app.rack_insert {
        app.add_to_rack_chain(ch, dev, &[], chain, spec);
        return;
    }
    let midi_sel = app.selected_track.filter(|&t| app.project.tracks.get(t).is_some_and(|t| t.kind == TrackKind::Midi));
    if spec.is_instrument() {
        match midi_sel {
            Some(t) => app.add_device(TrackRef::Track(t), spec),
            None => {
                app.add_midi_track(spec);
            }
        }
    } else if spec.is_midi_effect() {
        match midi_sel {
            Some(t) => app.add_device(TrackRef::Track(t), spec),
            None => app.status = "Select a MIDI track for MIDI effects".into(),
        }
    } else {
        let target = app.device_target;
        app.add_device(target, spec);
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.add(egui::TextEdit::singleline(&mut app.browser_search).hint_text("Search…").desired_width(f32::INFINITY));
    if let Some((ch, _, chain)) = app.rack_insert {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Adding into {} chain {}", app.track_ref_name(ch), chain + 1)).size(10.0).color(theme::ACCENT));
            if ui.small_button("Done").clicked() {
                app.rack_insert = None;
            }
        });
    }
    let q = app.browser_search.to_lowercase();
    let matches = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
    egui::ScrollArea::vertical().show(ui, |ui| {
        if q.is_empty() {
            egui::CollapsingHeader::new("Tracks").default_open(true).show(ui, |ui| {
                if item(ui, "+ MIDI Track", "New MIDI track with a Poly Synth (Ctrl+Shift+T)").clicked() {
                    app.add_midi_track(DeviceSpec::PolySynth(Default::default()));
                }
                if item(ui, "+ Audio Track", "New audio track (Ctrl+T)").clicked() {
                    app.add_audio_track();
                }
                if item(ui, "+ Return Track", "New return track with a reverb").clicked() {
                    app.add_return();
                }
                if item(ui, "+ Scene", "Add a scene row").clicked() {
                    app.add_scene();
                }
            });
        }

        let catalog = DeviceSpec::catalog();
        for (title, kind) in [("Instruments", DeviceKind::Instrument), ("MIDI Effects", DeviceKind::MidiEffect), ("Audio Effects", DeviceKind::AudioEffect)] {
            let list: Vec<&DeviceSpec> = catalog.iter().filter(|d| d.kind() == kind && matches(d.name())).collect();
            if list.is_empty() {
                continue;
            }
            egui::CollapsingHeader::new(title).default_open(!q.is_empty() || kind != DeviceKind::AudioEffect).id_salt(title).show(ui, |ui| {
                for spec in list {
                    let r = item(ui, spec.name(), device_hint(spec));
                    if r.clicked() {
                        place(app, spec.clone());
                    }
                    r.context_menu(|ui| {
                        if spec.is_instrument() && ui.button("New MIDI track with this").clicked() {
                            app.add_midi_track(spec.clone());
                            ui.close();
                        }
                        if !spec.is_instrument() && !spec.is_midi_effect() {
                            if ui.button("Add to Master").clicked() {
                                app.add_device(TrackRef::Master, spec.clone());
                                ui.close();
                            }
                            for r in 0..app.project.returns.len() {
                                if ui.button(format!("Add to {}", app.project.returns[r].name)).clicked() {
                                    app.add_device(TrackRef::Return(r), spec.clone());
                                    ui.close();
                                }
                            }
                        }
                    });
                }
            });
        }

        let presets: Vec<_> = app.presets().into_iter().filter(|(c, n, _)| matches(c) || matches(n)).collect();
        egui::CollapsingHeader::new(format!("Presets ({})", presets.len())).id_salt("presets").show(ui, |ui| {
            if presets.is_empty() {
                ui.label(RichText::new("Save one from a device's ... menu").size(10.0).color(theme::DIM));
            }
            let mut last = String::new();
            for (cat, name, path) in presets {
                if cat != last {
                    ui.label(RichText::new(&cat).size(10.0).color(theme::DIM));
                    last = cat.clone();
                }
                if item(ui, &format!("  {name}"), &path.display().to_string()).clicked()
                    && let Some(spec) = app.load_preset(&path)
                {
                    place(app, spec);
                }
            }
        });

        egui::CollapsingHeader::new("Plugins (CLAP)").id_salt("plugins").show(ui, |ui| {
            if ui.small_button("Scan plugin folders").clicked() {
                let (found, errors) = scan_clap(&default_clap_paths());
                app.plugin_list = found.into_iter().flat_map(|(path, ps)| ps.into_iter().map(move |p| (path.clone(), p))).collect();
                app.status = format!(
                    "Found {} CLAP plugin(s){}",
                    app.plugin_list.len(),
                    if errors.is_empty() { String::new() } else { format!(", {} failed", errors.len()) }
                );
            }
            if app.plugin_list.is_empty() {
                ui.label(RichText::new("Searches ~/.clap, /usr/lib/clap and $CLAP_PATH").size(10.0).color(theme::DIM));
            }
            let list = app.plugin_list.clone();
            for (path, info) in list {
                if !matches(&info.name) {
                    continue;
                }
                let label = format!("{}{}", info.name, if info.is_instrument { "  (inst)" } else { "" });
                if item(ui, &label, &format!("{} — {}", info.vendor, info.id)).clicked() {
                    let spec = DeviceSpec::Plugin(PluginRef {
                        format: PluginFormat::Clap,
                        path: path.display().to_string(),
                        id: info.id.clone(),
                        name: info.name.clone(),
                        is_instrument: info.is_instrument,
                        state: None,
                        params: Vec::new(),
                    });
                    place(app, spec);
                }
            }
        });

        egui::CollapsingHeader::new("Samples").default_open(true).id_salt("samples").show(ui, |ui| {
            let files: Vec<String> = app.sample_files().into_iter().filter(|f| matches(f)).collect();
            if files.is_empty() {
                ui.label(RichText::new(format!("No audio files in {} or its Samples folder", app.default_dir().display())).size(10.0).color(theme::DIM));
            }
            for f in files {
                let short = std::path::Path::new(&f).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| f.clone());
                let r = item(ui, &short, &format!("{f}\nClick: add to the selected audio track's next free slot. Right-click for more."));
                if r.clicked() {
                    app.add_sample_clip(&f);
                }
                r.context_menu(|ui| {
                    if let Some(t) = app.selected_track.filter(|&t| app.project.tracks.get(t).is_some_and(|t| t.kind == TrackKind::Audio))
                        && ui.button("Add to Arrangement at playhead").clicked()
                    {
                        let bar = app.project.time_signature.beats_per_bar();
                        let at = (app.engine.state.beat.load() / bar).floor() * bar;
                        app.add_sample_to_arrangement(t, at, &f);
                        ui.close();
                    }
                    if let Some(t) = app.selected_track.filter(|&t| app.project.tracks.get(t).is_some_and(|t| t.kind == TrackKind::Midi))
                        && ui.button("Load into a Sampler on this track").clicked()
                    {
                        app.add_device(TrackRef::Track(t), DeviceSpec::Sampler(SamplerParams { file: f.clone(), ..Default::default() }));
                        ui.close();
                    }
                    if ui.button("New audio track with this").clicked() {
                        let t = app.add_audio_track();
                        app.add_sample_to(t, Some(0), &f);
                        ui.close();
                    }
                });
            }
        });
    });
}
