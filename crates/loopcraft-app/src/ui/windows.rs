//! Floating windows: Settings (audio / MIDI / library), Groove Pool, Mappings.

use super::theme;
use crate::app::App;
use eframe::egui::{self, RichText};
use loopcraft_core::*;

pub fn show(app: &mut App, ctx: &egui::Context) {
    settings(app, ctx);
    grooves(app, ctx);
    mappings(app, ctx);
}

fn settings(app: &mut App, ctx: &egui::Context) {
    if !app.show_settings {
        return;
    }
    let mut open = true;
    let mut apply_audio = false;
    let mut reopen_midi = false;
    egui::Window::new("Settings").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
        ui.heading("Audio");
        let devices = app.device_list.clone().unwrap_or_default();
        let a = &mut app.audio_edit;
        egui::Grid::new("audiogrid").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            ui.label("Output");
            egui::ComboBox::from_id_salt("outdev").width(260.0).selected_text(a.output.clone().unwrap_or_else(|| "System default".into())).show_ui(ui, |ui| {
                ui.selectable_value(&mut a.output, None, "System default");
                for d in &devices.0 {
                    ui.selectable_value(&mut a.output, Some(d.clone()), d);
                }
            });
            ui.end_row();
            ui.label("Input");
            ui.horizontal(|ui| {
                ui.checkbox(&mut a.input_enabled, "");
                ui.add_enabled_ui(a.input_enabled, |ui| {
                    egui::ComboBox::from_id_salt("indev").width(236.0).selected_text(a.input.clone().unwrap_or_else(|| "System default".into())).show_ui(ui, |ui| {
                        ui.selectable_value(&mut a.input, None, "System default");
                        for d in &devices.1 {
                            ui.selectable_value(&mut a.input, Some(d.clone()), d);
                        }
                    });
                });
            });
            ui.end_row();
            ui.label("Sample rate");
            egui::ComboBox::from_id_salt("sr").selected_text(a.sample_rate.map_or("Device default".to_string(), |r| format!("{r} Hz"))).show_ui(ui, |ui| {
                ui.selectable_value(&mut a.sample_rate, None, "Device default");
                for r in [44_100u32, 48_000, 88_200, 96_000] {
                    ui.selectable_value(&mut a.sample_rate, Some(r), format!("{r} Hz"));
                }
            });
            ui.end_row();
            ui.label("Buffer");
            egui::ComboBox::from_id_salt("buf").selected_text(a.buffer.map_or("Device default".to_string(), |b| format!("{b} samples"))).show_ui(ui, |ui| {
                ui.selectable_value(&mut a.buffer, None, "Device default");
                for b in [64u32, 128, 256, 512, 1024, 2048] {
                    ui.selectable_value(&mut a.buffer, Some(b), format!("{b} samples"));
                }
            });
            ui.end_row();
        });
        let latency_ms = app.device.buffer.map(|b| b as f32 / app.sample_rate * 1000.0);
        ui.label(
            RichText::new(format!(
                "Running: {} · {:.0} Hz · {} out / {} in{}",
                app.device_label,
                app.sample_rate,
                app.device.out_channels,
                app.device.in_channels,
                latency_ms.map(|l| format!(" · buffer {l:.1} ms")).unwrap_or_default()
            ))
            .size(10.0)
            .color(theme::DIM),
        );
        ui.horizontal(|ui| {
            if ui.button("Apply audio settings").clicked() {
                apply_audio = true;
            }
            if ui.button("Rescan devices").clicked() {
                let d = crate::audio::list_devices();
                app.device_list = Some((d.outputs, d.inputs));
            }
        });
        ui.separator();
        ui.heading("MIDI inputs");
        if let Some(e) = &app.midi.error {
            ui.label(RichText::new(e).size(10.0).color(theme::REC));
        }
        if app.midi.available.is_empty() {
            ui.label(RichText::new("No MIDI input ports found").color(theme::DIM));
        }
        let all = app.settings.midi_inputs.is_empty();
        let mut all2 = all;
        if ui.checkbox(&mut all2, "Listen to all ports").changed() {
            app.settings.midi_inputs = if all2 { Vec::new() } else { app.midi.ports.clone() };
            reopen_midi = true;
        }
        if !all2 {
            for p in app.midi.available.clone() {
                let mut on = app.settings.midi_inputs.contains(&p);
                if ui.checkbox(&mut on, &p).changed() {
                    if on {
                        app.settings.midi_inputs.push(p);
                    } else {
                        app.settings.midi_inputs.retain(|x| x != &p);
                    }
                    reopen_midi = true;
                }
            }
        }
        if ui.button("Rescan MIDI").clicked() {
            reopen_midi = true;
        }
        ui.separator();
        ui.heading("Library");
        let mut lib = app.settings.library().display().to_string();
        ui.horizontal(|ui| {
            ui.label("Folder");
            if ui.add(egui::TextEdit::singleline(&mut lib).desired_width(280.0)).lost_focus() {
                app.settings.library_dir = Some(std::path::PathBuf::from(lib.trim()));
                app.settings.save();
            }
        });
        ui.label(RichText::new("New projects, presets and recordings of unsaved projects go here").size(10.0).color(theme::DIM));
    });
    if apply_audio {
        app.settings.audio = app.audio_edit.clone();
        app.restart_audio();
    }
    if reopen_midi {
        app.reopen_midi();
    }
    if !open {
        app.show_settings = false;
    }
}

fn grooves(app: &mut App, ctx: &egui::Context) {
    if !app.show_grooves {
        return;
    }
    let mut open = true;
    egui::Window::new("Groove Pool").open(&mut open).resizable(true).default_width(380.0).show(ctx, |ui| {
        ui.label(RichText::new("Clips pick a groove in their Clip properties. Timing is in fractions of a 16th; velocity is a relative change.").size(10.0).color(theme::DIM));
        let mut remove = None;
        let mut changed = false;
        for (i, g) in app.project.grooves.iter_mut().enumerate() {
            ui.separator();
            ui.horizontal(|ui| {
                changed |= ui.add(egui::TextEdit::singleline(&mut g.name).desired_width(140.0)).changed();
                ui.label("Amount");
                changed |= ui.add(egui::Slider::new(&mut g.amount, 0.0..=1.3).max_decimals(2)).changed();
                if ui.small_button("x").clicked() {
                    remove = Some(i);
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("timing").size(10.0).color(theme::DIM));
                for t in g.timing.iter_mut() {
                    changed |= ui.add(egui::DragValue::new(t).range(-1.0..=1.0).speed(0.01).max_decimals(2)).changed();
                }
                if ui.small_button("+").clicked() {
                    g.timing.push(0.0);
                    g.velocity.push(0.0);
                    changed = true;
                }
                if g.timing.len() > 1 && ui.small_button("−").clicked() {
                    g.timing.pop();
                    g.velocity.pop();
                    changed = true;
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("velocity").size(10.0).color(theme::DIM));
                for v in g.velocity.iter_mut() {
                    changed |= ui.add(egui::DragValue::new(v).range(-1.0..=1.0).speed(0.01).max_decimals(2)).changed();
                }
            });
        }
        if changed {
            app.dirty = true;
            app.rebuild_pending = true;
        }
        if let Some(i) = remove {
            app.delete_groove(i);
        }
        ui.separator();
        ui.horizontal(|ui| {
            for (name, r) in [("Swing 54%", 0.54), ("Swing 58%", 0.58), ("Swing 62%", 0.62), ("Swing 66%", 0.66)] {
                if ui.small_button(format!("+ {name}")).clicked() {
                    app.add_groove(Groove::swing(&format!("Swing 16ths {}", &name[6..]), r));
                    app.rebuild();
                }
            }
        });
    });
    if !open {
        app.show_grooves = false;
    }
}

fn mappings(app: &mut App, ctx: &egui::Context) {
    if !(app.show_mappings || app.map_mode.is_some()) {
        return;
    }
    let mut open = true;
    egui::Window::new("Mappings").open(&mut open).resizable(true).default_width(420.0).default_pos([900.0, 120.0]).show(ctx, |ui| {
        if app.map_mode.is_some() {
            let msg = match app.map_target {
                Some(t) => format!("Waiting for input → {}", app.mapping_label(&t)),
                None => "Click a highlighted control".into(),
            };
            ui.label(RichText::new(msg).color(theme::ACCENT));
        }
        if app.project.mappings.is_empty() {
            ui.label(RichText::new("No mappings yet. Turn on MIDI or KEY map mode in the toolbar.").color(theme::DIM));
        }
        let mut remove = None;
        let maps = app.project.mappings.clone();
        egui::Grid::new("mapgrid").num_columns(4).striped(true).show(ui, |ui| {
            for (i, m) in maps.iter().enumerate() {
                let src = match m.source {
                    MappingSource::Cc { channel, cc } => format!("Ch{} CC{cc}", channel + 1),
                    MappingSource::Note { channel, note } => format!("Ch{} Note {note}", channel + 1),
                    MappingSource::Key(c) => format!("Key {}", c.to_uppercase()),
                };
                ui.label(src);
                ui.label(app.mapping_label(&m.target));
                if crate::app::is_button(&m.target) {
                    ui.label("");
                } else {
                    let (mut lo, mut hi) = (m.min, m.max);
                    ui.horizontal(|ui| {
                        let a = ui.add(egui::DragValue::new(&mut lo).speed(0.05).prefix("min "));
                        let b = ui.add(egui::DragValue::new(&mut hi).speed(0.05).prefix("max "));
                        if a.changed() || b.changed() {
                            app.project.mappings[i].min = lo;
                            app.project.mappings[i].max = hi;
                            app.dirty = true;
                        }
                    });
                }
                if ui.small_button("x").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
        if let Some(i) = remove {
            app.remove_mapping(i);
        }
    });
    if !open {
        app.show_mappings = false;
        app.map_mode = None;
        app.map_target = None;
    }
}
