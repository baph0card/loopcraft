//! The user interface. Layout follows the familiar clip-launcher layout:
//! transport on top, browser on the left, Session or Arrangement in the
//! middle, and a detail view (clip editor / device chain) at the bottom.

mod arrangement;
mod browser;
mod detail;
mod session;
mod transport;

use crate::app::{App, ClipRef, DialogKind, View};
use eframe::egui::{self, Color32, Key};
use loopcraft_core::TrackKind;
use loopcraft_engine::Command;

pub mod theme {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x20);
    pub const PANEL: Color32 = Color32::from_rgb(0x26, 0x28, 0x2c);
    pub const SLOT: Color32 = Color32::from_rgb(0x33, 0x35, 0x3a);
    pub const SLOT_HOVER: Color32 = Color32::from_rgb(0x3d, 0x40, 0x46);
    pub const LINE: Color32 = Color32::from_rgb(0x44, 0x47, 0x4d);
    pub const TEXT: Color32 = Color32::from_rgb(0xdd, 0xdd, 0xdd);
    pub const DIM: Color32 = Color32::from_rgb(0x8a, 0x8d, 0x93);
    pub const ACCENT: Color32 = Color32::from_rgb(0xff, 0x9a, 0x3c);
    pub const PLAY: Color32 = Color32::from_rgb(0x6f, 0xdc, 0x6f);
    pub const REC: Color32 = Color32::from_rgb(0xf0, 0x50, 0x50);
    pub const SOLO: Color32 = Color32::from_rgb(0x4f, 0x9c, 0xff);

    pub fn rgb(c: [u8; 3]) -> Color32 {
        Color32::from_rgb(c[0], c[1], c[2])
    }

    pub fn dim(c: Color32, f: f32) -> Color32 {
        let m = |x: u8| (x as f32 * f) as u8;
        Color32::from_rgb(m(c.r()), m(c.g()), m(c.b()))
    }
}

pub fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = theme::PANEL;
    v.window_fill = theme::PANEL;
    v.extreme_bg_color = theme::BG;
    v.selection.bg_fill = theme::dim(theme::ACCENT, 0.7);
    v.widgets.inactive.weak_bg_fill = theme::SLOT;
    v.widgets.hovered.weak_bg_fill = theme::SLOT_HOVER;
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        s.visuals = v.clone();
        s.spacing.item_spacing = egui::vec2(4.0, 3.0);
        s.spacing.button_padding = egui::vec2(6.0, 2.0);
    });
}

/// Computer keyboard as a piano, laid out like most DAWs: A row = white keys.
const KEYMAP: [(Key, i32); 17] = [
    (Key::A, 0),
    (Key::W, 1),
    (Key::S, 2),
    (Key::E, 3),
    (Key::D, 4),
    (Key::F, 5),
    (Key::T, 6),
    (Key::G, 7),
    (Key::Y, 8),
    (Key::H, 9),
    (Key::U, 10),
    (Key::J, 11),
    (Key::K, 12),
    (Key::O, 13),
    (Key::L, 14),
    (Key::P, 15),
    (Key::Semicolon, 16),
];

fn handle_keys(app: &mut App, ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() || app.dialog.is_some() || app.renaming.is_some() {
        return;
    }
    let events = ctx.input(|i| i.events.clone());
    let kb_track = app.selected_track.filter(|&t| app.computer_keys && app.project.tracks.get(t).is_some_and(|t| t.kind == TrackKind::Midi));
    for ev in events {
        let egui::Event::Key { key, pressed, repeat, modifiers, .. } = ev else { continue };
        if modifiers.command {
            if !pressed {
                continue;
            }
            match key {
                Key::S if modifiers.shift => app.open_dialog(DialogKind::SaveAs),
                Key::S => app.save(),
                Key::O => app.open_dialog(DialogKind::Open),
                Key::N => app.new_project(),
                Key::Z if modifiers.shift => app.redo(),
                Key::Z => app.undo(),
                Key::Y => app.redo(),
                Key::D => {
                    if let Some(ClipRef::Session { track, slot }) = app.selected_clip {
                        app.duplicate_clip_down(track, slot);
                    }
                }
                _ => {}
            }
            continue;
        }
        if let (Some(t), Some(&(_, semi))) = (kb_track, KEYMAP.iter().find(|(k, _)| *k == key)) {
            if repeat {
                continue;
            }
            if pressed {
                let note = (60 + app.octave * 12 + semi).clamp(0, 127) as u8;
                app.held_notes.push((key, note));
                app.send(Command::NoteOn { track: t, note, velocity: 100 });
            } else if let Some(i) = app.held_notes.iter().position(|(k, _)| *k == key) {
                let (_, note) = app.held_notes.remove(i);
                app.send(Command::NoteOff { track: t, note });
            }
            continue;
        }
        if !pressed || repeat {
            continue;
        }
        match key {
            Key::Space => {
                if app.engine.state.playing.load(std::sync::atomic::Ordering::Relaxed) {
                    app.send(Command::Stop);
                } else {
                    app.send(Command::Play);
                }
            }
            Key::Tab => app.view = if app.view == View::Session { View::Arrangement } else { View::Session },
            Key::Z if kb_track.is_some() => app.octave = (app.octave - 1).max(-4),
            Key::X if kb_track.is_some() => app.octave = (app.octave + 1).min(4),
            Key::Delete | Key::Backspace => {
                if let (Some(n), Some(c)) = (app.piano_selected, app.selected_clip) {
                    app.checkpoint();
                    if let Some(loopcraft_core::ClipContent::Midi { notes }) = app.clip_mut(c).map(|c| &mut c.content)
                        && n < notes.len()
                    {
                        notes.remove(n);
                    }
                    app.piano_selected = None;
                    app.commit_clip(c);
                } else if let Some(c) = app.selected_clip {
                    app.delete_clip(c);
                }
            }
            _ => {}
        }
    }
}

fn dialogs(app: &mut App, ctx: &egui::Context) {
    let Some(d) = app.dialog.as_mut() else { return };
    let title = match d.kind {
        DialogKind::Open => "Open Project",
        DialogKind::SaveAs => "Save Project As",
        DialogKind::Export => "Export Audio",
    };
    let mut action: Option<bool> = None;
    egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        ui.label("File path:");
        let r = ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(460.0));
        if d.kind == DialogKind::Export {
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.export_arrangement, true, "Arrangement");
                ui.radio_value(&mut d.export_arrangement, false, "All scenes, 8 bars each");
            });
            ui.label(egui::RichText::new("48 kHz, 24-bit WAV").color(theme::DIM));
        }
        ui.horizontal(|ui| {
            let label = match d.kind {
                DialogKind::Open => "Open",
                DialogKind::SaveAs => "Save",
                DialogKind::Export => "Export",
            };
            if ui.button(label).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                action = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                action = Some(false);
            }
        });
    });
    if let Some(go) = action {
        let d = app.dialog.take().expect("dialog open");
        if go {
            let path = std::path::PathBuf::from(d.path.trim());
            match d.kind {
                DialogKind::Open => app.open(&path),
                DialogKind::SaveAs => app.save_to(&path),
                DialogKind::Export => app.export(path, d.export_arrangement),
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        handle_keys(self, &ctx);

        egui::Panel::top("transport").frame(egui::Frame::new().fill(theme::BG).inner_margin(6)).show(ui, |ui| transport::show(self, ui));
        egui::Panel::left("browser").resizable(true).default_size(170.0).show(ui, |ui| browser::show(self, ui));
        egui::Panel::bottom("detail").resizable(true).default_size(260.0).min_size(180.0).show(ui, |ui| detail::show(self, ui));
        egui::CentralPanel::default_margins().show(ui, |ui| match self.view {
            View::Session => session::show(self, ui),
            View::Arrangement => arrangement::show(self, ui),
        });
        dialogs(self, &ctx);

        let title = format!("{}{} — LoopCraft", self.project.name, if self.dirty { " •" } else { "" });
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

/// Bars.beats.sixteenths, 1-based, like a DAW position display.
pub fn format_position(beat: f64, beats_per_bar: f64) -> String {
    let bar = (beat / beats_per_bar).floor();
    let in_bar = beat - bar * beats_per_bar;
    let b = in_bar.floor();
    let six = ((in_bar - b) * 4.0).floor();
    format!("{}.{}.{}", bar as i64 + 1, b as i64 + 1, six as i64 + 1)
}

/// A small level meter, drawn bottom-up.
pub fn meter(ui: &mut egui::Ui, l: f32, r: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, height), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 1.0, theme::BG);
    let to_h = |v: f32| {
        let db = loopcraft_core::gain_to_db(v).max(-60.0);
        ((db + 60.0) / 66.0).clamp(0.0, 1.0) * rect.height()
    };
    for (i, v) in [l, r].into_iter().enumerate() {
        let x0 = rect.left() + i as f32 * 5.0;
        let h = to_h(v);
        let bar = egui::Rect::from_min_max(egui::pos2(x0, rect.bottom() - h), egui::pos2(x0 + 4.0, rect.bottom()));
        let color = if v >= 1.0 {
            theme::REC
        } else if v > 0.5 {
            theme::ACCENT
        } else {
            theme::PLAY
        };
        p.rect_filled(bar, 0.0, color);
    }
}

#[derive(Clone, Copy)]
pub enum Icon {
    Play,
    Stop,
    Record,
}

/// A button with a painted icon (the default fonts lack some transport glyphs).
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, active: bool, active_color: Color32, size: egui::Vec2) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let p = ui.painter();
    let bg = if active {
        active_color
    } else if resp.hovered() {
        theme::SLOT_HOVER
    } else {
        theme::SLOT
    };
    p.rect_filled(rect, 3.0, bg);
    let fg = if active { Color32::BLACK } else { theme::TEXT };
    let c = rect.center();
    let r = (size.y * 0.28).min(size.x * 0.3);
    match icon {
        Icon::Play => {
            p.add(egui::Shape::convex_polygon(vec![c + egui::vec2(-r * 0.8, -r), c + egui::vec2(-r * 0.8, r), c + egui::vec2(r, 0.0)], fg, egui::Stroke::NONE));
        }
        Icon::Stop => {
            p.rect_filled(egui::Rect::from_center_size(c, egui::vec2(r * 1.6, r * 1.6)), 1.0, fg);
        }
        Icon::Record => {
            p.circle_filled(c, r * 0.95, if active { Color32::BLACK } else { theme::REC });
        }
    }
    resp
}

pub fn small_toggle(ui: &mut egui::Ui, on: bool, label: &str, on_color: Color32) -> egui::Response {
    let fill = if on { on_color } else { theme::SLOT };
    let text = egui::RichText::new(label).color(if on { Color32::BLACK } else { theme::TEXT }).size(11.0);
    ui.add(egui::Button::new(text).fill(fill).min_size(egui::vec2(20.0, 18.0)))
}
