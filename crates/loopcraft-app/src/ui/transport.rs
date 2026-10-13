use super::{Icon, format_position, icon_button, mappable, small_toggle, theme, toggle_map};
use crate::app::{App, DialogKind, MapMode, View};
use eframe::egui::{self, RichText};
use loopcraft_core::{MappingTarget, Quantization};
use loopcraft_engine::Command;
use std::sync::atomic::Ordering::Relaxed;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let st = app.engine.state.clone();
    let playing = st.playing.load(Relaxed);
    ui.horizontal(|ui| {
        ui.label(RichText::new("LoopCraft").strong().color(theme::ACCENT));
        ui.menu_button("File", |ui| {
            if ui.button("New").clicked() {
                app.new_project();
                ui.close();
            }
            if ui.button("Open…").clicked() {
                app.open_dialog(DialogKind::Open);
                ui.close();
            }
            if ui.button("Save").clicked() {
                app.save();
                ui.close();
            }
            if ui.button("Save As…").clicked() {
                app.open_dialog(DialogKind::SaveAs);
                ui.close();
            }
            if ui.button("Collect All and Save").on_hover_text("Copy samples from outside the project folder into it").clicked() {
                app.collect_all_and_save();
                ui.close();
            }
            ui.separator();
            if ui.button("Export Audio…").clicked() {
                app.open_dialog(DialogKind::Export);
                ui.close();
            }
            ui.separator();
            if ui.button("Settings…").clicked() {
                app.open_settings();
                ui.close();
            }
        });
        ui.menu_button("Edit", |ui| {
            if ui.button("Undo").clicked() {
                app.undo();
                ui.close();
            }
            if ui.button("Redo").clicked() {
                app.redo();
                ui.close();
            }
            ui.separator();
            if ui.button("Group selected tracks  (Ctrl+G)").clicked() {
                app.group_selected();
                ui.close();
            }
            if ui.button("Groove Pool…").clicked() {
                app.show_grooves = true;
                ui.close();
            }
            if ui.button("Mappings…").clicked() {
                app.show_mappings = true;
                ui.close();
            }
        });
        ui.separator();

        // Tempo
        let mut tempo = st.tempo.load();
        let follows = st.tempo_follows_arrangement.load(Relaxed);
        let r = ui.add_enabled(!follows, egui::DragValue::new(&mut tempo).range(20.0..=999.0).speed(0.1).fixed_decimals(2).suffix(" bpm"));
        if r.changed() {
            app.project.tempo = tempo;
            app.dirty = true;
            app.send(Command::SetTempo(tempo));
        }
        let r = r.on_hover_text(if follows { "Following the Arrangement's tempo automation" } else { "Song tempo" });
        mappable(app, ui, r.rect, MappingTarget::Tempo);
        if ui.small_button("Tap").on_hover_text("Tap tempo").clicked() {
            app.tap_tempo();
        }
        let mut num = app.project.time_signature.numerator;
        let mut den = app.project.time_signature.denominator;
        let a = ui.add(egui::DragValue::new(&mut num).range(1..=32));
        ui.label("/");
        let b = ui.add(egui::DragValue::new(&mut den).range(1..=32));
        if (a.changed() || b.changed()) && [1, 2, 4, 8, 16, 32].contains(&den) {
            app.checkpoint();
            app.project.time_signature.numerator = num;
            app.project.time_signature.denominator = den;
            app.rebuild();
        }
        if small_toggle(ui, app.project.metronome, "Metro", theme::ACCENT).clicked() {
            app.project.metronome = !app.project.metronome;
            app.send(Command::SetMetronome(app.project.metronome));
        }
        let ci = app.project.count_in_bars;
        egui::ComboBox::from_id_salt("countin").width(40.0).selected_text(if ci == 0 { "Count".into() } else { format!("{ci} bar") }).show_ui(ui, |ui| {
            for n in [0u32, 1, 2, 4] {
                if ui.selectable_label(ci == n, if n == 0 { "No count-in".to_string() } else { format!("Count-in {n} bar") }).clicked() {
                    app.project.count_in_bars = n;
                    app.settings.count_in_bars = n;
                    app.settings.save();
                    app.dirty = true;
                }
            }
        });
        ui.separator();

        // Transport
        let isz = egui::vec2(26.0, 20.0);
        let r = icon_button(ui, Icon::Play, playing, theme::PLAY, isz).on_hover_text("Play (Space)");
        if r.clicked() {
            app.play();
        }
        mappable(app, ui, r.rect, MappingTarget::Play);
        let r = icon_button(ui, Icon::Stop, false, theme::ACCENT, isz).on_hover_text("Stop; press again to return to the start");
        if r.clicked() {
            if playing {
                app.send(Command::Stop);
            } else {
                app.send(Command::Seek(0.0));
            }
        }
        mappable(app, ui, r.rect, MappingTarget::Stop);
        let arr_rec = app.arrangement_recording;
        if icon_button(ui, Icon::Record, arr_rec, theme::REC, isz).on_hover_text("Arrangement Record (F9): record armed tracks onto the timeline").clicked() {
            app.toggle_arrangement_record();
        }
        let sess_rec = app.engine.state.tracks.iter().any(|t| t.recording_slot.load(Relaxed) >= 0);
        if small_toggle(ui, sess_rec, "Sess Rec", theme::REC)
            .on_hover_text("Session Record: record armed tracks into the selected scene; overdub playing MIDI clips")
            .clicked()
        {
            app.session_record();
        }
        if ui.small_button("Capture").on_hover_text("Capture MIDI: turn what you just played into a clip, without having pressed Record").clicked() {
            app.capture_midi();
        }
        let count = st.count_in_left.load();
        let pos = if count > 0.0 { format!("-{:.0}", count.ceil()) } else { format_position(st.beat.load(), app.project.time_signature.beats_per_bar()) };
        ui.label(RichText::new(pos).monospace().size(15.0).color(if count > 0.0 { theme::REC } else { theme::TEXT }));

        let mut q = app.project.quantization;
        egui::ComboBox::from_id_salt("quant").width(56.0).selected_text(q.label()).show_ui(ui, |ui| {
            for opt in Quantization::ALL {
                ui.selectable_value(&mut q, opt, opt.label());
            }
        });
        if q != app.project.quantization {
            app.project.quantization = q;
            app.dirty = true;
            app.send(Command::SetQuantization(q));
        }
        let looping = app.project.loop_region.is_some();
        if small_toggle(ui, looping, "Loop", theme::ACCENT).on_hover_text("Loop the Arrangement (drag on the ruler to set it)").clicked() {
            let bar = app.project.time_signature.beats_per_bar();
            app.project.loop_region = if looping { None } else { Some(app.last_loop.unwrap_or((0.0, 4.0 * bar))) };
            if let Some(l) = app.project.loop_region {
                app.last_loop = Some(l);
            }
            app.send(Command::SetLoop(app.project.loop_region));
        }
        if ui.small_button("◀◀").on_hover_text("Previous locator").clicked() {
            app.jump_locator(false);
        }
        if ui.small_button("▶▶").on_hover_text("Next locator").clicked() {
            app.jump_locator(true);
        }

        let off_arrangement = st.tracks.iter().any(|t| !t.follows_arrangement.load(Relaxed));
        if ui
            .add_enabled(off_arrangement, egui::Button::new(RichText::new("Back to Arr.").color(if off_arrangement { theme::ACCENT } else { theme::DIM })))
            .on_hover_text("Return all tracks to the Arrangement")
            .clicked()
        {
            app.send(Command::BackToArrangement);
        }
        ui.separator();
        if ui.selectable_label(app.view == View::Session, "Session").clicked() {
            app.view = View::Session;
        }
        if ui.selectable_label(app.view == View::Arrangement, "Arrangement").on_hover_text("Tab switches views").clicked() {
            app.view = View::Arrangement;
        }
        ui.separator();
        if small_toggle(ui, app.map_mode == Some(MapMode::Midi), "MIDI", theme::MIDI_MAP).on_hover_text("MIDI Map mode (Ctrl+M)").clicked() {
            toggle_map(app, MapMode::Midi);
        }
        if small_toggle(ui, app.map_mode == Some(MapMode::Key), "KEY", theme::KEY_MAP).on_hover_text("Key Map mode (Ctrl+K)").clicked() {
            toggle_map(app, MapMode::Key);
        }
        let kb = small_toggle(ui, app.computer_keys, "Keys", theme::ACCENT)
            .on_hover_text("Play armed (or the selected) MIDI tracks with A–K; Z/X octave, C/V velocity");
        if kb.clicked() {
            app.computer_keys = !app.computer_keys;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("CPU {:>3.0}%", st.cpu_load.load() * 100.0)).monospace().color(theme::DIM));
            let active = app.midi.last_event.as_ref().is_some_and(|(t, _)| t.elapsed().as_millis() < 150);
            let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 3.5, if active { theme::PLAY } else { theme::SLOT });
            ui.label(RichText::new("MIDI").size(10.0).color(theme::DIM));
        });
    });
}

pub fn status_bar(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let msg = if app.status.is_empty() { "Ready".to_string() } else { app.status.clone() };
        ui.label(RichText::new(msg).size(11.0).color(if app.status.is_empty() { theme::DIM } else { theme::ACCENT }));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let lat = app.engine.state.latency.load(Relaxed);
            if app.computer_keys {
                ui.label(RichText::new(format!("Keys: C{} vel {}", 3 + app.octave, app.key_velocity)).size(10.0).color(theme::DIM));
                ui.separator();
            }
            let ports = if app.midi.ports.is_empty() { "no MIDI input".to_string() } else { app.midi.ports.join(", ") };
            ui.label(RichText::new(format!("{} · {:.0} Hz · PDC {lat} smp · {ports}", app.device_label, app.sample_rate)).size(10.0).color(theme::DIM));
        });
    });
}
