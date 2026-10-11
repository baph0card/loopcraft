use super::{Icon, format_position, icon_button, small_toggle, theme};
use crate::app::{App, DialogKind, View};
use eframe::egui::{self, RichText};
use loopcraft_core::Quantization;
use loopcraft_engine::Command;
use std::sync::atomic::Ordering::Relaxed;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let st = app.engine.state.clone();
    let playing = st.playing.load(Relaxed);
    ui.horizontal(|ui| {
        ui.label(RichText::new("LoopCraft").strong().color(theme::ACCENT));
        ui.separator();
        if ui.button("New").clicked() {
            app.new_project();
        }
        if ui.button("Open…").clicked() {
            app.open_dialog(DialogKind::Open);
        }
        if ui.button("Save").clicked() {
            app.save();
        }
        if ui.button("Save As…").clicked() {
            app.open_dialog(DialogKind::SaveAs);
        }
        if ui.button("Export…").on_hover_text("Render to a WAV file").clicked() {
            app.open_dialog(DialogKind::Export);
        }
        ui.separator();

        // Transport
        let isz = egui::vec2(26.0, 20.0);
        if icon_button(ui, Icon::Play, playing, theme::PLAY, isz).on_hover_text("Play (Space)").clicked() {
            app.send(Command::Play);
        }
        if icon_button(ui, Icon::Stop, false, theme::ACCENT, isz).on_hover_text("Stop; press again to return to the start").clicked() {
            if playing {
                app.send(Command::Stop);
            } else {
                app.send(Command::Seek(0.0));
            }
        }
        let recording = st.tracks.iter().any(|t| t.recording_slot.load(Relaxed) >= 0);
        if icon_button(ui, Icon::Record, recording, theme::REC, isz).on_hover_text("Record 4 bars of MIDI into the selected track").clicked() {
            app.record_midi();
        }

        let mut tempo = st.tempo.load();
        ui.label(RichText::new("BPM").color(theme::DIM));
        if ui.add(egui::DragValue::new(&mut tempo).range(20.0..=999.0).speed(0.1).fixed_decimals(2)).changed() {
            app.project.tempo = tempo;
            app.dirty = true;
            app.send(Command::SetTempo(tempo));
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

        let mut q = app.project.quantization;
        egui::ComboBox::from_id_salt("quant").width(64.0).selected_text(q.label()).show_ui(ui, |ui| {
            for opt in Quantization::ALL {
                ui.selectable_value(&mut q, opt, opt.label());
            }
        });
        if q != app.project.quantization {
            app.project.quantization = q;
            app.dirty = true;
            app.send(Command::SetQuantization(q));
        }

        if small_toggle(ui, app.project.metronome, "Metro", theme::ACCENT).clicked() {
            app.project.metronome = !app.project.metronome;
            app.send(Command::SetMetronome(app.project.metronome));
        }
        let looping = app.project.loop_region.is_some();
        if small_toggle(ui, looping, "Loop", theme::ACCENT).on_hover_text("Loop the Arrangement (right-click the ruler to place it)").clicked() {
            let bar = app.project.time_signature.beats_per_bar();
            app.project.loop_region = if looping { None } else { Some((0.0, 4.0 * bar)) };
            app.send(Command::SetLoop(app.project.loop_region));
        }

        let pos = format_position(st.beat.load(), app.project.time_signature.beats_per_bar());
        ui.label(RichText::new(pos).monospace().size(15.0).color(theme::TEXT));

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
        let kb = small_toggle(ui, app.computer_keys, "Keys", theme::ACCENT).on_hover_text("Play the selected MIDI track with A–K; Z/X change octave");
        if kb.clicked() {
            app.computer_keys = !app.computer_keys;
        }
        if app.computer_keys {
            ui.label(RichText::new(format!("C{}", 3 + app.octave)).color(theme::DIM));
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("CPU {:>3.0}%", st.cpu_load.load() * 100.0)).monospace().color(theme::DIM));
            ui.label(RichText::new(&app.device_label).color(theme::DIM).size(11.0));
        });
    });
}
