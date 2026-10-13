//! Detail view: clip editor (piano roll / waveform with warp markers) and
//! the device chain.

use super::{session::follow_editor, theme};
use crate::app::{App, ClipRef, DetailTab, PianoDrag, PianoDragMode};
use eframe::egui::{self, Align2, Color32, FontId, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use loopcraft_core::*;
use loopcraft_engine::TrackRef;
use std::sync::atomic::Ordering::Relaxed;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.selectable_value(&mut app.detail_tab, DetailTab::Clip, "Clip");
        ui.selectable_value(&mut app.detail_tab, DetailTab::Devices, "Devices");
        ui.separator();
        let what = match (app.detail_tab, app.selected_clip.and_then(|c| app.clip(c))) {
            (DetailTab::Clip, Some(c)) => c.name.clone(),
            (DetailTab::Clip, None) => "no clip selected".into(),
            (DetailTab::Devices, _) => app.track_ref_name(app.device_target),
        };
        ui.label(RichText::new(what).color(theme::DIM));
        if app.detail_tab == DetailTab::Devices {
            ui.separator();
            for (label, ch) in [("Master", TrackRef::Master)].into_iter().chain((0..app.project.returns.len()).map(|r| ("Return", TrackRef::Return(r)))) {
                let name = if label == "Master" { "Master".to_string() } else { app.track_ref_name(ch) };
                if ui.selectable_label(app.device_target == ch, RichText::new(name).size(11.0)).clicked() {
                    app.device_target = ch;
                }
            }
            if let Some(t) = app.selected_track
                && ui.selectable_label(app.device_target == TrackRef::Track(t), RichText::new(app.track_ref_name(TrackRef::Track(t))).size(11.0)).clicked()
            {
                app.device_target = TrackRef::Track(t);
            }
        }
    });
    ui.separator();
    match app.detail_tab {
        DetailTab::Clip => clip_view(app, ui),
        DetailTab::Devices => super::devices::device_chain(app, ui),
    }
}

// ---------------------------------------------------------------------------
// Clip view
// ---------------------------------------------------------------------------

fn clip_view(app: &mut App, ui: &mut egui::Ui) {
    let Some(cref) = app.selected_clip.filter(|c| app.clip(*c).is_some()) else {
        ui.label(
            RichText::new("Click a clip to edit it. Double-click an empty slot on a MIDI track to create one; arm a track and click an empty slot to record.")
                .color(theme::DIM),
        );
        return;
    };
    ui.horizontal_top(|ui| {
        let h = ui.available_height();
        ui.allocate_ui_with_layout(vec2(224.0, h), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::ScrollArea::vertical().id_salt("clipprops").auto_shrink([false, false]).show(ui, |ui| {
                clip_properties(app, ui, cref);
            });
        });
        ui.separator();
        // A note selection belongs to one clip.
        if app.piano_sel_owner != Some(cref) {
            app.piano_sel.clear();
            app.piano_sel_owner = Some(cref);
        }
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
    let grooves: Vec<String> = app.project.grooves.iter().map(|g| g.name.clone()).collect();
    egui::Grid::new("clipprops_grid").num_columns(2).spacing([6.0, 4.0]).show(ui, |ui| {
        ui.label("Name");
        changed |= ui.add(egui::TextEdit::singleline(&mut c.name).desired_width(120.0)).lost_focus();
        ui.end_row();
        ui.label("Color");
        ui.horizontal(|ui| {
            for col in App::PALETTE {
                let (r, resp) = ui.allocate_exact_size(vec2(11.0, 11.0), Sense::click());
                ui.painter().rect_filled(r, 2.0, theme::rgb(col));
                if c.color == col {
                    ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Outside);
                }
                if resp.clicked() {
                    c.color = col;
                    changed = true;
                }
            }
        });
        ui.end_row();
        ui.label("Length");
        changed |= ui.add(egui::DragValue::new(&mut c.length).range(0.25..=4096.0).speed(0.25).suffix(" beats")).changed();
        ui.end_row();
        ui.label("Loop");
        ui.horizontal(|ui| {
            changed |= ui.checkbox(&mut c.looping, "").changed();
            if c.looping {
                ui.label(RichText::new("from").size(10.0).color(theme::DIM));
                let max = (c.length - 0.25).max(0.0);
                changed |= ui.add(egui::DragValue::new(&mut c.loop_start).range(0.0..=max).speed(0.25)).on_hover_text("Loop start (beats)").changed();
            }
        });
        ui.end_row();
        ui.label("Launch");
        egui::ComboBox::from_id_salt("launchmode").width(110.0).selected_text(format!("{:?}", c.launch_mode)).show_ui(ui, |ui| {
            for m in [LaunchMode::Trigger, LaunchMode::Gate, LaunchMode::Toggle] {
                changed |= ui.selectable_value(&mut c.launch_mode, m, format!("{m:?}")).changed();
            }
        });
        ui.end_row();
        ui.label("Quantize");
        let label = c.quantization.map(|q| q.label()).unwrap_or("Global");
        egui::ComboBox::from_id_salt("clipquant").width(110.0).selected_text(label).show_ui(ui, |ui| {
            changed |= ui.selectable_value(&mut c.quantization, None, "Global").changed();
            for q in Quantization::ALL {
                changed |= ui.selectable_value(&mut c.quantization, Some(q), q.label()).changed();
            }
        });
        ui.end_row();
        ui.label("Velocity");
        let mut pct = c.velocity_amount * 100.0;
        if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix("%").max_decimals(0)).on_hover_text("How much launch velocity scales the clip").changed() {
            c.velocity_amount = pct / 100.0;
            changed = true;
        }
        ui.end_row();
        ui.label("Groove");
        let gl = c.groove.and_then(|g| grooves.get(g).cloned()).unwrap_or_else(|| "None".into());
        egui::ComboBox::from_id_salt("clipgroove").width(110.0).selected_text(gl).show_ui(ui, |ui| {
            changed |= ui.selectable_value(&mut c.groove, None, "None").changed();
            for (i, g) in grooves.iter().enumerate() {
                changed |= ui.selectable_value(&mut c.groove, Some(i), g).changed();
            }
        });
        ui.end_row();
        if let ClipContent::Audio(a) = &mut c.content {
            ui.label("Warp");
            egui::ComboBox::from_id_salt("warp").width(110.0).selected_text(format!("{:?}", a.warp)).show_ui(ui, |ui| {
                for w in [WarpMode::Beats, WarpMode::Stretch, WarpMode::Complex, WarpMode::Repitch, WarpMode::Off] {
                    changed |= ui.selectable_value(&mut a.warp, w, format!("{w:?}")).changed();
                }
            });
            ui.end_row();
            ui.label("Orig. BPM");
            ui.horizontal(|ui| {
                changed |= ui.add(egui::DragValue::new(&mut a.original_bpm).range(20.0..=400.0).speed(0.1)).changed();
                if ui.small_button("×2").clicked() {
                    a.original_bpm *= 2.0;
                    changed = true;
                }
                if ui.small_button("÷2").clicked() {
                    a.original_bpm /= 2.0;
                    changed = true;
                }
            });
            ui.end_row();
            ui.label("Start");
            changed |= ui.add(egui::DragValue::new(&mut a.start_offset).range(0.0..=4096.0).speed(0.25).suffix(" beats")).changed();
            ui.end_row();
            ui.label("Transpose");
            changed |= ui.add(egui::DragValue::new(&mut a.transpose).range(-48.0..=48.0).speed(0.1).suffix(" st")).changed();
            ui.end_row();
            ui.label("Gain");
            changed |= ui.add(egui::DragValue::new(&mut a.gain_db).range(-40.0..=24.0).speed(0.1).suffix(" dB")).changed();
            ui.end_row();
            ui.label("Reverse");
            changed |= ui.checkbox(&mut a.reverse, "").changed();
            ui.end_row();
            ui.label("Fades");
            ui.horizontal(|ui| {
                changed |= ui.add(egui::DragValue::new(&mut a.fade_in).range(0.0..=64.0).speed(0.01).prefix("in ")).changed();
                changed |= ui.add(egui::DragValue::new(&mut a.fade_out).range(0.0..=64.0).speed(0.01).prefix("out ")).changed();
            });
            ui.end_row();
            ui.label("Markers");
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}", a.warp_markers.len())).color(theme::DIM));
                if !a.warp_markers.is_empty() && ui.small_button("Clear").clicked() {
                    a.warp_markers.clear();
                    changed = true;
                }
            });
            ui.end_row();
            ui.label("File");
            ui.label(RichText::new(&a.file).size(10.0).color(theme::DIM));
            ui.end_row();
        }
    });
    if c.groove.is_some() && ui.small_button("Commit groove to notes").clicked() {
        app.commit_groove(cref);
        return;
    }
    if let ClipRef::Session { .. } = cref {
        ui.separator();
        changed |= follow_editor(ui, &mut c.follow, "clipfollow", true);
    }
    if changed {
        c.loop_start = c.loop_start.min((c.length - 0.25).max(0.0));
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
        if let ClipRef::Session { track, slot } = cref
            && ui.small_button("Duplicate down").clicked()
        {
            app.duplicate_clip_down(track, slot);
        }
    });
}

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn piano_roll(app: &mut App, ui: &mut egui::Ui, cref: ClipRef) {
    let Some(clip) = app.clip(cref).cloned() else { return };
    let ClipContent::Midi { notes } = &clip.content else { return };
    let sel: std::collections::BTreeSet<usize> = app.selected_notes(cref).into_iter().collect();

    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Grid").color(theme::DIM));
            for (g, l) in [(1.0, "1/4"), (0.5, "1/8"), (0.25, "1/16"), (0.125, "1/32")] {
                ui.selectable_value(&mut app.piano_grid, g, l);
            }
            ui.separator();
            let grid = app.piano_grid;
            let what = if sel.is_empty() { "all notes" } else { "the selection" };
            if ui.small_button("Quantize").on_hover_text(format!("Snap {what} to the grid")).clicked() {
                app.quantize_notes(cref, grid, 1.0, false);
            }
            for (label, semis) in [("−12", -12), ("−1", -1), ("+1", 1), ("+12", 12)] {
                if ui.small_button(label).on_hover_text(format!("Transpose {what}")).clicked() {
                    app.transpose_notes(cref, semis);
                }
            }
            if ui.small_button("Duplicate loop").clicked() {
                app.duplicate_loop(cref);
            }
            ui.separator();
            ui.toggle_value(&mut app.piano_fold, "Fold").on_hover_text("Show only pitches that have notes");
            let label = match app.piano_scale {
                Some((root, k)) => format!("{} {k:?}", NOTE_NAMES[root as usize % 12]),
                None => "Scale".into(),
            };
            egui::ComboBox::from_id_salt("pianoscale").width(96.0).selected_text(label).show_ui(ui, |ui| {
                if ui.selectable_label(app.piano_scale.is_none(), "No scale").clicked() {
                    app.piano_scale = None;
                }
                let root = app.piano_scale.map_or(0, |s| s.0);
                ui.horizontal_wrapped(|ui| {
                    for (i, n) in NOTE_NAMES.iter().enumerate() {
                        if ui.selectable_label(app.piano_scale.is_some() && root == i as u8, *n).clicked() {
                            app.piano_scale = Some((i as u8, app.piano_scale.map_or(ScaleKind::Major, |s| s.1)));
                        }
                    }
                });
                ui.separator();
                for k in ScaleKind::ALL {
                    if ui.selectable_label(app.piano_scale.is_some_and(|s| s.1 == k), format!("{k:?}")).clicked() {
                        app.piano_scale = Some((root, k));
                    }
                }
            });
            // Selection details.
            if !sel.is_empty() {
                ui.separator();
                let first = *sel.iter().next().expect("non-empty");
                let n = notes[first];
                let mut vel = n.velocity;
                let mut prob = n.probability * 100.0;
                ui.label(RichText::new(format!("{} selected", sel.len())).size(11.0).color(theme::DIM));
                let a = ui.add(egui::DragValue::new(&mut vel).range(1..=127).prefix("vel "));
                let b = ui.add(egui::DragValue::new(&mut prob).range(0.0..=100.0).suffix("%").prefix("chance "));
                if a.drag_started() || b.drag_started() || ((a.changed() || b.changed()) && !a.dragged() && !b.dragged()) {
                    app.checkpoint();
                }
                if a.changed() || b.changed() {
                    app.set_selected_velocity(cref, a.changed().then_some(vel), b.changed().then_some(prob / 100.0));
                    match cref {
                        ClipRef::Session { track, slot } => app.push_clip(track, slot),
                        ClipRef::Arrangement { .. } => app.rebuild_pending = true,
                    }
                }
            }
        });

        let avail = ui.available_size();
        let vel_h = 44.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(avail.x.max(200.0), (avail.y - vel_h - 4.0).max(80.0)), Sense::click_and_drag());
        let keys_w = 34.0;
        let grid_r = Rect::from_min_max(pos2(rect.left() + keys_w, rect.top()), rect.max);

        // Visible rows: a 25-semitone window that the wheel scrolls, or with
        // Fold just the pitches in use.
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
        if resp.hovered() && !app.piano_fold {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.5 {
                lo = (lo + if scroll > 0.0 { 1 } else { -1 }).clamp(0, 127 - SPAN);
            }
        }
        app.piano_view = Some((cref, lo));
        let rows: Vec<i32> = if app.piano_fold && !notes.is_empty() {
            let set: std::collections::BTreeSet<i32> = notes.iter().map(|n| n.pitch as i32).collect();
            set.into_iter().collect()
        } else {
            (lo..lo + SPAN).collect()
        };
        let row_h = grid_r.height() / rows.len() as f32;
        let len = clip.length.max(0.25);
        let px_beat = grid_r.width() / len as f32;
        let row_of = |pitch: i32| rows.iter().position(|&p| p == pitch);
        let y_of_row = |r: usize| grid_r.bottom() - (r + 1) as f32 * row_h;
        let x_of = |beat: f64| grid_r.left() + beat as f32 * px_beat;
        let pitch_at = |y: f32| {
            let r = ((grid_r.bottom() - y) / row_h).floor() as i64;
            match rows.get(r.clamp(0, rows.len() as i64 - 1) as usize) {
                Some(&p) if r >= 0 && (r as usize) < rows.len() => p,
                // Off the top/bottom: continue the scale of rows.
                _ if r < 0 => rows[0] + r as i32,
                _ => rows[rows.len() - 1] + (r - rows.len() as i64 + 1) as i32,
            }
        };
        let beat_at = |x: f32| ((x - grid_r.left()) / px_beat) as f64;
        let in_scale = |pitch: i32| match app.piano_scale {
            Some((root, k)) => k.degrees().contains(&(((pitch - root as i32).rem_euclid(12)) as u8)),
            None => true,
        };

        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, theme::BG);
        for (r, &pitch) in rows.iter().enumerate() {
            let black = matches!(pitch % 12, 1 | 3 | 6 | 8 | 10);
            let y = y_of_row(r);
            let row = Rect::from_min_size(pos2(grid_r.left(), y), vec2(grid_r.width(), row_h));
            let fill = if app.piano_scale.is_some() {
                if in_scale(pitch) { theme::dim(theme::SLOT, 0.9) } else { theme::BG }
            } else if black {
                theme::BG
            } else {
                theme::dim(theme::SLOT, 0.8)
            };
            p.rect_filled(row, 0.0, fill);
            if app.piano_scale.is_some_and(|(root, _)| (pitch - root as i32).rem_euclid(12) == 0) {
                p.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0, theme::dim(theme::ACCENT, 0.5)));
            }
            let key = Rect::from_min_size(pos2(rect.left(), y), vec2(keys_w - 2.0, row_h - 0.5));
            p.rect_filled(key, 0.0, if black { Color32::from_gray(40) } else { Color32::from_gray(200) });
            if (pitch % 12 == 0 || app.piano_fold) && row_h >= 7.0 {
                let name = format!("{}{}", NOTE_NAMES[(pitch % 12) as usize], pitch / 12 - 2);
                p.text(key.right_center() - vec2(2.0, 0.0), Align2::RIGHT_CENTER, name, FontId::proportional(row_h.min(10.0)), if black { Color32::WHITE } else { Color32::BLACK });
            }
        }
        if clip.looping && clip.loop_start > 0.0 {
            p.rect_filled(Rect::from_min_max(grid_r.min, pos2(x_of(clip.loop_start), grid_r.bottom())), 0.0, Color32::from_black_alpha(60));
            p.line_segment([pos2(x_of(clip.loop_start), grid_r.top()), pos2(x_of(clip.loop_start), grid_r.bottom())], Stroke::new(1.5, theme::ACCENT));
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
        // Paste position.
        let cx = x_of(app.piano_cursor.min(len));
        p.line_segment([pos2(cx, grid_r.top()), pos2(cx, grid_r.top() + 6.0)], Stroke::new(2.0, theme::TEXT));
        let groove = clip.groove.and_then(|g| app.project.grooves.get(g)).cloned();

        let color = theme::rgb(clip.color);
        let note_rect = |n: &Note| row_of(n.pitch as i32).map(|r| Rect::from_min_max(pos2(x_of(n.start), y_of_row(r) + 0.5), pos2(x_of((n.start + n.length).min(len)), y_of_row(r) + row_h - 0.5)));
        for (i, n) in notes.iter().enumerate() {
            let Some(r) = note_rect(n) else { continue };
            let vel = 0.45 + 0.55 * n.velocity as f32 / 127.0;
            let mut fill = theme::dim(color, vel);
            if n.probability < 1.0 {
                fill = fill.gamma_multiply(0.5 + 0.5 * n.probability);
            }
            let s = sel.contains(&i);
            if s {
                fill = Color32::from_rgb(fill.r().saturating_add(60), fill.g().saturating_add(60), fill.b().saturating_add(60));
            }
            p.rect_filled(r, 2.0, fill);
            p.rect_stroke(r, 2.0, Stroke::new(if s { 1.5 } else { 0.5 }, if s { Color32::WHITE } else { Color32::BLACK }), StrokeKind::Inside);
            if !in_scale(n.pitch as i32) {
                p.circle_filled(r.left_center() + vec2(3.0, 0.0), 1.5, theme::REC);
            }
            if let Some(g) = &groove {
                let (off, _) = g.apply(n.start);
                if off.abs() > 1e-4 {
                    let x = x_of(n.start + off * g.amount as f64);
                    p.line_segment([pos2(x, r.top()), pos2(x, r.bottom())], Stroke::new(1.0, theme::ACCENT));
                }
            }
        }

        let pos = match cref {
            ClipRef::Session { track, slot } => app.engine.state.tracks.get(track).filter(|ts| ts.playing_slot.load(Relaxed) == slot as i32).map(|ts| ts.clip_position.load()),
            ClipRef::Arrangement { track, index } => {
                app.engine.state.tracks.get(track).filter(|ts| ts.arrangement_clip.load(Relaxed) == index as i32 && ts.playing_slot.load(Relaxed) < 0).map(|ts| ts.clip_position.load())
            }
        };
        if let Some(cp) = pos {
            let x = x_of(cp);
            p.line_segment([pos2(x, grid_r.top()), pos2(x, grid_r.bottom())], Stroke::new(1.5, theme::ACCENT));
        }

        // ---- Interaction ----
        let hit = |pos: egui::Pos2| -> Option<(usize, bool)> {
            notes.iter().enumerate().rev().find_map(|(i, n)| {
                let r = note_rect(n)?;
                r.expand2(vec2(0.0, 0.5)).contains(pos).then(|| (i, (r.right() - pos.x) < 6.0 && r.width() > 8.0))
            })
        };
        let snap = |b: f64, g: f64| (b / g).floor() * g;
        let grid = app.piano_grid;
        let mods = ui.input(|i| i.modifiers);
        if let Some((_, true)) = resp.hover_pos().and_then(hit) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        if resp.double_clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            match hit(pos) {
                Some((i, _)) => {
                    app.checkpoint();
                    edit_notes(app, cref, |ns| {
                        ns.remove(i);
                    });
                    app.piano_sel.clear();
                    app.commit_clip(cref);
                }
                None if grid_r.contains(pos) => add_note(app, cref, snap(beat_at(pos.x), grid), pitch_at(pos.y), grid, len),
                None => {}
            }
            return;
        }
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            app.piano_cursor = snap(beat_at(pos.x), grid).clamp(0.0, len);
            match hit(pos) {
                Some((i, _)) if mods.shift || mods.command => {
                    if !app.piano_sel.remove(&i) {
                        app.piano_sel.insert(i);
                    }
                }
                Some((i, _)) => app.piano_sel = [i].into_iter().collect(),
                // A click on empty grid clears the selection; with nothing selected it draws a note.
                None if grid_r.contains(pos) && !sel.is_empty() => app.piano_sel.clear(),
                None if grid_r.contains(pos) => add_note(app, cref, snap(beat_at(pos.x), grid), pitch_at(pos.y), grid, len),
                None => {}
            }
        }
        if resp.drag_started()
            && let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
        {
            // The press point, not where the pointer is once the drag registers.
            let (b0, p0) = (beat_at(pos.x), pitch_at(pos.y));
            match hit(pos) {
                Some((i, resize)) => {
                    if !app.piano_sel.contains(&i) {
                        if !(mods.shift || mods.command) {
                            app.piano_sel.clear();
                        }
                        app.piano_sel.insert(i);
                    }
                    app.checkpoint();
                    let origs = app.selected_notes(cref).into_iter().filter_map(|j| notes.get(j).map(|n| (j, *n))).collect();
                    let mode = if resize { PianoDragMode::Resize } else { PianoDragMode::Move };
                    app.piano_drag = Some(PianoDrag { mode, origs, beat0: b0, pitch0: p0 });
                }
                None => {
                    app.piano_drag = Some(PianoDrag { mode: PianoDragMode::Marquee { additive: mods.shift || mods.command }, origs: Vec::new(), beat0: b0, pitch0: p0 });
                }
            }
        }
        if let (Some(d), Some(pos)) = (app.piano_drag.clone(), resp.interact_pointer_pos())
            && resp.dragged()
        {
            match d.mode {
                PianoDragMode::Marquee { additive } => {
                    let start_y = rows.iter().position(|&q| q == d.pitch0).map_or(pos.y, |r| y_of_row(r) + row_h * 0.5);
                    let r = Rect::from_two_pos(pos2(x_of(d.beat0), start_y), pos).intersect(grid_r);
                    p.rect_filled(r, 0.0, theme::ACCENT.gamma_multiply(0.12));
                    p.rect_stroke(r, 0.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
                    app.select_notes_in(cref, beat_at(r.left()), beat_at(r.right()), pitch_at(r.bottom() - 0.5), pitch_at(r.top() + 0.5), additive);
                }
                _ => {
                    // Alt disables grid snapping, like most piano rolls.
                    let raw = beat_at(pos.x) - d.beat0;
                    let db = if mods.alt { raw } else { (raw / grid).round() * grid };
                    let dp = pitch_at(pos.y) - d.pitch0;
                    app.apply_piano_drag(cref, &d, db, if matches!(d.mode, PianoDragMode::Move) { dp } else { 0 }, grid, len);
                }
            }
        }
        if resp.drag_stopped() && app.piano_drag.is_some() {
            let d = app.piano_drag.take();
            if !matches!(d.map(|d| d.mode), Some(PianoDragMode::Marquee { .. })) {
                app.commit_clip(cref);
            }
        }

        // Velocity lane: drag over notes to set velocity. With a selection,
        // only selected notes change.
        let (vr, vresp) = ui.allocate_exact_size(vec2(rect.width(), vel_h), Sense::click_and_drag());
        let vp = ui.painter_at(vr);
        vp.rect_filled(vr, 0.0, theme::BG);
        vp.text(vr.left_top() + vec2(3.0, 2.0), Align2::LEFT_TOP, "vel", FontId::proportional(9.0), theme::DIM);
        let lane = Rect::from_min_max(pos2(vr.left() + keys_w, vr.top() + 2.0), vr.max - vec2(0.0, 2.0));
        for (i, n) in notes.iter().enumerate() {
            let x = x_of(n.start);
            let h = n.velocity as f32 / 127.0 * lane.height();
            let s = sel.contains(&i);
            vp.line_segment([pos2(x + 1.0, lane.bottom()), pos2(x + 1.0, lane.bottom() - h)], Stroke::new(3.0, if s { Color32::WHITE } else { theme::dim(color, 0.9) }));
        }
        if (vresp.dragged() || vresp.clicked())
            && let Some(pos) = vresp.interact_pointer_pos()
        {
            let b = beat_at(pos.x);
            let v = (((lane.bottom() - pos.y) / lane.height()).clamp(0.0, 1.0) * 126.0 + 1.0).round() as u8;
            let tol = (4.0 / px_beat) as f64;
            if vresp.drag_started() || vresp.clicked() {
                app.checkpoint();
            }
            let only = sel.clone();
            edit_notes(app, cref, |ns| {
                for (i, n) in ns.iter_mut().enumerate() {
                    if (n.start - b).abs() <= tol && (only.is_empty() || only.contains(&i)) {
                        n.velocity = v;
                    }
                }
            });
            app.dirty = true;
        }
        if vresp.drag_stopped() || vresp.clicked() {
            app.commit_clip(cref);
        }
    });
}

fn add_note(app: &mut App, cref: ClipRef, start: f64, pitch: i32, grid: f64, len: f64) {
    if start >= len || start < 0.0 {
        return;
    }
    let pitch = pitch.clamp(0, 127) as u8;
    let pitch = match app.piano_scale {
        Some((root, k)) => k.snap(root, pitch),
        None => pitch,
    };
    app.checkpoint();
    let mut idx = 0;
    edit_notes(app, cref, |ns| {
        ns.push(Note::new(pitch, start, grid, 100));
        idx = ns.len() - 1;
    });
    app.piano_sel = [idx].into_iter().collect();
    app.piano_cursor = start + grid;
    app.commit_clip(cref);
}

fn edit_notes(app: &mut App, cref: ClipRef, f: impl FnOnce(&mut Vec<Note>)) {
    if let Some(ClipContent::Midi { notes }) = app.clip_mut(cref).map(|c| &mut c.content) {
        f(notes);
    }
}

fn waveform(app: &mut App, ui: &mut egui::Ui, cref: ClipRef) {
    let Some(clip) = app.clip(cref).cloned() else { return };
    let ClipContent::Audio(a) = &clip.content else { return };
    ui.vertical(|ui| {
        ui.label(
            RichText::new("Double-click the marker strip to add a warp marker · drag markers to warp · right-click to remove").size(10.0).color(theme::DIM),
        );
        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(vec2(avail.x.max(200.0), avail.y.max(80.0)), Sense::click_and_drag());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, theme::BG);
        let Some(buf) = loopcraft_dsp::SampleLoader::load(app.loader.as_ref(), &a.file) else {
            p.text(rect.center(), Align2::CENTER_CENTER, format!("Missing file: {}", a.file), FontId::proportional(12.0), theme::REC);
            return;
        };
        let strip = Rect::from_min_size(rect.min, vec2(rect.width(), 14.0));
        let wave = Rect::from_min_max(pos2(rect.left(), strip.bottom()), rect.max);
        p.rect_filled(strip, 0.0, theme::PANEL);
        let len = clip.length.max(1e-6);
        let x_of = |beat: f64| rect.left() + (beat / len) as f32 * rect.width();
        let beat_of = |x: f32| ((x - rect.left()) / rect.width()) as f64 * len;
        let color = theme::rgb(clip.color);
        let mid = wave.center().y;
        let w = rect.width().max(1.0) as usize;
        let sr = buf.sample_rate as f64;
        let ch = &buf.channels[0];
        for x in 0..w {
            let b0 = len * x as f64 / w as f64;
            let b1 = len * (x + 1) as f64 / w as f64;
            let (mut s0, mut s1) = ((a.seconds_at(b0) * sr) as isize, (a.seconds_at(b1) * sr) as isize);
            if a.reverse {
                let n = ch.len() as isize;
                (s0, s1) = (n - s1, n - s0);
            }
            let (s0, s1) = (s0.max(0) as usize, (s1.max(0) as usize).max(s0.max(0) as usize + 1));
            let (mut mn, mut mx) = (0.0f32, 0.0f32);
            for s in ch.iter().take(s1.min(ch.len())).skip(s0.min(ch.len())).step_by(((s1 - s0) / 64).max(1)) {
                mn = mn.min(*s);
                mx = mx.max(*s);
            }
            let xf = rect.left() + x as f32;
            p.line_segment([pos2(xf, mid - mx * wave.height() * 0.48), pos2(xf, mid - mn * wave.height() * 0.48)], Stroke::new(1.0, color));
        }
        for b in 0..=(clip.length.ceil() as usize) {
            let x = x_of(b as f64);
            p.line_segment([pos2(x, wave.top()), pos2(x, wave.top() + if b % 4 == 0 { 12.0 } else { 6.0 })], Stroke::new(1.0, theme::DIM));
        }
        // Fades.
        if a.fade_in > 0.0 {
            p.line_segment([pos2(x_of(0.0), wave.bottom()), pos2(x_of(a.fade_in), wave.top())], Stroke::new(1.0, theme::ACCENT));
        }
        if a.fade_out > 0.0 {
            p.line_segment([pos2(x_of(len - a.fade_out), wave.top()), pos2(x_of(len), wave.bottom())], Stroke::new(1.0, theme::ACCENT));
        }
        // Warp markers.
        let mut hit = None;
        for (i, m) in a.warp_markers.iter().enumerate() {
            let x = x_of(m.beat);
            let press = if resp.drag_started() { ui.input(|i| i.pointer.press_origin()) } else { None };
            let near = press.is_some_and(|q| (q.x - x).abs() < 6.0 && q.y < strip.bottom() + 4.0)
                || resp.hover_pos().is_some_and(|q| (q.x - x).abs() < 6.0 && q.y < strip.bottom() + 4.0)
                || resp.interact_pointer_pos().is_some_and(|q| (q.x - x).abs() < 6.0 && q.y < strip.bottom() + 4.0);
            if near {
                hit = Some(i);
            }
            p.add(egui::Shape::convex_polygon(
                vec![pos2(x - 5.0, strip.top()), pos2(x + 5.0, strip.top()), pos2(x, strip.bottom())],
                if near { Color32::WHITE } else { theme::ACCENT },
                Stroke::NONE,
            ));
            p.line_segment([pos2(x, strip.bottom()), pos2(x, wave.bottom())], Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.5)));
        }
        let in_strip = |q: egui::Pos2| q.y < strip.bottom() + 4.0;
        if resp.double_clicked()
            && hit.is_none()
            && let Some(q) = resp.interact_pointer_pos().filter(|q| in_strip(*q))
        {
            let b = (beat_of(q.x) / 0.25).round() * 0.25;
            app.checkpoint();
            if let Some(ClipContent::Audio(ac)) = app.clip_mut(cref).map(|c| &mut c.content) {
                if ac.warp_markers.is_empty() {
                    // Pin the start and end too, so one marker warps only its neighbours.
                    let end = ac.seconds_at(len);
                    ac.warp_markers.push(WarpMarker { beat: 0.0, seconds: ac.seconds_at(0.0) });
                    ac.warp_markers.push(WarpMarker { beat: len, seconds: end });
                }
                let s = ac.seconds_at(b);
                ac.warp_markers.retain(|m| (m.beat - b).abs() > 1e-6);
                ac.warp_markers.push(WarpMarker { beat: b, seconds: s });
                ac.warp_markers.sort_by(|x, y| x.beat.total_cmp(&y.beat));
            }
            app.commit_clip(cref);
        }
        if resp.drag_started() {
            app.warp_drag = hit;
            if hit.is_some() {
                app.checkpoint();
            }
        }
        if let (Some(i), Some(q)) = (app.warp_drag, resp.interact_pointer_pos())
            && resp.dragged()
        {
            let b = (beat_of(q.x) / 0.0625).round() * 0.0625;
            if let Some(ClipContent::Audio(ac)) = app.clip_mut(cref).map(|c| &mut c.content) {
                let lo = if i > 0 { ac.warp_markers[i - 1].beat + 0.0625 } else { -64.0 };
                let hi = ac.warp_markers.get(i + 1).map_or(4096.0, |m| m.beat - 0.0625);
                if let Some(m) = ac.warp_markers.get_mut(i) {
                    m.beat = b.clamp(lo, hi.max(lo));
                }
            }
            app.dirty = true;
        }
        if resp.drag_stopped() && app.warp_drag.is_some() {
            app.warp_drag = None;
            app.commit_clip(cref);
        }
        if resp.secondary_clicked()
            && let Some(i) = hit
        {
            app.checkpoint();
            if let Some(ClipContent::Audio(ac)) = app.clip_mut(cref).map(|c| &mut c.content) {
                ac.warp_markers.remove(i);
                if ac.warp_markers.len() < 2 {
                    ac.warp_markers.clear();
                }
            }
            app.commit_clip(cref);
        }
        let pos = match cref {
            ClipRef::Session { track, slot } => {
                app.engine.state.tracks.get(track).filter(|ts| ts.playing_slot.load(Relaxed) == slot as i32).map(|ts| ts.clip_position.load())
            }
            ClipRef::Arrangement { track, index } => {
                app.engine.state.tracks.get(track).filter(|ts| ts.arrangement_clip.load(Relaxed) == index as i32).map(|ts| ts.clip_position.load())
            }
        };
        if let Some(cp) = pos {
            let x = x_of(cp);
            p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, theme::ACCENT));
        }
        p.text(
            rect.left_bottom() + vec2(6.0, -6.0),
            Align2::LEFT_BOTTOM,
            format!("{:?} · {:.1} → {:.1} bpm · {:.2}s", a.warp, a.original_bpm, app.engine.state.tempo.load(), buf.len() as f64 / sr),
            FontId::proportional(11.0),
            theme::DIM,
        );
    });
}
