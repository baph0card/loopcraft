//! Session view: a grid of clip slots (tracks x scenes) with a mixer below.

use super::{Icon, icon_button, knob, mappable, meter, small_toggle, theme};
use crate::app::{App, ClipRef, DetailTab, Rename};
use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Stroke, StrokeKind, vec2};
use loopcraft_core::*;
use loopcraft_engine::{Command, EngineState, TrackRef};
use std::sync::atomic::Ordering::Relaxed;

const COL_W: f32 = 112.0;
const SLOT_H: f32 = 22.0;
const HEADER_H: f32 = 22.0;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let st = app.engine.state.clone();
    let blink = (ui.input(|i| i.time) * 4.0) as i64 % 2 == 0;
    // Release Gate-mode clips when the mouse button comes up.
    if let Some((t, s)) = app.gate_held
        && ui.input(|i| i.pointer.any_released())
    {
        app.send(Command::ReleaseClip { track: t, slot: s });
        app.gate_held = None;
    }
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for t in 0..app.project.tracks.len() {
                if t >= app.project.tracks.len() || app.hidden(t) {
                    continue;
                }
                track_column(app, ui, t, &st, blink);
            }
            if app.project.tracks.is_empty() {
                ui.vertical(|ui| {
                    ui.set_width(220.0);
                    ui.add_space(40.0);
                    ui.label(RichText::new("No tracks yet.").color(theme::DIM));
                    ui.label(RichText::new("Add one from the browser on the left.").color(theme::DIM));
                });
            }
            for r in 0..app.project.returns.len() {
                if r < app.project.returns.len() {
                    return_column(app, ui, r, &st);
                }
            }
            master_column(app, ui, &st, blink);
        });
    });
}

fn rename_field(app: &mut App, ui: &mut egui::Ui, target: Rename) -> bool {
    let Some((cur, text)) = app.renaming.as_mut() else { return false };
    if *cur != target {
        return false;
    }
    let r = ui.add(egui::TextEdit::singleline(text).desired_width(COL_W - 8.0));
    r.request_focus();
    if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        let (target, text) = app.renaming.take().expect("renaming");
        app.checkpoint();
        match target {
            Rename::Track(t) => {
                if let Some(t) = app.project.tracks.get_mut(t) {
                    t.name = text
                }
            }
            Rename::Scene(s) => {
                if let Some(s) = app.project.scenes.get_mut(s) {
                    s.name = text
                }
            }
            Rename::Return(r) => {
                if let Some(r) = app.project.returns.get_mut(r) {
                    r.name = text
                }
            }
        }
    }
    true
}

fn header(ui: &mut egui::Ui, label: &str, color: Color32, selected: bool, indent: usize, fold: Option<bool>) -> (egui::Response, Option<egui::Response>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(COL_W, HEADER_H), Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, if selected { color } else { theme::dim(color, 0.75) });
    // Group nesting marks.
    for d in 0..indent {
        p.rect_filled(egui::Rect::from_min_size(rect.min + vec2(2.0 + d as f32 * 4.0, 2.0), vec2(2.0, HEADER_H - 4.0)), 0.0, Color32::from_black_alpha(120));
    }
    let x0 = 6.0 + indent as f32 * 4.0 + if fold.is_some() { 14.0 } else { 0.0 };
    p.text(rect.left_center() + vec2(x0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(12.0), Color32::BLACK);
    if selected {
        p.rect_stroke(rect, 2.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
    }
    let fold_resp = fold.map(|open| {
        let r = egui::Rect::from_min_size(rect.min + vec2(2.0 + indent as f32 * 4.0, 3.0), vec2(14.0, HEADER_H - 6.0));
        let fr = ui.interact(r, ui.id().with(("fold", label, rect.min.x as i32)), Sense::click());
        let c = r.center();
        let s = 4.0;
        let pts = if open { vec![c + vec2(-s, -s * 0.5), c + vec2(s, -s * 0.5), c + vec2(0.0, s * 0.7)] } else { vec![c + vec2(-s * 0.5, -s), c + vec2(-s * 0.5, s), c + vec2(s * 0.7, 0.0)] };
        ui.painter().add(egui::Shape::convex_polygon(pts, Color32::BLACK, Stroke::NONE));
        fr
    });
    (resp, fold_resp)
}

fn select_track(app: &mut App, ui: &egui::Ui, t: usize) {
    let multi = ui.input(|i| i.modifiers.command || i.modifiers.shift);
    if multi {
        if let Some(i) = app.selected_tracks.iter().position(|&x| x == t) {
            app.selected_tracks.remove(i);
        } else {
            if app.selected_tracks.is_empty()
                && let Some(cur) = app.selected_track
            {
                app.selected_tracks.push(cur);
            }
            app.selected_tracks.push(t);
        }
    } else {
        app.selected_tracks.clear();
    }
    app.selected_track = Some(t);
    app.device_target = TrackRef::Track(t);
}

fn track_menu(app: &mut App, ui: &mut egui::Ui, t: usize) {
    let name = app.project.tracks[t].name.clone();
    let kind = app.project.tracks[t].kind;
    if ui.button("Rename").clicked() {
        app.renaming = Some((Rename::Track(t), name));
        ui.close();
    }
    if ui.button("Duplicate track").clicked() {
        app.duplicate_track(t);
        ui.close();
    }
    if ui.button("Delete track").clicked() {
        app.delete_track(t);
        ui.close();
        return;
    }
    ui.horizontal(|ui| {
        if ui.button("◀ Move").clicked() {
            app.move_track(t, true);
            ui.close();
        }
        if ui.button("Move ▶").clicked() {
            app.move_track(t, false);
            ui.close();
        }
    });
    ui.separator();
    if ui.button("Group selected tracks  (Ctrl+G)").clicked() {
        if app.selected_tracks.is_empty() {
            app.selected_tracks = vec![t];
        }
        app.group_selected();
        ui.close();
    }
    if kind == TrackKind::Group && ui.button("Ungroup").clicked() {
        app.ungroup(t);
        ui.close();
    }
    ui.separator();
    ui.horizontal(|ui| {
        for c in App::PALETTE {
            let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
            ui.painter().rect_filled(r, 2.0, theme::rgb(c));
            if resp.clicked() {
                app.checkpoint();
                if let Some(tr) = app.project.tracks.get_mut(t) {
                    tr.color = c;
                }
                ui.close();
            }
        }
    });
}

fn track_column(app: &mut App, ui: &mut egui::Ui, t: usize, st: &EngineState, blink: bool) {
    let Some(ts) = st.tracks.get(t) else { return };
    let track = &app.project.tracks[t];
    let color = theme::rgb(track.color);
    let name = track.name.clone();
    let kind = track.kind;
    let folded = track.folded;
    let armed = track.armed;
    let indent = app.project.depth(t);
    let playing_slot = ts.playing_slot.load(Relaxed);
    let queued_slot = ts.queued_slot.load(Relaxed);
    let stop_queued = ts.stop_queued.load(Relaxed);
    let rec_slot = ts.recording_slot.load(Relaxed);
    let clip_pos = ts.clip_position.load();
    let has_clips = track.slots.iter().any(Option::is_some);
    let selected = app.selected_track == Some(t) || app.selected_tracks.contains(&t);

    ui.vertical(|ui| {
        ui.set_width(COL_W);
        if !rename_field(app, ui, Rename::Track(t)) {
            let fold = (kind == TrackKind::Group).then_some(!folded);
            let (resp, fold_resp) = header(ui, &name, color, selected, indent, fold);
            if fold_resp.is_some_and(|f| f.clicked()) {
                app.project.tracks[t].folded = !folded;
            } else if resp.clicked() {
                select_track(app, ui, t);
            }
            if resp.double_clicked() {
                app.renaming = Some((Rename::Track(t), name.clone()));
            }
            resp.context_menu(|ui| track_menu(app, ui, t));
        }
        if t >= app.project.tracks.len() {
            return;
        }

        for s in 0..app.project.scenes.len() {
            let (rect, resp) = ui.allocate_exact_size(vec2(COL_W, SLOT_H), Sense::click());
            let inner = rect.shrink2(vec2(1.0, 1.0));
            let is_sel = app.selected_clip == Some(ClipRef::Session { track: t, slot: s });
            let scene_sel = app.selected_scene == s;
            let p = ui.painter().clone();
            let mut filled = false;
            let mut gate = false;
            if kind == TrackKind::Group {
                // Group slot: shows whether any child has a clip in this row.
                let children: Vec<usize> = (0..app.project.tracks.len()).filter(|&c| app.project.is_inside(c, t)).collect();
                let any = children.iter().any(|&c| app.project.tracks[c].slots.get(s).is_some_and(Option::is_some));
                let any_playing = children.iter().any(|&c| st.tracks.get(c).is_some_and(|x| x.playing_slot.load(Relaxed) == s as i32));
                filled = any;
                p.rect_filled(inner, 2.0, if any { if any_playing { theme::dim(color, 0.9) } else { theme::dim(color, 0.45) } } else { theme::SLOT });
                if any {
                    let c0 = inner.left_center() + vec2(8.0, 0.0);
                    p.add(egui::Shape::convex_polygon(vec![c0 + vec2(-4.0, -5.0), c0 + vec2(-4.0, 5.0), c0 + vec2(4.0, 0.0)], if any_playing { theme::PLAY } else { Color32::from_black_alpha(160) }, Stroke::NONE));
                }
            } else {
                let clip = app.project.tracks[t].slots.get(s).and_then(|c| c.as_ref());
                match clip {
                    Some(c) => {
                        filled = true;
                        gate = c.launch_mode == LaunchMode::Gate;
                        let is_playing = playing_slot == s as i32;
                        let is_queued = queued_slot == s as i32;
                        let base = theme::rgb(c.color);
                        p.rect_filled(inner, 2.0, if is_playing { base } else { theme::dim(base, 0.6) });
                        let tri_color = if rec_slot == s as i32 {
                            if blink { theme::REC } else { theme::dim(theme::REC, 0.5) }
                        } else if is_playing || (is_queued && blink) {
                            theme::PLAY
                        } else {
                            Color32::from_black_alpha(160)
                        };
                        let c0 = inner.left_center() + vec2(8.0, 0.0);
                        if rec_slot == s as i32 {
                            p.circle_filled(c0, 5.0, tri_color);
                        } else {
                            p.add(egui::Shape::convex_polygon(vec![c0 + vec2(-4.0, -5.0), c0 + vec2(-4.0, 5.0), c0 + vec2(4.0, 0.0)], tri_color, Stroke::NONE));
                        }
                        let mut label = match &c.content {
                            ClipContent::Audio(_) => format!("♪ {}", c.name),
                            _ => c.name.clone(),
                        };
                        if c.follow.is_some_and(|f| f.enabled) {
                            label.push_str(" →");
                        }
                        p.text(inner.left_center() + vec2(16.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(11.0), Color32::BLACK);
                        if is_playing {
                            let frac = (clip_pos / c.length.max(1e-6)).clamp(0.0, 1.0) as f32;
                            let bar = egui::Rect::from_min_size(egui::pos2(inner.left(), inner.bottom() - 3.0), vec2(inner.width() * frac, 3.0));
                            p.rect_filled(bar, 0.0, Color32::from_black_alpha(140));
                        }
                    }
                    None => {
                        let rec_here = rec_slot == s as i32;
                        p.rect_filled(inner, 2.0, if resp.hovered() { theme::SLOT_HOVER } else if scene_sel { theme::dim(theme::SLOT_HOVER, 0.9) } else { theme::SLOT });
                        if rec_here {
                            p.circle_filled(inner.center(), 5.0, if blink { theme::REC } else { theme::dim(theme::REC, 0.5) });
                        } else if armed {
                            p.circle_stroke(inner.center(), 4.5, Stroke::new(1.5, theme::dim(theme::REC, 0.8)));
                        } else if has_clips {
                            let sq = egui::Rect::from_center_size(inner.center(), vec2(7.0, 7.0));
                            p.rect_filled(sq, 0.0, theme::LINE);
                        }
                    }
                }
            }
            if is_sel {
                p.rect_stroke(inner, 2.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
            }

            if resp.clicked() {
                app.selected_track = Some(t);
                app.selected_scene = s;
                app.device_target = TrackRef::Track(t);
                app.selected_clip = Some(ClipRef::Session { track: t, slot: s });
                app.piano_selected = None;
                if filled {
                    app.detail_tab = DetailTab::Clip;
                }
                if !filled && armed && kind != TrackKind::Group {
                    app.record_slot(t, s);
                } else {
                    app.send(Command::LaunchClip { track: t, slot: s });
                    if gate {
                        app.gate_held = Some((t, s));
                    }
                }
            }
            if resp.double_clicked() && !filled && kind == TrackKind::Midi {
                app.new_midi_clip(t, s);
            }
            resp.context_menu(|ui| slot_menu(app, ui, t, s, filled, kind));
            mappable(app, ui, rect, MappingTarget::LaunchClip { track: t, slot: s });
        }

        // Track stop button.
        let r = icon_button(ui, Icon::Stop, stop_queued && blink, theme::ACCENT, vec2(COL_W, 18.0)).on_hover_text("Stop this track");
        if r.clicked() {
            app.send(Command::StopTrack { track: t });
        }
        mappable(app, ui, r.rect, MappingTarget::StopTrack(t));
        ui.add_space(6.0);
        mixer_strip(app, ui, TrackRef::Track(t), st);
    });
}

fn slot_menu(app: &mut App, ui: &mut egui::Ui, t: usize, s: usize, filled: bool, kind: TrackKind) {
    let r = ClipRef::Session { track: t, slot: s };
    if kind == TrackKind::Group {
        ui.label(RichText::new("Launches the row's clips in this group").color(theme::DIM));
        return;
    }
    if filled {
        if ui.button("Copy  (Ctrl+C)").clicked() {
            app.clipboard = app.clip(r).cloned();
            ui.close();
        }
        if ui.button("Duplicate to next slot  (Ctrl+D)").clicked() {
            app.duplicate_clip_down(t, s);
            ui.close();
        }
        if ui.button("Copy to Arrangement at playhead").clicked() {
            app.clip_to_arrangement(t, s);
            ui.close();
        }
        if ui.button("Delete clip").clicked() {
            app.delete_clip(r);
            ui.close();
        }
    } else {
        if kind == TrackKind::Midi && ui.button("Insert MIDI clip").clicked() {
            app.new_midi_clip(t, s);
            ui.close();
        }
        if ui.add_enabled(app.clipboard.is_some(), egui::Button::new("Paste  (Ctrl+V)")).clicked()
            && let Some(c) = app.clipboard.clone()
        {
            app.paste_clip(t, s, c);
            ui.close();
        }
        if ui.button("Record into this slot").clicked() {
            if !app.project.tracks[t].armed {
                app.set_arm(t, true, true);
            }
            app.record_slot(t, s);
            ui.close();
        }
        if kind == TrackKind::Audio {
            ui.label(RichText::new("Add audio from the Samples browser").color(theme::DIM));
        }
    }
}

fn return_column(app: &mut App, ui: &mut egui::Ui, r: usize, st: &EngineState) {
    ui.vertical(|ui| {
        ui.set_width(COL_W);
        let name = app.project.returns[r].name.clone();
        if !rename_field(app, ui, Rename::Return(r)) {
            let (resp, _) = header(ui, &name, theme::DIM, app.device_target == TrackRef::Return(r), 0, None);
            if resp.clicked() {
                app.device_target = TrackRef::Return(r);
                app.detail_tab = DetailTab::Devices;
            }
            if resp.double_clicked() {
                app.renaming = Some((Rename::Return(r), name.clone()));
            }
            resp.context_menu(|ui| {
                if ui.button("Rename").clicked() {
                    app.renaming = Some((Rename::Return(r), name.clone()));
                    ui.close();
                }
                if ui.button("Delete return").clicked() {
                    app.delete_return(r);
                    ui.close();
                }
            });
        }
        if r >= app.project.returns.len() {
            return;
        }
        let h = app.project.scenes.len() as f32 * (SLOT_H + ui.spacing().item_spacing.y) + 18.0;
        let (rect, _) = ui.allocate_exact_size(vec2(COL_W, h), Sense::hover());
        let fx: Vec<&str> = app.project.returns[r].effects.iter().map(|d| d.name()).collect();
        ui.painter().text(rect.center_top() + vec2(0.0, 12.0), Align2::CENTER_CENTER, fx.join(", "), FontId::proportional(10.0), theme::DIM);
        ui.add_space(6.0);
        mixer_strip(app, ui, TrackRef::Return(r), st);
    });
}

fn scene_menu(app: &mut App, ui: &mut egui::Ui, s: usize) {
    let name = app.project.scenes[s].name.clone();
    if ui.button("Rename").clicked() {
        app.renaming = Some((Rename::Scene(s), name));
        ui.close();
    }
    let mut has_tempo = app.project.scenes[s].tempo.is_some();
    if ui.checkbox(&mut has_tempo, "Scene sets tempo").changed() {
        app.checkpoint();
        app.project.scenes[s].tempo = has_tempo.then_some(app.project.tempo);
        app.rebuild();
    }
    if let Some(mut bpm) = app.project.scenes[s].tempo
        && ui.add(egui::DragValue::new(&mut bpm).range(20.0..=999.0).speed(0.1).suffix(" bpm")).changed()
    {
        app.project.scenes[s].tempo = Some(bpm);
        app.dirty = true;
        app.rebuild_pending = true;
    }
    let mut has_sig = app.project.scenes[s].time_signature.is_some();
    if ui.checkbox(&mut has_sig, "Scene sets time signature").changed() {
        app.checkpoint();
        app.project.scenes[s].time_signature = has_sig.then_some(app.project.time_signature);
        app.rebuild();
    }
    if let Some(mut sig) = app.project.scenes[s].time_signature {
        ui.horizontal(|ui| {
            let a = ui.add(egui::DragValue::new(&mut sig.numerator).range(1..=32));
            ui.label("/");
            let b = ui.add(egui::DragValue::new(&mut sig.denominator).range(1..=32));
            if (a.changed() || b.changed()) && [1, 2, 4, 8, 16, 32].contains(&sig.denominator) {
                app.project.scenes[s].time_signature = Some(sig);
                app.dirty = true;
                app.rebuild_pending = true;
            }
        });
    }
    ui.separator();
    let mut fa = app.project.scenes[s].follow;
    if follow_editor(ui, &mut fa, ("scenefollow", s), false) {
        app.checkpoint();
        app.project.scenes[s].follow = fa;
        app.rebuild();
    }
    ui.separator();
    ui.horizontal(|ui| {
        let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
        ui.painter().rect_stroke(r, 2.0, Stroke::new(1.0, theme::DIM), StrokeKind::Inside);
        if resp.on_hover_text("No color").clicked() {
            app.project.scenes[s].color = None;
        }
        for c in App::PALETTE {
            let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
            ui.painter().rect_filled(r, 2.0, theme::rgb(c));
            if resp.clicked() {
                app.project.scenes[s].color = Some(c);
                app.dirty = true;
            }
        }
    });
    ui.separator();
    if ui.button("Insert scene above").clicked() {
        app.insert_scene(s);
        ui.close();
    }
    if ui.button("Duplicate scene").clicked() {
        app.duplicate_scene(s);
        ui.close();
    }
    if ui.button("Delete scene").clicked() {
        app.delete_scene(s);
        ui.close();
    }
}

/// Follow-action editor shared by clips and scenes. Returns true if changed.
pub fn follow_editor(ui: &mut egui::Ui, fa: &mut Option<FollowAction>, id: impl std::hash::Hash + Copy + std::fmt::Debug, clip: bool) -> bool {
    let mut changed = false;
    let mut on = fa.is_some_and(|f| f.enabled);
    if ui.checkbox(&mut on, "Follow action").changed() {
        changed = true;
        match fa {
            Some(f) => f.enabled = on,
            None if on => *fa = Some(FollowAction::default()),
            None => {}
        }
    }
    let Some(f) = fa.as_mut().filter(|f| f.enabled) else { return changed };
    egui::Grid::new(("fa", id)).num_columns(2).spacing([6.0, 3.0]).show(ui, |ui| {
        if clip {
            ui.label("Linked");
            changed |= ui.checkbox(&mut f.linked, "").on_hover_text("Fire at the end of each clip loop").changed();
            ui.end_row();
        }
        if !f.linked || !clip {
            ui.label("After");
            changed |= ui.add(egui::DragValue::new(&mut f.after).range(0.25..=1024.0).speed(0.25).suffix(" beats")).changed();
            ui.end_row();
        }
        for (label, act) in [("A", &mut f.action_a), ("B", &mut f.action_b)] {
            ui.label(label);
            changed |= follow_kind_combo(ui, act, (id, label));
            ui.end_row();
        }
        ui.label("Chance A");
        let mut pct = f.chance_a * 100.0;
        if ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix("%").max_decimals(0)).changed() {
            f.chance_a = pct / 100.0;
            changed = true;
        }
        ui.end_row();
    });
    changed
}

fn follow_kind_combo(ui: &mut egui::Ui, k: &mut FollowKind, id: impl std::hash::Hash + std::fmt::Debug) -> bool {
    let mut changed = false;
    let label = match *k {
        FollowKind::Jump(i) => format!("Jump {}", i + 1),
        other => format!("{other:?}"),
    };
    egui::ComboBox::from_id_salt(id).width(90.0).selected_text(label).show_ui(ui, |ui| {
        for opt in [
            FollowKind::NoAction,
            FollowKind::Stop,
            FollowKind::Again,
            FollowKind::Previous,
            FollowKind::Next,
            FollowKind::First,
            FollowKind::Last,
            FollowKind::Any,
            FollowKind::Other,
        ] {
            changed |= ui.selectable_value(k, opt, format!("{opt:?}")).changed();
        }
        if ui.selectable_label(matches!(k, FollowKind::Jump(_)), "Jump…").clicked() && !matches!(k, FollowKind::Jump(_)) {
            *k = FollowKind::Jump(0);
            changed = true;
        }
    });
    if let FollowKind::Jump(i) = k {
        let mut n = *i + 1;
        if ui.add(egui::DragValue::new(&mut n).range(1..=999).prefix("to ")).changed() {
            *i = n - 1;
            changed = true;
        }
    }
    changed
}

fn master_column(app: &mut App, ui: &mut egui::Ui, st: &EngineState, blink: bool) {
    ui.vertical(|ui| {
        ui.set_width(COL_W + 20.0);
        let (resp, _) = header(ui, "Master", theme::ACCENT, app.device_target == TrackRef::Master, 0, None);
        if resp.clicked() {
            app.device_target = TrackRef::Master;
            app.detail_tab = DetailTab::Devices;
        }
        let next = st.next_scene.load(Relaxed);
        for s in 0..app.project.scenes.len() {
            if s >= app.project.scenes.len() {
                break;
            }
            if app.renaming.as_ref().is_some_and(|(r, _)| *r == Rename::Scene(s)) {
                rename_field(app, ui, Rename::Scene(s));
                continue;
            }
            let scene = &app.project.scenes[s];
            let mut label = scene.name.clone();
            if let Some(bpm) = scene.tempo {
                label.push_str(&format!("  {bpm:.0}"));
            }
            if let Some(sig) = scene.time_signature {
                label.push_str(&format!("  {}/{}", sig.numerator, sig.denominator));
            }
            if scene.follow.is_some_and(|f| f.enabled) {
                label.push_str("  →");
            }
            let fill = match scene.color {
                Some(c) => theme::dim(theme::rgb(c), 0.55),
                None if app.selected_scene == s => theme::SLOT_HOVER,
                None => theme::SLOT,
            };
            let (rect, resp) = ui.allocate_exact_size(vec2(COL_W + 20.0, SLOT_H), Sense::click());
            let inner = rect.shrink2(vec2(1.0, 1.0));
            let p = ui.painter();
            p.rect_filled(inner, 2.0, fill);
            let c0 = inner.left_center() + vec2(8.0, 0.0);
            let queued = next == s as i32 && blink;
            p.add(egui::Shape::convex_polygon(vec![c0 + vec2(-4.0, -5.0), c0 + vec2(-4.0, 5.0), c0 + vec2(4.0, 0.0)], if queued { theme::PLAY } else { theme::TEXT }, Stroke::NONE));
            p.text(inner.left_center() + vec2(16.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(11.0), theme::TEXT);
            if app.selected_scene == s {
                p.rect_stroke(inner, 2.0, Stroke::new(1.0, theme::DIM), StrokeKind::Inside);
            }
            let name = scene.name.clone();
            if resp.clicked() {
                app.selected_scene = s;
                app.send(Command::LaunchScene { scene: s });
            }
            if resp.double_clicked() {
                app.renaming = Some((Rename::Scene(s), name));
            }
            resp.context_menu(|ui| scene_menu(app, ui, s));
            mappable(app, ui, rect, MappingTarget::LaunchScene(s));
        }
        if ui.add(egui::Button::new(RichText::new("Stop All Clips").size(11.0)).min_size(vec2(COL_W + 20.0, 18.0))).clicked() {
            app.send(Command::StopAllClips);
        }
        ui.add_space(6.0);
        mixer_strip(app, ui, TrackRef::Master, st);
        ui.add_space(4.0);
        egui::Frame::new().fill(theme::BG).corner_radius(3).inner_margin(4).show(ui, |ui| {
            ui.set_width(COL_W + 12.0);
            ui.label(RichText::new("Crossfader").size(10.0).color(theme::DIM));
            let mut x = app.project.crossfader;
            ui.spacing_mut().slider_width = COL_W;
            let r = ui.add(egui::Slider::new(&mut x, -1.0..=1.0).show_value(false));
            if r.double_clicked() {
                x = 0.0;
            }
            if x != app.project.crossfader {
                app.project.crossfader = x;
                app.dirty = true;
                app.send(Command::SetCrossfader(x));
            }
            mappable(app, ui, r.rect, MappingTarget::Crossfader);
            ui.horizontal(|ui| {
                if small_toggle(ui, app.cue_mode, if app.cue_mode { "Cue" } else { "Solo" }, theme::SOLO)
                    .on_hover_text("Solo buttons: Solo mutes other tracks; Cue sends soloed tracks to outputs 3/4")
                    .clicked()
                {
                    app.cue_mode = !app.cue_mode;
                    app.send(Command::SetCueMode(app.cue_mode));
                }
                let mut cue = app.project.cue_volume_db;
                if knob(ui, &mut cue, -60.0, 6.0, 0.0, 22.0, theme::SOLO).on_hover_text(format!("Cue volume {cue:.1} dB")).changed() {
                    app.project.cue_volume_db = cue;
                    app.send(Command::SetCueVolume(cue));
                }
                meter(ui, st.cue.peak_l.load(), st.cue.peak_r.load(), 22.0);
            });
        });
    });
}

fn input_label(i: InputSource, app: &App) -> String {
    match i {
        InputSource::None => "No input".into(),
        InputSource::Stereo(n) => format!("In {}/{}", n * 2 + 1, n * 2 + 2),
        InputSource::Mono(n) => format!("In {}", n + 1),
        InputSource::Track(t) => format!("← {}", app.project.tracks.get(t).map(|t| t.name.as_str()).unwrap_or("?")),
        InputSource::Master => "← Master".into(),
    }
}

fn io_section(app: &mut App, ui: &mut egui::Ui, t: usize) {
    let tr = &app.project.tracks[t];
    let kind = tr.kind;
    if kind == TrackKind::Group {
        return;
    }
    let mut input = tr.input;
    let mut monitor = tr.monitor;
    let in_ch = app.engine.state.input_channels.load(Relaxed) as u8;
    ui.spacing_mut().combo_width = COL_W - 16.0;
    if kind == TrackKind::Audio {
        egui::ComboBox::from_id_salt(("input", t)).selected_text(RichText::new(input_label(input, app)).size(10.0)).show_ui(ui, |ui| {
            ui.selectable_value(&mut input, InputSource::None, "No input");
            for n in 0..in_ch.div_ceil(2).max(1) {
                ui.selectable_value(&mut input, InputSource::Stereo(n), input_label(InputSource::Stereo(n), app));
            }
            for n in 0..in_ch.max(1) {
                ui.selectable_value(&mut input, InputSource::Mono(n), input_label(InputSource::Mono(n), app));
            }
            ui.separator();
            for o in 0..app.project.tracks.len() {
                if o != t && app.project.tracks[o].kind != TrackKind::Group {
                    ui.selectable_value(&mut input, InputSource::Track(o), input_label(InputSource::Track(o), app));
                }
            }
            ui.selectable_value(&mut input, InputSource::Master, "← Master");
        });
    } else {
        ui.label(RichText::new("In: all MIDI").size(10.0).color(theme::DIM));
    }
    ui.horizontal(|ui| {
        for (m, l) in [(Monitor::In, "In"), (Monitor::Auto, "Auto"), (Monitor::Off, "Off")] {
            if small_toggle(ui, monitor == m, l, theme::ACCENT).on_hover_text("Monitoring").clicked() {
                monitor = m;
            }
        }
    });
    let (old_in, old_mon) = (app.project.tracks[t].input, app.project.tracks[t].monitor);
    if input != old_in {
        app.checkpoint();
        app.project.tracks[t].input = input;
        app.rebuild();
    }
    if monitor != old_mon {
        app.project.tracks[t].monitor = monitor;
        app.dirty = true;
        app.send(Command::SetMonitor { track: t, monitor });
    }
}

fn mixer_strip(app: &mut App, ui: &mut egui::Ui, ch: TrackRef, st: &EngineState) {
    let (pl, pr) = match ch {
        TrackRef::Track(t) => st.tracks.get(t).map(|s| (s.channel.peak_l.load(), s.channel.peak_r.load())).unwrap_or_default(),
        TrackRef::Return(r) => st.returns.get(r).map(|s| (s.peak_l.load(), s.peak_r.load())).unwrap_or_default(),
        TrackRef::Master => (st.master.peak_l.load(), st.master.peak_r.load()),
    };
    let Some(mix) = app.mixer_mut(ch).copied() else { return };

    egui::Frame::new().fill(theme::BG).corner_radius(3).inner_margin(4).show(ui, |ui| {
        ui.set_width(COL_W - 8.0);
        if let TrackRef::Track(t) = ch {
            io_section(app, ui, t);
        }
        // Sends
        if let TrackRef::Track(t) = ch {
            ui.horizontal_wrapped(|ui| {
                for r in 0..app.project.returns.len() {
                    let letter = (b'A' + r as u8) as char;
                    let mut db = app.project.tracks[t].sends.get(r).copied().unwrap_or(-70.0);
                    ui.vertical(|ui| {
                        let resp = knob(ui, &mut db, -70.0, 0.0, -70.0, 24.0, theme::DIM).on_hover_text(format!("Send {letter}: {db:.1} dB"));
                        if resp.changed() {
                            if let Some(s) = app.project.tracks[t].sends.get_mut(r) {
                                *s = db;
                            }
                            app.dirty = true;
                            app.send(Command::SetSend { track: t, ret: r, db });
                        }
                        mappable(app, ui, resp.rect, MappingTarget::Send { track: t, ret: r });
                        ui.label(RichText::new(letter.to_string()).size(9.0).color(theme::DIM));
                    });
                }
            });
        }
        // Pan
        let mut pan = mix.pan;
        let resp = ui.add(egui::Slider::new(&mut pan, -1.0..=1.0).show_value(false)).on_hover_text(format!("Pan {pan:+.2} (double-click to center)"));
        if resp.double_clicked() {
            pan = 0.0;
        }
        if pan != mix.pan {
            if let Some(m) = app.mixer_mut(ch) {
                m.pan = pan;
            }
            app.dirty = true;
            app.send(Command::SetPan { channel: ch, pan });
        }
        if let TrackRef::Track(t) = ch {
            mappable(app, ui, resp.rect, MappingTarget::Pan(t));
        }
        ui.horizontal(|ui| {
            let mut vol = mix.volume_db;
            let resp = ui.add(egui::Slider::new(&mut vol, -70.0..=6.0).vertical().show_value(false));
            let resp = resp.on_hover_text(format!("{vol:.1} dB (double-click for 0 dB)"));
            if resp.double_clicked() {
                vol = 0.0;
            }
            if vol != mix.volume_db {
                if let Some(m) = app.mixer_mut(ch) {
                    m.volume_db = vol;
                }
                app.dirty = true;
                app.send(Command::SetVolume { channel: ch, db: vol });
            }
            if let TrackRef::Track(t) = ch {
                mappable(app, ui, resp.rect, MappingTarget::Volume(t));
            }
            let input_peak = if let TrackRef::Track(t) = ch { st.tracks.get(t).map_or(0.0, |s| s.input_peak.load()) } else { 0.0 };
            let armed = matches!(ch, TrackRef::Track(t) if app.project.tracks.get(t).is_some_and(|x| x.armed));
            if armed && input_peak > 0.0 {
                meter(ui, input_peak, input_peak, 100.0);
            } else {
                meter(ui, pl, pr, 100.0);
            }
            ui.vertical(|ui| {
                let active = !mix.mute;
                let r = small_toggle(ui, active, "On", theme::ACCENT).on_hover_text("Track activator");
                if r.clicked() {
                    if let Some(m) = app.mixer_mut(ch) {
                        m.mute = active;
                    }
                    app.send(Command::SetMute { channel: ch, mute: active });
                }
                if let TrackRef::Track(t) = ch {
                    mappable(app, ui, r.rect, MappingTarget::Mute(t));
                    let r = small_toggle(ui, mix.solo, if app.cue_mode { "C" } else { "S" }, theme::SOLO).on_hover_text(if app.cue_mode { "Cue" } else { "Solo" });
                    if r.clicked() {
                        let solo = !mix.solo;
                        if !ui.input(|i| i.modifiers.command) && !app.cue_mode && solo {
                            // Exclusive solo unless Ctrl is held.
                            for o in 0..app.project.tracks.len() {
                                if o != t && app.project.tracks[o].mixer.solo {
                                    app.project.tracks[o].mixer.solo = false;
                                    app.send(Command::SetSolo { track: o, solo: false });
                                }
                            }
                        }
                        app.project.tracks[t].mixer.solo = solo;
                        app.send(Command::SetSolo { track: t, solo });
                    }
                    mappable(app, ui, r.rect, MappingTarget::Solo(t));
                    if app.project.tracks[t].kind != TrackKind::Group {
                        let r = icon_button(ui, Icon::Record, armed, theme::REC, vec2(22.0, 18.0)).on_hover_text("Arm for recording (Ctrl-click: arm several)");
                        if r.clicked() {
                            let exclusive = !ui.input(|i| i.modifiers.command);
                            app.set_arm(t, !armed, exclusive);
                        }
                        mappable(app, ui, r.rect, MappingTarget::Arm(t));
                    }
                    let xf = app.project.tracks[t].crossfade;
                    let (label, color) = match xf {
                        CrossfadeAssign::None => ("·", theme::SLOT),
                        CrossfadeAssign::A => ("A", theme::XFADE_A),
                        CrossfadeAssign::B => ("B", theme::XFADE_B),
                    };
                    if small_toggle(ui, xf != CrossfadeAssign::None, label, color).on_hover_text("Crossfader assignment (click to cycle)").clicked() {
                        let next = match xf {
                            CrossfadeAssign::None => CrossfadeAssign::A,
                            CrossfadeAssign::A => CrossfadeAssign::B,
                            CrossfadeAssign::B => CrossfadeAssign::None,
                        };
                        app.checkpoint();
                        app.project.tracks[t].crossfade = next;
                        app.rebuild();
                    }
                }
            });
        });
        ui.label(RichText::new(format!("{:+.1} dB", mix.volume_db)).size(10.0).color(theme::DIM));
    });
}
