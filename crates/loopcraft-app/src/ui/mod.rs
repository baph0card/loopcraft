//! The user interface. Layout follows the familiar clip-launcher layout:
//! transport on top, browser on the left, Session or Arrangement in the
//! middle, and a detail view (clip editor / device chain) at the bottom.

mod arrangement;
mod browser;
mod detail;
mod devices;
mod session;
mod transport;
mod windows;

use crate::app::{App, ClipRef, DialogKind, MapMode, View};
use eframe::egui::{self, Color32, Key};
use loopcraft_core::{MappingSource, MappingTarget};
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
    pub const MIDI_MAP: Color32 = Color32::from_rgb(0x8a, 0x5c, 0xf0);
    pub const KEY_MAP: Color32 = Color32::from_rgb(0xf0, 0xa0, 0x30);
    pub const XFADE_A: Color32 = Color32::from_rgb(0xf0, 0xb0, 0x40);
    pub const XFADE_B: Color32 = Color32::from_rgb(0x40, 0xb0, 0xf0);

    pub fn rgb(c: [u8; 3]) -> Color32 {
        Color32::from_rgb(c[0], c[1], c[2])
    }

    pub fn dim(c: Color32, f: f32) -> Color32 {
        let m = |x: u8| (x as f32 * f).min(255.0) as u8;
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

fn key_char(key: Key) -> Option<char> {
    let n = key.symbol_or_name();
    let mut it = n.chars();
    let c = it.next()?;
    it.next().is_none().then(|| c.to_ascii_lowercase())
}

fn handle_keys(app: &mut App, ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() || app.dialog.is_some() || app.renaming.is_some() {
        return;
    }
    let events = ctx.input(|i| i.events.clone());
    let live = app.live_targets();
    let kb = app.computer_keys && !live.is_empty() && app.map_mode.is_none();
    for ev in events {
        match &ev {
            egui::Event::Copy => {
                if let Some(c) = note_target(app)
                    && app.copy_notes(c)
                {
                    app.notes_copied_last = true;
                } else if let Some(c) = app.selected_clip.and_then(|c| app.clip(c)).cloned() {
                    app.notes_copied_last = false;
                    app.clipboard = Some(c);
                    app.status = "Copied clip".into();
                }
                continue;
            }
            egui::Event::Paste(_) => {
                if app.notes_copied_last
                    && let Some(c) = note_target_any(app)
                {
                    let at = app.piano_cursor;
                    app.paste_notes(c, at);
                } else if let (Some(c), Some(ClipRef::Session { track, slot })) = (app.clipboard.clone(), app.selected_clip) {
                    app.paste_clip(track, slot, c);
                }
                continue;
            }
            _ => {}
        }
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
                Key::A if note_target_any(app).is_some() => {
                    let c = note_target_any(app).expect("checked");
                    app.select_all_notes(c);
                }
                Key::D if note_target(app).is_some() => {
                    let c = note_target(app).expect("checked");
                    let g = app.piano_grid;
                    app.duplicate_selected_notes(c, g);
                }
                Key::D => match app.selected_clip {
                    Some(ClipRef::Session { track, slot }) => app.duplicate_clip_down(track, slot),
                    Some(ClipRef::Arrangement { track, index }) => app.duplicate_arrangement_clip(track, index),
                    None => {}
                },
                Key::E => {
                    if let Some(ClipRef::Arrangement { track, index }) = app.selected_clip {
                        let at = app.engine.state.beat.load();
                        app.split_arrangement_clip(track, index, at);
                    }
                }
                Key::G => app.group_selected(),
                Key::T if modifiers.shift => {
                    app.add_midi_track(loopcraft_core::DeviceSpec::PolySynth(Default::default()));
                }
                Key::T => {
                    app.add_audio_track();
                }
                Key::M => toggle_map(app, MapMode::Midi),
                Key::K => toggle_map(app, MapMode::Key),
                Key::Comma => app.open_settings(),
                _ => {}
            }
            continue;
        }
        if pressed && !repeat && key == Key::Escape {
            app.map_mode = None;
            app.map_target = None;
            app.macro_learn = None;
            continue;
        }
        // Key mappings (and key-map learn) take precedence over the piano keys.
        if pressed
            && !repeat
            && let Some(c) = key_char(key)
            && app.key_mapping(c)
        {
            continue;
        }
        if kb && let Some(&(_, semi)) = KEYMAP.iter().find(|(k, _)| *k == key) {
            if repeat {
                continue;
            }
            if pressed {
                let note = (60 + app.octave * 12 + semi).clamp(0, 127) as u8;
                app.held_notes.push((key, note));
                for &t in &live {
                    app.send(Command::NoteOn { track: t, note, velocity: app.key_velocity });
                }
            } else if let Some(i) = app.held_notes.iter().position(|(k, _)| *k == key) {
                let (_, note) = app.held_notes.remove(i);
                for &t in &live {
                    app.send(Command::NoteOff { track: t, note });
                }
            }
            continue;
        }
        if !pressed || repeat {
            continue;
        }
        match key {
            Key::Space => {
                if app.playing() {
                    app.send(Command::Stop);
                } else if modifiers.shift {
                    app.send(Command::Play);
                } else {
                    app.play();
                }
            }
            Key::Tab => app.view = if app.view == View::Session { View::Arrangement } else { View::Session },
            Key::F9 => app.toggle_arrangement_record(),
            Key::Z if kb => app.octave = (app.octave - 1).max(-4),
            Key::X if kb => app.octave = (app.octave + 1).min(4),
            Key::C if kb => app.key_velocity = app.key_velocity.saturating_sub(20).max(1),
            Key::V if kb => app.key_velocity = (app.key_velocity + 20).min(127),
            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight if note_target(app).is_some() => {
                let c = note_target(app).expect("checked");
                let g = app.piano_grid;
                let (db, dp) = match key {
                    Key::ArrowUp => (0.0, if modifiers.shift { 12 } else { 1 }),
                    Key::ArrowDown => (0.0, if modifiers.shift { -12 } else { -1 }),
                    Key::ArrowLeft => (-g, 0),
                    _ => (g, 0),
                };
                app.nudge_notes(c, db, dp);
            }
            Key::ArrowUp if app.view == View::Session => app.selected_scene = app.selected_scene.saturating_sub(1),
            Key::ArrowDown if app.view == View::Session => app.selected_scene = (app.selected_scene + 1).min(app.project.scenes.len().saturating_sub(1)),
            Key::Enter if app.view == View::Session => {
                let s = app.selected_scene;
                app.send(Command::LaunchScene { scene: s });
                app.selected_scene = (s + 1).min(app.project.scenes.len().saturating_sub(1));
            }
            Key::Delete | Key::Backspace => {
                if let Some(c) = note_target(app) {
                    app.delete_selected_notes(c);
                } else if let Some(c) = app.selected_clip {
                    app.delete_clip(c);
                }
            }
            _ => {}
        }
    }
}

/// The MIDI clip shown in the clip editor, if it has selected notes.
fn note_target(app: &App) -> Option<ClipRef> {
    let c = note_target_any(app)?;
    (!app.selected_notes(c).is_empty()).then_some(c)
}

/// The MIDI clip shown in the clip editor.
fn note_target_any(app: &App) -> Option<ClipRef> {
    let c = app.selected_clip?;
    (app.detail_tab == crate::app::DetailTab::Clip && matches!(app.clip(c).map(|c| &c.content), Some(loopcraft_core::ClipContent::Midi { .. }))).then_some(c)
}

pub fn toggle_map(app: &mut App, m: MapMode) {
    app.map_mode = if app.map_mode == Some(m) { None } else { Some(m) };
    app.map_target = None;
    app.status = match app.map_mode {
        Some(MapMode::Midi) => "MIDI Map: click a control, then move a knob or press a pad (Esc to finish)".into(),
        Some(MapMode::Key) => "Key Map: click a control, then press a computer key (Esc to finish)".into(),
        None => String::new(),
    };
}

/// In map mode, cover a control with a clickable overlay that picks it as
/// the mapping target (and shows its current mapping).
pub fn mappable(app: &mut App, ui: &mut egui::Ui, rect: egui::Rect, target: MappingTarget) {
    let Some(mode) = app.map_mode else { return };
    let resp = ui.interact(rect, ui.id().with(("map", format!("{target:?}"))), egui::Sense::click());
    let color = if mode == MapMode::Midi { theme::MIDI_MAP } else { theme::KEY_MAP };
    let picked = app.map_target == Some(target);
    let p = ui.painter();
    p.rect_filled(
        rect,
        2.0,
        color.gamma_multiply(if picked {
            0.85
        } else if resp.hovered() {
            0.6
        } else {
            0.4
        }),
    );
    let existing = app.project.mappings.iter().find(|m| m.target == target).map(|m| match m.source {
        MappingSource::Cc { channel, cc } => format!("{}/CC{cc}", channel + 1),
        MappingSource::Note { channel, note } => format!("{}/N{note}", channel + 1),
        MappingSource::Key(c) => c.to_uppercase().to_string(),
    });
    if let Some(label) = existing {
        p.text(rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::monospace(10.0), Color32::WHITE);
    }
    if resp.clicked() {
        app.pick_map_target(target);
    }
}

fn dialogs(app: &mut App, ctx: &egui::Context) {
    let Some(d) = app.dialog.as_mut() else { return };
    let title = match d.kind {
        DialogKind::Open => "Open Project",
        DialogKind::SaveAs => "Save Project As",
        DialogKind::Export => "Export Audio",
        DialogKind::SavePreset => "Save Device Preset",
    };
    let mut action: Option<bool> = None;
    let recent = app.settings.recent.clone();
    egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        ui.label(if d.kind == DialogKind::SavePreset { "Preset name:" } else { "File path:" });
        let r = ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(460.0));
        if d.kind == DialogKind::Open && !recent.is_empty() {
            ui.label(egui::RichText::new("Recent").color(theme::DIM));
            for p in recent.iter().take(6) {
                if ui.add(egui::Button::new(egui::RichText::new(p.display().to_string()).size(11.0)).frame(false)).clicked() {
                    d.path = p.display().to_string();
                }
            }
        }
        if d.kind == DialogKind::Export {
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.export_arrangement, true, "Arrangement");
                ui.radio_value(&mut d.export_arrangement, false, "All scenes, 8 bars each");
            });
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("exportfmt").selected_text(crate::app::EXPORT_FORMATS[d.export_format].0).show_ui(ui, |ui| {
                    for (i, f) in crate::app::EXPORT_FORMATS.iter().enumerate() {
                        ui.selectable_value(&mut d.export_format, i, f.0);
                    }
                });
                egui::ComboBox::from_id_salt("exportrate").selected_text(format!("{} Hz", d.export_rate)).show_ui(ui, |ui| {
                    for r in crate::app::EXPORT_RATES {
                        ui.selectable_value(&mut d.export_rate, r, format!("{r} Hz"));
                    }
                });
                ui.checkbox(&mut d.export_stems, "Stems (one file per track)");
            });
        }
        ui.horizontal(|ui| {
            let label = match d.kind {
                DialogKind::Open => "Open",
                DialogKind::SaveAs | DialogKind::SavePreset => "Save",
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
                DialogKind::Export => app.export(path, d.export_arrangement, d.export_stems, d.export_format, d.export_rate),
                DialogKind::SavePreset => {
                    if let Some(spec) = d.preset_spec {
                        app.save_preset(&spec, d.path.trim());
                    }
                }
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
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(8, 2)))
            .show(ui, |ui| transport::status_bar(self, ui));
        egui::Panel::left("browser").resizable(true).default_size(190.0).show(ui, |ui| browser::show(self, ui));
        egui::Panel::bottom("detail").resizable(true).default_size(330.0).min_size(220.0).show(ui, |ui| detail::show(self, ui));
        egui::CentralPanel::default_margins().show(ui, |ui| match self.view {
            View::Session => session::show(self, ui),
            View::Arrangement => arrangement::show(self, ui),
        });
        dialogs(self, &ctx);
        windows::show(self, &ctx);

        let pointer_down = ctx.input(|i| i.pointer.any_down());
        self.end_frame(pointer_down);
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

/// A rotary knob: drag up/down, double-click resets to `default`.
pub fn knob(ui: &mut egui::Ui, value: &mut f32, min: f32, max: f32, default: f32, size: f32, color: Color32) -> egui::Response {
    let (rect, mut resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
    let span = (max - min).max(1e-9);
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let d = -resp.drag_delta().y / if fine { 600.0 } else { 150.0 } * span;
        *value = (*value + d).clamp(min.min(max), max.max(min));
        resp.mark_changed();
    }
    if resp.double_clicked() {
        *value = default;
        resp.mark_changed();
    }
    let t = ((*value - min) / span).clamp(0.0, 1.0);
    let p = ui.painter();
    let c = rect.center();
    let rad = size * 0.42;
    let a0 = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let arc = |from: f32, to: f32| -> Vec<egui::Pos2> {
        let n = 24;
        (0..=n)
            .map(|i| {
                let a = from + (to - from) * i as f32 / n as f32;
                c + egui::vec2(a.cos(), a.sin()) * rad
            })
            .collect()
    };
    p.add(egui::Shape::line(arc(a0, a0 + sweep), egui::Stroke::new(3.0, theme::SLOT)));
    if t > 0.0 {
        p.add(egui::Shape::line(arc(a0, a0 + sweep * t), egui::Stroke::new(3.0, color)));
    }
    let a = a0 + sweep * t;
    p.line_segment([c, c + egui::vec2(a.cos(), a.sin()) * rad], egui::Stroke::new(1.5, theme::TEXT));
    resp
}
