//! Detail view: clip editor (piano roll / waveform) and device chain.

use super::theme;
use crate::app::{App, ClipRef, DetailTab};
use eframe::egui::{self, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use loopcraft_core::*;
use loopcraft_dsp::specs;
use loopcraft_engine::{DeviceRef, TrackRef};
use std::sync::atomic::Ordering::Relaxed;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.selectable_value(&mut app.detail_tab, DetailTab::Clip, "Clip");
        ui.selectable_value(&mut app.detail_tab, DetailTab::Devices, "Devices");
        ui.separator();
        let target = match app.device_target {
            TrackRef::Track(t) => app.project.tracks.get(t).map(|t| t.name.clone()).unwrap_or_default(),
            TrackRef::Return(r) => app.project.returns.get(r).map(|t| t.name.clone()).unwrap_or_default(),
            TrackRef::Master => "Master".into(),
        };
        let what = match (app.detail_tab, app.selected_clip.and_then(|c| app.clip(c))) {
            (DetailTab::Clip, Some(c)) => c.name.clone(),
            (DetailTab::Clip, None) => "no clip selected".into(),
            (DetailTab::Devices, _) => target,
        };
        ui.label(RichText::new(what).color(theme::DIM));
        if !app.status.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(&app.status).color(theme::ACCENT).size(11.0));
            });
        }
    });
    ui.separator();
    match app.detail_tab {
        DetailTab::Clip => clip_view(app, ui),
        DetailTab::Devices => device_chain(app, ui),
    }
}

// ---------------------------------------------------------------------------
// Clip view
// ---------------------------------------------------------------------------

fn clip_view(app: &mut App, ui: &mut egui::Ui) {
    let Some(cref) = app.selected_clip.filter(|c| app.clip(*c).is_some()) else {
        ui.label(RichText::new("Click a clip to edit it. Double-click an empty slot on a MIDI track to create one.").color(theme::DIM));
        return;
    };
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(190.0);
            clip_properties(app, ui, cref);
        });
        ui.separator();
        let is_midi = matches!(app.clip(cref).map(|c| &c.content), Some(ClipContent::Midi { .. }));
        if is_midi {
            piano_roll(app, ui, cref);
        } else {
            waveform(app, ui, cref);
        }
    });
}

fn clip_properties(app: &mut App, ui: &mut egui::Ui, cref: ClipRef) {
    let mut changed = false;
    let mut c = app.clip(cref).cloned().expect("checked");
    egui::Grid::new("clipprops").num_columns(2).spacing([6.0, 4.0]).show(ui, |ui| {
        ui.label("Name");
        changed |= ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(110.0)).lost_focus();
        ui.end_row();
        ui.label("Length");
        changed |= ui.add(egui::DragValue::new(&mut c.length).range(0.25..=1024.0).speed(0.25).suffix(" beats")).changed();
        ui.end_row();
        ui.label("Loop");
        changed |= ui.checkbox(&mut c.looping, "").changed();
        ui.end_row();
        ui.label("Launch");
        egui::ComboBox::from_id_salt("launchmode").width(100.0).selected_text(format!("{:?}", c.launch_mode)).show_ui(ui, |ui| {
            for m in [LaunchMode::Trigger, LaunchMode::Gate, LaunchMode::Toggle] {
                changed |= ui.selectable_value(&mut c.launch_mode, m, format!("{m:?}")).changed();
            }
        });
        ui.end_row();
        ui.label("Quantize");
        let label = c.quantization.map(|q| q.label()).unwrap_or("Global");
        egui::ComboBox::from_id_salt("clipquant").width(100.0).selected_text(label).show_ui(ui, |ui| {
            changed |= ui.selectable_value(&mut c.quantization, None, "Global").changed();
            for q in Quantization::ALL {
                changed |= ui.selectable_value(&mut c.quantization, Some(q), q.label()).changed();
            }
        });
        ui.end_row();
        if let ClipContent::Audio(a) = &mut c.content {
            ui.label("Warp");
            egui::ComboBox::from_id_salt("warp").width(100.0).selected_text(format!("{:?}", a.warp)).show_ui(ui, |ui| {
                for w in [WarpMode::Stretch, WarpMode::Repitch, WarpMode::Off] {
                    changed |= ui.selectable_value(&mut a.warp, w, format!("{w:?}")).changed();
                }
            });
            ui.end_row();
            ui.label("Orig. BPM");
            changed |= ui.add(egui::DragValue::new(&mut a.original_bpm).range(20.0..=400.0).speed(0.1)).changed();
            ui.end_row();
            ui.label("Start");
            changed |= ui.add(egui::DragValue::new(&mut a.start_offset).range(0.0..=1024.0).speed(0.25).suffix(" beats")).changed();
            ui.end_row();
            ui.label("Gain");
            changed |= ui.add(egui::DragValue::new(&mut a.gain_db).range(-40.0..=24.0).speed(0.1).suffix(" dB")).changed();
            ui.end_row();
            ui.label("File");
            ui.label(RichText::new(&a.file).size(10.0).color(theme::DIM));
            ui.end_row();
        }
    });
    if changed {
        app.checkpoint();
        if let Some(dst) = app.clip_mut(cref) {
            *dst = c;
        }
        app.commit_clip(cref);
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.small_button("Delete clip").clicked() {
            app.delete_clip(cref);
        }
        if let ClipRef::Session { track, slot } = cref {
            if ui.small_button("Duplicate down").clicked() {
                app.duplicate_clip_down(track, slot);
            }
        }
    });
}

fn piano_roll(app: &mut App, ui: &mut egui::Ui, cref: ClipRef) {
    let Some(clip) = app.clip(cref).cloned() else { return };
    let ClipContent::Midi { notes } = &clip.content else { return };

    // Grid selector.
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Grid").color(theme::DIM));
            for (g, l) in [(1.0, "1/4"), (0.5, "1/8"), (0.25, "1/16"), (0.125, "1/32")] {
                ui.selectable_value(&mut app.piano_grid, g, l);
            }
            ui.label(RichText::new("click: add · drag: move · drag right edge: resize · double-click / Delete: remove").size(10.0).color(theme::DIM));
        });

        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(vec2(avail.x.max(200.0), avail.y.max(80.0)), Sense::click_and_drag());
        let keys_w = 34.0;
        let grid_r = Rect::from_min_max(pos2(rect.left() + keys_w, rect.top()), rect.max);

        // Visible pitch range: 25 rows, centred on the notes when the clip is
        // first shown, then stable (mouse wheel scrolls it).
        const SPAN: i32 = 25;
        let mut lo = match app.piano_view {
            Some((c, lo)) if c == cref => lo,
            _ => {
                let lo = if notes.is_empty() {
                    48
                } else {
                    let (a, b) = notes.iter().fold((127i32, 0i32), |(lo, hi), n| (lo.min(n.pitch as i32), hi.max(n.pitch as i32)));
                    (a + b) / 2 - SPAN / 2
                };
                lo.clamp(0, 127 - SPAN)
            }
        };
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.5 {
                lo = (lo + if scroll > 0.0 { 1 } else { -1 }).clamp(0, 127 - SPAN);
            }
        }
        app.piano_view = Some((cref, lo));
        let hi = lo + SPAN - 1;
        let rows = (hi - lo + 1) as f32;
        let row_h = grid_r.height() / rows;
        let len = clip.length.max(0.25);
        let px_beat = grid_r.width() / len as f32;
        let y_of = |pitch: i32| grid_r.bottom() - (pitch - lo + 1) as f32 * row_h;
        let x_of = |beat: f64| grid_r.left() + beat as f32 * px_beat;
        let pitch_at = |y: f32| lo + ((grid_r.bottom() - y) / row_h).floor() as i32;
        let beat_at = |x: f32| ((x - grid_r.left()) / px_beat) as f64;

        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, theme::BG);
        for pitch in lo..=hi {
            let black = matches!(pitch % 12, 1 | 3 | 6 | 8 | 10);
            let y = y_of(pitch);
            let row = Rect::from_min_size(pos2(grid_r.left(), y), vec2(grid_r.width(), row_h));
            p.rect_filled(row, 0.0, if black { theme::BG } else { theme::dim(theme::SLOT, 0.8) });
            let key = Rect::from_min_size(pos2(rect.left(), y), vec2(keys_w - 2.0, row_h - 0.5));
            p.rect_filled(key, 0.0, if black { Color32::from_gray(40) } else { Color32::from_gray(200) });
            if pitch % 12 == 0 && row_h >= 7.0 {
                p.text(
                    key.right_center() - vec2(2.0, 0.0),
                    Align2::RIGHT_CENTER,
                    format!("C{}", pitch / 12 - 2),
                    FontId::proportional(row_h.min(10.0)),
                    Color32::BLACK,
                );
            }
        }
        let mut b = 0.0;
        while b <= len + 1e-9 {
            let strong = (b % 4.0).abs() < 1e-9;
            let beat = (b % 1.0).abs() < 1e-9;
            let c = if strong {
                theme::DIM
            } else if beat {
                theme::LINE
            } else {
                theme::dim(theme::LINE, 0.55)
            };
            p.line_segment([pos2(x_of(b), grid_r.top()), pos2(x_of(b), grid_r.bottom())], Stroke::new(1.0, c));
            b += app.piano_grid;
        }

        let color = theme::rgb(clip.color);
        for (i, n) in notes.iter().enumerate() {
            if (n.pitch as i32) < lo || (n.pitch as i32) > hi {
                continue;
            }
            let r = Rect::from_min_max(
                pos2(x_of(n.start), y_of(n.pitch as i32) + 0.5),
                pos2(x_of((n.start + n.length).min(len)), y_of(n.pitch as i32) + row_h - 0.5),
            );
            let vel = 0.45 + 0.55 * n.velocity as f32 / 127.0;
            p.rect_filled(r, 2.0, theme::dim(color, vel));
            let sel = app.piano_selected == Some(i);
            p.rect_stroke(r, 2.0, Stroke::new(if sel { 1.5 } else { 0.5 }, if sel { Color32::WHITE } else { Color32::BLACK }), StrokeKind::Inside);
        }

        // Playhead for session clips that are playing.
        if let ClipRef::Session { track, slot } = cref {
            if let Some(ts) = app.engine.state.tracks.get(track) {
                if ts.playing_slot.load(Relaxed) == slot as i32 {
                    let x = x_of(ts.clip_position.load());
                    p.line_segment([pos2(x, grid_r.top()), pos2(x, grid_r.bottom())], Stroke::new(1.5, theme::ACCENT));
                }
            }
        }

        // Interaction.
        let hit = |pos: egui::Pos2| -> Option<(usize, bool)> {
            let pitch = pitch_at(pos.y);
            let beat = beat_at(pos.x);
            notes.iter().enumerate().rev().find(|(_, n)| n.pitch as i32 == pitch && beat >= n.start && beat <= n.start + n.length).map(|(i, n)| {
                let right_edge = x_of(n.start + n.length);
                (i, (right_edge - pos.x) < 6.0)
            })
        };
        let snap = |b: f64, g: f64| (b / g).floor() * g;
        let grid = app.piano_grid;

        if resp.double_clicked() {
            if let Some((i, _)) = resp.interact_pointer_pos().and_then(hit) {
                app.checkpoint();
                edit_notes(app, cref, |ns| {
                    ns.remove(i);
                });
                app.piano_selected = None;
                app.commit_clip(cref);
                return;
            }
        }
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                match hit(pos) {
                    Some((i, _)) => app.piano_selected = Some(i),
                    None if grid_r.contains(pos) => {
                        let start = snap(beat_at(pos.x), grid);
                        let pitch = pitch_at(pos.y).clamp(0, 127) as u8;
                        if start < len {
                            app.checkpoint();
                            let mut idx = 0;
                            edit_notes(app, cref, |ns| {
                                ns.push(Note::new(pitch, start, grid, 100));
                                idx = ns.len() - 1;
                            });
                            app.piano_selected = Some(idx);
                            app.commit_clip(cref);
                        }
                    }
                    None => {}
                }
            }
        }
        if resp.drag_started() {
            if let Some(pos) = resp.interact_pointer_pos() {
                if let Some((i, resize)) = hit(pos) {
                    app.checkpoint();
                    app.piano_selected = Some(i);
                    app.piano_drag = Some((i, resize, notes[i], beat_at(pos.x), pitch_at(pos.y)));
                }
            }
        }
        if let (Some((i, resize, orig, b0, p0)), Some(pos)) = (app.piano_drag, resp.interact_pointer_pos()) {
            if resp.dragged() {
                let db = ((beat_at(pos.x) - b0) / grid).round() * grid;
                let dp = pitch_at(pos.y) - p0;
                edit_notes(app, cref, |ns| {
                    if let Some(n) = ns.get_mut(i) {
                        if resize {
                            n.length = (orig.length + db).max(grid.min(orig.length));
                        } else {
                            n.start = (orig.start + db).clamp(0.0, (len - grid).max(0.0));
                            n.pitch = (orig.pitch as i32 + dp).clamp(0, 127) as u8;
                        }
                    }
                });
            }
        }
        if resp.drag_stopped() && app.piano_drag.is_some() {
            app.piano_drag = None;
            app.commit_clip(cref);
        }
    });
}

fn edit_notes(app: &mut App, cref: ClipRef, f: impl FnOnce(&mut Vec<Note>)) {
    if let Some(ClipContent::Midi { notes }) = app.clip_mut(cref).map(|c| &mut c.content) {
        f(notes);
    }
}

fn waveform(app: &mut App, ui: &mut egui::Ui, cref: ClipRef) {
    let Some(clip) = app.clip(cref).cloned() else { return };
    let ClipContent::Audio(a) = &clip.content else { return };
    let avail = ui.available_size();
    let (rect, _) = ui.allocate_exact_size(vec2(avail.x.max(200.0), avail.y.max(80.0)), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, theme::BG);
    let Some(buf) = loopcraft_dsp::SampleLoader::load(app.loader.as_ref(), &a.file) else {
        p.text(rect.center(), Align2::CENTER_CENTER, format!("Missing file: {}", a.file), FontId::proportional(12.0), theme::REC);
        return;
    };
    let samples_per_beat = 60.0 / a.original_bpm * buf.sample_rate as f64;
    let start = a.start_offset * samples_per_beat;
    let total = clip.length * samples_per_beat;
    let color = theme::rgb(clip.color);
    let mid = rect.center().y;
    let w = rect.width().max(1.0) as usize;
    for x in 0..w {
        let s0 = (start + total * x as f64 / w as f64) as usize;
        let s1 = ((start + total * (x + 1) as f64 / w as f64) as usize).max(s0 + 1);
        let ch = &buf.channels[0];
        let (mut mn, mut mx) = (0.0f32, 0.0f32);
        for s in ch.iter().take(s1.min(ch.len())).skip(s0.min(ch.len())).step_by(((s1 - s0) / 64).max(1)) {
            mn = mn.min(*s);
            mx = mx.max(*s);
        }
        let xf = rect.left() + x as f32;
        p.line_segment([pos2(xf, mid - mx * rect.height() * 0.48), pos2(xf, mid - mn * rect.height() * 0.48)], Stroke::new(1.0, color));
    }
    for b in 0..=(clip.length.ceil() as usize) {
        let x = rect.left() + (b as f64 / clip.length) as f32 * rect.width();
        p.line_segment([pos2(x, rect.top()), pos2(x, rect.top() + if b % 4 == 0 { 12.0 } else { 6.0 })], Stroke::new(1.0, theme::DIM));
    }
    if let ClipRef::Session { track, slot } = cref {
        if let Some(ts) = app.engine.state.tracks.get(track) {
            if ts.playing_slot.load(Relaxed) == slot as i32 {
                let x = rect.left() + (ts.clip_position.load() / clip.length) as f32 * rect.width();
                p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, theme::ACCENT));
            }
        }
    }
    p.text(
        rect.left_bottom() + vec2(6.0, -6.0),
        Align2::LEFT_BOTTOM,
        format!("{:?} · {:.1} → {:.1} bpm", a.warp, a.original_bpm, app.engine.state.tempo.load()),
        FontId::proportional(11.0),
        theme::DIM,
    );
}

// ---------------------------------------------------------------------------
// Device chain
// ---------------------------------------------------------------------------

fn device_chain(app: &mut App, ui: &mut egui::Ui) {
    let ch = app.device_target;
    let (instrument, effects) = match ch {
        TrackRef::Track(t) => match app.project.tracks.get(t) {
            Some(tr) => (tr.instrument.clone(), tr.effects.clone()),
            None => {
                ui.label(RichText::new("Select a track.").color(theme::DIM));
                return;
            }
        },
        TrackRef::Return(r) => (None, app.project.returns.get(r).map(|t| t.effects.clone()).unwrap_or_default()),
        TrackRef::Master => (None, app.project.master.effects.clone()),
    };
    egui::ScrollArea::horizontal().show(ui, |ui| {
        ui.horizontal_top(|ui| {
            if let Some(spec) = instrument {
                device_box(app, ui, ch, DeviceRef::Instrument, &spec, None);
            }
            for (i, spec) in effects.iter().enumerate() {
                device_box(app, ui, ch, DeviceRef::Effect(i), spec, Some((i, effects.len())));
            }
            if effects.is_empty() {
                ui.label(RichText::new("Add effects from the browser on the left.").color(theme::DIM));
            }
        });
    });
}

fn device_box(app: &mut App, ui: &mut egui::Ui, ch: TrackRef, dev: DeviceRef, spec: &DeviceSpec, pos: Option<(usize, usize)>) {
    egui::Frame::new().fill(theme::BG).stroke(Stroke::new(1.0, theme::LINE)).corner_radius(4).inner_margin(6).show(ui, |ui| {
        ui.vertical(|ui| device_box_contents(app, ui, ch, dev, spec, pos));
    });
}

fn device_box_contents(app: &mut App, ui: &mut egui::Ui, ch: TrackRef, dev: DeviceRef, spec: &DeviceSpec, pos: Option<(usize, usize)>) {
    {
        ui.set_width(230.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(spec.name()).strong().color(if spec.is_instrument() { theme::ACCENT } else { theme::TEXT }));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((i, n)) = pos {
                    if ui.small_button("x").on_hover_text("Remove").clicked() {
                        app.remove_effect(ch, i);
                    }
                    if i + 1 < n && ui.small_button("▶").on_hover_text("Move right").clicked() {
                        app.move_effect(ch, i, false);
                    }
                    if i > 0 && ui.small_button("◀").on_hover_text("Move left").clicked() {
                        app.move_effect(ch, i, true);
                    }
                }
            });
        });
        ui.separator();
        if let DeviceSpec::Plugin(p) = spec {
            ui.label(RichText::new(format!("CLAP · {}", p.id)).size(10.0).color(theme::DIM));
            ui.label(RichText::new("Parameter editing for plugins is on the roadmap.").size(10.0).color(theme::DIM));
            return;
        }
        for (i, info) in specs::param_infos(spec).iter().enumerate() {
            let mut v = specs::get_param(spec, i);
            let log = matches!(info.unit, "Hz" | "ms" | "s") && info.min > 0.0;
            ui.horizontal(|ui| {
                ui.add_sized([66.0, 16.0], egui::Label::new(RichText::new(info.name).size(11.0).color(theme::DIM)).truncate());
                ui.spacing_mut().slider_width = 92.0;
                let slider = egui::Slider::new(&mut v, info.min..=info.max).logarithmic(log).suffix(format!(" {}", info.unit)).max_decimals(2);
                if ui.add(slider).changed() {
                    app.set_param(ch, dev, i, v);
                }
            });
        }
        extra_controls(app, ui, ch, dev, spec);
    }
}

/// Non-numeric settings that need a rebuild (sources, files, modes).
fn extra_controls(app: &mut App, ui: &mut egui::Ui, ch: TrackRef, dev: DeviceRef, spec: &DeviceSpec) {
    let mut edited: Option<DeviceSpec> = None;
    match spec {
        DeviceSpec::Compressor(c) => {
            let tracks: Vec<String> = app.project.tracks.iter().map(|t| t.name.clone()).collect();
            let label = c.sidechain_track.and_then(|i| tracks.get(i).cloned()).unwrap_or_else(|| "Off".into());
            let mut sel = c.sidechain_track;
            egui::ComboBox::from_id_salt(("sc", format!("{ch:?}{dev:?}"))).selected_text(format!("Sidechain: {label}")).show_ui(ui, |ui| {
                ui.selectable_value(&mut sel, None, "Off");
                for (i, n) in tracks.iter().enumerate() {
                    if TrackRef::Track(i) != ch {
                        ui.selectable_value(&mut sel, Some(i), n);
                    }
                }
            });
            if sel != c.sidechain_track {
                let mut c2 = c.clone();
                c2.sidechain_track = sel;
                edited = Some(DeviceSpec::Compressor(c2));
            }
        }
        DeviceSpec::Sampler(s) => {
            let mut file = s.file.clone();
            ui.horizontal(|ui| {
                ui.label(RichText::new("File").size(11.0).color(theme::DIM));
                if ui.add(egui::TextEdit::singleline(&mut file).desired_width(140.0)).lost_focus() && file != s.file {
                    let mut s2 = s.clone();
                    s2.file = file.clone();
                    edited = Some(DeviceSpec::Sampler(s2));
                }
            });
            let mut root = s.root_key;
            if ui.add(egui::DragValue::new(&mut root).range(0..=127).prefix("Root key ")).changed() {
                let mut s2 = s.clone();
                s2.root_key = root;
                edited = Some(DeviceSpec::Sampler(s2));
            }
        }
        DeviceSpec::PolySynth(p) => {
            let mut p2 = p.clone();
            let mut changed = false;
            ui.horizontal(|ui| {
                for (label, w) in [("Osc 1", &mut p2.osc1), ("Osc 2", &mut p2.osc2)] {
                    let id = (label, format!("{ch:?}"));
                    egui::ComboBox::from_id_salt(id).width(70.0).selected_text(format!("{label}: {w:?}")).show_ui(ui, |ui| {
                        for opt in [Waveform::Saw, Waveform::Square, Waveform::Triangle, Waveform::Sine, Waveform::Noise] {
                            changed |= ui.selectable_value(w, opt, format!("{opt:?}")).changed();
                        }
                    });
                }
            });
            egui::ComboBox::from_id_salt(("fmode", format!("{ch:?}"))).selected_text(format!("Filter: {:?}", p2.filter_mode)).show_ui(ui, |ui| {
                for m in [FilterMode::LowPass, FilterMode::HighPass, FilterMode::BandPass] {
                    changed |= ui.selectable_value(&mut p2.filter_mode, m, format!("{m:?}")).changed();
                }
            });
            if changed {
                edited = Some(DeviceSpec::PolySynth(p2));
            }
        }
        DeviceSpec::DrumRack(d) => {
            ui.label(
                RichText::new(format!("{} pads · notes {}", d.pads.len(), d.pads.iter().map(|p| p.note.to_string()).collect::<Vec<_>>().join(" ")))
                    .size(10.0)
                    .color(theme::DIM),
            );
        }
        _ => {}
    }
    if let Some(new) = edited {
        app.checkpoint();
        let slot = match (ch, dev) {
            (TrackRef::Track(t), DeviceRef::Instrument) => app.project.tracks.get_mut(t).and_then(|t| t.instrument.as_mut()),
            (_, DeviceRef::Effect(e)) => app.effects_mut(ch).and_then(|fx| fx.get_mut(e)),
            _ => None,
        };
        if let Some(s) = slot {
            *s = new;
        }
        app.rebuild();
    }
}
