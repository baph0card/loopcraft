//! Browser: instruments, effects, plugins and samples to drop onto tracks.

use super::theme;
use crate::app::App;
use eframe::egui::{self, RichText};
use loopcraft_core::*;
use loopcraft_plugin::{default_clap_paths, scan_clap};

fn item(ui: &mut egui::Ui, label: &str, hint: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(label).size(12.0)).frame(false)).on_hover_text(hint).clicked()
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(4.0);
        egui::CollapsingHeader::new("Tracks").default_open(true).show(ui, |ui| {
            if item(ui, "+ MIDI Track", "New MIDI track with a Poly Synth") {
                app.add_midi_track(DeviceSpec::PolySynth(Default::default()));
            }
            if item(ui, "+ Audio Track", "New audio track") {
                app.add_audio_track();
            }
            if item(ui, "+ Return Track", "New return track with a reverb") {
                app.add_return();
            }
            if item(ui, "+ Scene", "Add a scene row") {
                app.add_scene();
            }
        });

        egui::CollapsingHeader::new("Instruments").default_open(true).show(ui, |ui| {
            let target = app.selected_track.filter(|&t| app.project.tracks.get(t).is_some_and(|t| t.kind == TrackKind::Midi));
            let hint = if target.is_some() { "Replace the selected track's instrument" } else { "Create a new MIDI track" };
            let mut chosen = None;
            if item(ui, "Poly Synth", hint) {
                chosen = Some(DeviceSpec::PolySynth(Default::default()));
            }
            if item(ui, "Drum Rack", hint) {
                chosen = Some(DeviceSpec::DrumRack(Default::default()));
            }
            if item(ui, "Sampler", "Plays a WAV across the keyboard; set its file in the device view") {
                chosen = Some(DeviceSpec::Sampler(Default::default()));
            }
            if let Some(spec) = chosen {
                match target {
                    Some(t) => app.add_device(loopcraft_engine::TrackRef::Track(t), spec),
                    None => {
                        app.add_midi_track(spec);
                    }
                }
            }
        });

        egui::CollapsingHeader::new("Audio Effects").default_open(true).show(ui, |ui| {
            let effects: [(&str, DeviceSpec); 7] = [
                ("EQ", DeviceSpec::Eq(Default::default())),
                ("Compressor", DeviceSpec::Compressor(Default::default())),
                ("Delay", DeviceSpec::Delay(Default::default())),
                ("Reverb", DeviceSpec::Reverb(Default::default())),
                ("Saturator", DeviceSpec::Saturator(Default::default())),
                ("Auto Filter", DeviceSpec::AutoFilter(Default::default())),
                ("Utility", DeviceSpec::Utility(Default::default())),
            ];
            for (name, spec) in effects {
                if item(ui, name, "Add to the end of the selected chain") {
                    let target = app.device_target;
                    app.add_device(target, spec);
                }
            }
        });

        egui::CollapsingHeader::new("Plugins (CLAP)").show(ui, |ui| {
            if ui.small_button("Scan plugin folders").clicked() {
                let (found, errors) = scan_clap(&default_clap_paths());
                app.plugin_list = found.into_iter().flat_map(|(path, ps)| ps.into_iter().map(move |p| (path.clone(), p))).collect();
                app.status = format!("Found {} CLAP plugin(s){}", app.plugin_list.len(), if errors.is_empty() { String::new() } else { format!(", {} failed", errors.len()) });
            }
            if app.plugin_list.is_empty() {
                ui.label(RichText::new("Searches ~/.clap, /usr/lib/clap and $CLAP_PATH").size(10.0).color(theme::DIM));
            }
            let list = app.plugin_list.clone();
            for (path, info) in list {
                let label = format!("{}{}", info.name, if info.is_instrument { "  (inst)" } else { "" });
                if item(ui, &label, &format!("{} — {}", info.vendor, info.id)) {
                    let spec = DeviceSpec::Plugin(PluginRef {
                        format: PluginFormat::Clap,
                        path: path.display().to_string(),
                        id: info.id.clone(),
                        name: info.name.clone(),
                        is_instrument: info.is_instrument,
                        state: None,
                    });
                    if info.is_instrument && app.selected_track.is_none() {
                        app.add_midi_track(spec);
                    } else {
                        let target = app.device_target;
                        app.add_device(target, spec);
                    }
                }
            }
        });

        egui::CollapsingHeader::new("Samples").default_open(true).show(ui, |ui| {
            let files = app.sample_files();
            if files.is_empty() {
                ui.label(RichText::new(format!("No .wav files in {}", app.default_dir().display())).size(10.0).color(theme::DIM));
            }
            for f in files {
                if item(ui, &f, "Add as a warped clip on the selected audio track") {
                    app.add_sample_clip(&f);
                }
            }
        });
    });
}
