//! Arrangement view: the song as a timeline of clips per track, with
//! automation lanes, a tempo lane, locators and the loop brace.

use super::theme;
use crate::app::{App, ArrDrag, ArrDragMode, ClipRef, DetailTab};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use loopcraft_core::*;
use loopcraft_engine::{Command, TrackRef};
use std::sync::atomic::Ordering::Relaxed;

const HEAD_W: f32 = 150.0;
const RULER_H: f32 = 30.0;
const LANE_H: f32 = 46.0;
const AUTO_H: f32 = 54.0;
const TEMPO_H: f32 = 50.0;
const EDGE: f32 = 6.0;

struct Lane {
    track: usize,
    y: f32,
    auto: Option<AutomationTarget>,
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let st = app.engine.state.clone();
    let bar = app.project.time_signature.beats_per_bar();

    ui.horizontal(|ui| {
        ui.label(RichText::new("Zoom").color(theme::DIM));
        if ui.small_button("−").clicked() {
            app.arr_pixels_per_beat = (app.arr_pixels_per_beat / 1.4).max(2.0);
        }
        if ui.small_button("+").clicked() {
            app.arr_pixels_per_beat = (app.arr_pixels_per_beat * 1.4).min(160.0);
        }
        ui.checkbox(&mut app.arr_follow, "Follow");
        ui.checkbox(&mut app.show_tempo_lane, "Tempo lane");
        if ui.small_button("+ Locator").on_hover_text("Add a locator at the playhead").clicked() {
            let b = st.beat.load();
            app.add_locator(b);
        }
        ui.separator();
        if let Some(ClipRef::Arrangement { track, index }) = app.selected_clip {
            if ui.small_button("Split at playhead (Ctrl+E)").clicked() {
                app.split_arrangement_clip(track, index, st.beat.load());
            }
            if ui.small_button("Duplicate (Ctrl+D)").clicked() {
                app.duplicate_arrangement_clip(track, index);
            }
            if ui.small_button("Consolidate").clicked() {
                app.consolidate_region(track, index);
            }
        }
        ui.label(RichText::new("Ruler: click = playhead, drag = loop · edges trim · right-click for more").color(theme::DIM).size(10.0));
    });

    let ppb = app.arr_pixels_per_beat;
    let len = (app.project.arrangement_length() + 16.0 * bar).max(64.0 * bar);
    let width = HEAD_W + len as f32 * ppb;

    // Lane layout.
    let mut lanes = Vec::new();
    let mut y = RULER_H + if app.show_tempo_lane { TEMPO_H } else { 0.0 };
    for t in 0..app.project.tracks.len() {
        if app.hidden(t) {
            continue;
        }
        let auto = app.automation_view.get(&t).copied();
        lanes.push(Lane { track: t, y, auto });
        y += LANE_H + if auto.is_some() { AUTO_H } else { 0.0 };
    }
    let height = y + 20.0;

    // Keep the playhead in view.
    let view_w = ui.available_width();
    let beat_now = st.beat.load();
    let mut scroll = egui::ScrollArea::both().auto_shrink([false, false]);
    if app.arr_follow && app.playing() && app.arr_drag.is_none() {
        let px = beat_now as f32 * ppb;
        let visible = view_w - HEAD_W - 30.0;
        if px < app.arr_scroll_x || px > app.arr_scroll_x + visible {
            scroll = scroll.horizontal_scroll_offset((px - 20.0).max(0.0));
        }
    }

    let out = scroll.show(ui, |ui| {
        let (area, _) = ui.allocate_exact_size(vec2(width, height.max(ui.available_height())), Sense::hover());
        let x_of = |beat: f64| area.left() + HEAD_W + beat as f32 * ppb;
        let beat_of = |x: f32| ((x - area.left() - HEAD_W) / ppb).max(0.0) as f64;
        let snap_step = if ppb * bar as f32 > 160.0 {
            0.25
        } else if ppb * bar as f32 > 60.0 {
            1.0
        } else {
            bar
        };
        let snap = |b: f64| (b / snap_step).round() * snap_step;
        let snap_floor = |b: f64| (b / snap_step).floor() * snap_step;
        let p = ui.painter().clone();
        p.rect_filled(area, 0.0, theme::BG);

        // Grid.
        let step_bars = if ppb * bar as f32 > 40.0 {
            1.0
        } else if ppb * bar as f32 > 12.0 {
            4.0
        } else {
            8.0
        };
        let mut b = 0.0;
        while b < len {
            let x = x_of(b);
            let strong = ((b / bar) % (step_bars * 4.0)).abs() < 1e-6;
            p.line_segment(
                [pos2(x, area.top() + RULER_H), pos2(x, area.bottom())],
                Stroke::new(1.0, if strong { theme::LINE } else { theme::dim(theme::LINE, 0.6) }),
            );
            b += bar * step_bars;
        }
        if let Some((ls, le)) = app.project.loop_region {
            let lr = Rect::from_min_max(pos2(x_of(ls), area.top() + RULER_H), pos2(x_of(le), area.bottom()));
            p.rect_filled(lr, 0.0, theme::ACCENT.gamma_multiply(0.05));
        }

        ruler(app, ui, &p, area, width, len, bar, step_bars, &x_of, &beat_of, &snap_floor);
        if app.show_tempo_lane {
            tempo_lane(app, ui, &p, area, width, &x_of, &beat_of, &snap);
        }

        // Track lanes.
        let mut actions = LaneActions::default();
        for lane in &lanes {
            track_lane(app, ui, &p, area, width, lane, &st, &x_of, &beat_of, &mut actions);
            if let Some(target) = lane.auto {
                automation_lane(app, ui, &p, area, width, lane.track, lane.y + LANE_H, target, &x_of, &beat_of, &snap);
            }
        }
        apply_lane_actions(app, actions, bar);

        // Region dragging: a ghost follows the pointer; the edit lands on release.
        if let Some(d) = app.arr_drag
            && let Some(pos) = ui.ctx().pointer_interact_pos()
        {
            let delta = beat_of(pos.x) - d.press_beat;
            let (start, dur, offset) = drag_result(&d, delta, snap);
            let to_lane = lanes.iter().rev().find(|l| pos.y >= area.top() + l.y).map(|l| l.track).unwrap_or(d.track);
            let ghost_track = if d.mode == ArrDragMode::Move { to_lane } else { d.track };
            if let Some(l) = lanes.iter().find(|l| l.track == ghost_track) {
                let y = area.top() + l.y;
                let r = Rect::from_min_max(pos2(x_of(start), y + 3.0), pos2(x_of(start + dur), y + LANE_H - 3.0));
                p.rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
            }
            if ui.input(|inp| inp.pointer.any_released()) {
                app.arr_drag = None;
                let moved = (start - d.orig_start).abs() > 1e-9 || (dur - d.orig_dur).abs() > 1e-9 || ghost_track != d.track;
                if moved {
                    app.checkpoint();
                    match d.mode {
                        ArrDragMode::Move => {
                            app.move_arrangement_clip(d.track, d.index, ghost_track, start);
                        }
                        ArrDragMode::Resize => {
                            if let Some(a) = app.project.tracks.get_mut(d.track).and_then(|t| t.arrangement.get_mut(d.index)) {
                                a.duration = Some(dur);
                                if !a.clip.looping && dur + offset > a.clip.length {
                                    // Lengthening a one-shot region past its clip turns looping on.
                                    a.clip.looping = true;
                                }
                            }
                            let end = start + dur;
                            // Overwrite what the region now covers.
                            let a = app.project.tracks[d.track].arrangement.remove(d.index);
                            app.clear_range(d.track, d.orig_start + d.orig_dur, end);
                            app.project.tracks[d.track].arrangement.push(a);
                            app.sort_arrangement(d.track);
                        }
                        ArrDragMode::Trim => {
                            app.trim_region_start(d.track, d.index, start);
                        }
                    }
                    app.selected_clip = app.selected_clip.filter(|c| app.clip(*c).is_some());
                    app.rebuild();
                }
            }
        }

        // Locators across the lanes.
        for (b, _) in &app.project.locators {
            let x = x_of(*b);
            p.line_segment([pos2(x, area.top() + RULER_H), pos2(x, area.bottom())], Stroke::new(1.0, theme::dim(theme::TEXT, 0.35)));
        }

        // Playhead.
        let x = x_of(beat_now);
        p.line_segment([pos2(x, area.top()), pos2(x, area.bottom())], Stroke::new(1.5, theme::ACCENT));
        p.add(egui::Shape::convex_polygon(
            vec![Pos2::new(x - 5.0, area.top()), Pos2::new(x + 5.0, area.top()), Pos2::new(x, area.top() + 7.0)],
            theme::ACCENT,
            Stroke::NONE,
        ));
    });
    app.arr_scroll_x = out.state.offset.x;
}

fn drag_result(d: &ArrDrag, delta: f64, snap: impl Fn(f64) -> f64) -> (f64, f64, f64) {
    match d.mode {
        ArrDragMode::Move => (snap(d.orig_start + delta).max(0.0), d.orig_dur, d.orig_offset),
        ArrDragMode::Resize => {
            let end = snap(d.orig_start + d.orig_dur + delta).max(d.orig_start + 0.25);
            (d.orig_start, end - d.orig_start, d.orig_offset)
        }
        ArrDragMode::Trim => {
            let end = d.orig_start + d.orig_dur;
            let s = snap(d.orig_start + delta).clamp(d.orig_start - d.orig_offset, end - 0.25).max(0.0);
            (s, end - s, d.orig_offset + (s - d.orig_start))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn ruler(
    app: &mut App,
    ui: &mut egui::Ui,
    p: &egui::Painter,
    area: Rect,
    width: f32,
    len: f64,
    bar: f64,
    step_bars: f64,
    x_of: &dyn Fn(f64) -> f32,
    beat_of: &dyn Fn(f32) -> f64,
    snap_floor: &dyn Fn(f64) -> f64,
) {
    let ruler = Rect::from_min_size(pos2(area.left() + HEAD_W, area.top()), vec2(width - HEAD_W, RULER_H));
    let r_resp = ui.interact(ruler, ui.id().with("ruler"), Sense::click_and_drag());
    p.rect_filled(ruler, 0.0, theme::PANEL);
    if let Some((ls, le)) = app.project.loop_region {
        let lr = Rect::from_min_max(pos2(x_of(ls), ruler.top()), pos2(x_of(le), ruler.top() + 10.0));
        p.rect_filled(lr, 0.0, theme::dim(theme::ACCENT, 0.7));
    }
    let mut b = 0.0;
    while b < len {
        p.text(pos2(x_of(b) + 3.0, ruler.top() + 18.0), Align2::LEFT_CENTER, format!("{}", (b / bar) as i64 + 1), FontId::monospace(10.0), theme::DIM);
        b += bar * step_bars;
    }
    // Locators.
    let mut remove_loc = None;
    let mut seek_to = None;
    for (i, (b, name)) in app.project.locators.iter().enumerate() {
        let x = x_of(*b);
        let flag = Rect::from_min_size(pos2(x, ruler.bottom() - 9.0), vec2(8.0, 9.0));
        p.add(egui::Shape::convex_polygon(vec![flag.left_top(), flag.left_bottom(), flag.right_center()], theme::TEXT, Stroke::NONE));
        p.text(pos2(x + 10.0, ruler.bottom() - 4.0), Align2::LEFT_CENTER, name, FontId::proportional(9.0), theme::TEXT);
        let lr = ui.interact(flag.expand(2.0), ui.id().with(("loc", i)), Sense::click());
        if lr.clicked() {
            seek_to = Some(*b);
        }
        lr.context_menu(|ui| {
            if ui.button("Delete locator").clicked() {
                remove_loc = Some(i);
                ui.close();
            }
        });
    }
    if let Some(b) = seek_to {
        app.send(Command::Seek(b));
    }
    if let Some(i) = remove_loc {
        app.checkpoint();
        app.project.locators.remove(i);
    }
    if r_resp.clicked()
        && let Some(pos) = r_resp.interact_pointer_pos()
    {
        app.send(Command::Seek(snap_floor(beat_of(pos.x))));
    }
    // Drag on the ruler sets the loop brace.
    if r_resp.drag_started()
        && let Some(pos) = r_resp.interact_pointer_pos()
    {
        app.loop_drag = Some(snap_floor(beat_of(pos.x)));
    }
    if let (Some(a), Some(pos)) = (app.loop_drag, ui.ctx().pointer_interact_pos()) {
        let b = (beat_of(pos.x) / bar).round() * bar;
        let (s, e) = if b >= a { (a, b.max(a + bar)) } else { (b, a) };
        let lr = Rect::from_min_max(pos2(x_of(s), ruler.top()), pos2(x_of(e), ruler.top() + 10.0));
        p.rect_filled(lr, 0.0, theme::ACCENT);
        if ui.input(|i| i.pointer.any_released()) {
            app.loop_drag = None;
            app.project.loop_region = Some((s, e));
            app.last_loop = Some((s, e));
            app.dirty = true;
            app.send(Command::SetLoop(app.project.loop_region));
        }
    }
    let click_beat = r_resp.interact_pointer_pos().map(|p| snap_floor(beat_of(p.x)));
    r_resp.context_menu(|ui| {
        let at = click_beat.unwrap_or(0.0);
        if ui.button("Add locator here").clicked() {
            app.add_locator(at);
            ui.close();
        }
        if ui.button("Loop 4 bars from here").clicked() {
            app.project.loop_region = Some((at, at + 4.0 * bar));
            app.send(Command::SetLoop(app.project.loop_region));
            ui.close();
        }
        if ui.button("Loop off").clicked() {
            app.project.loop_region = None;
            app.send(Command::SetLoop(None));
            ui.close();
        }
        ui.separator();
        if ui.button("Insert 1 bar of silence here").clicked() {
            app.insert_time(at, bar);
            ui.close();
        }
        if let Some((ls, le)) = app.project.loop_region
            && ui.button("Delete time inside the loop brace").clicked()
        {
            app.delete_time(ls, le);
            ui.close();
        }
        if ui.button("Add tempo point here").clicked() {
            let bpm = app.project.tempo_at(at);
            app.checkpoint();
            app.set_tempo_point(at, bpm, 1e-6);
            app.show_tempo_lane = true;
            app.rebuild();
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn tempo_lane(
    app: &mut App,
    ui: &mut egui::Ui,
    p: &egui::Painter,
    area: Rect,
    width: f32,
    x_of: &dyn Fn(f64) -> f32,
    beat_of: &dyn Fn(f32) -> f64,
    snap: &dyn Fn(f64) -> f64,
) {
    let y = area.top() + RULER_H;
    let head = Rect::from_min_size(pos2(area.left(), y), vec2(HEAD_W, TEMPO_H));
    p.rect_filled(head.shrink(1.0), 2.0, theme::PANEL);
    p.text(head.left_top() + vec2(8.0, 6.0), Align2::LEFT_TOP, "Tempo", FontId::proportional(12.0), theme::TEXT);
    if !app.project.tempo_automation.is_empty()
        && ui.put(Rect::from_min_size(head.left_bottom() + vec2(8.0, -20.0), vec2(50.0, 16.0)), egui::Button::new(RichText::new("Clear").size(10.0))).clicked()
    {
        app.checkpoint();
        app.project.tempo_automation.clear();
        app.rebuild();
    }
    let body = Rect::from_min_max(pos2(area.left() + HEAD_W, y), pos2(area.left() + width, y + TEMPO_H));
    p.rect_filled(body, 0.0, theme::dim(theme::PANEL, 0.7));
    let (lo, hi) = (60.0, 200.0);
    let y_of = |bpm: f64| body.bottom() - 3.0 - ((bpm - lo) / (hi - lo)).clamp(0.0, 1.0) as f32 * (TEMPO_H - 6.0);
    let bpm_of = |yy: f32| lo + ((body.bottom() - 3.0 - yy) / (TEMPO_H - 6.0)).clamp(0.0, 1.0) as f64 * (hi - lo);
    let pts = app.project.tempo_automation.clone();
    let line_y = |b: f64| y_of(loopcraft_core::tempo_at(&pts, app.project.tempo, b));
    let mut last = pos2(body.left(), line_y(0.0));
    for &(b, bpm) in &pts {
        let pt = pos2(x_of(b), y_of(bpm));
        p.line_segment([last, pt], Stroke::new(1.5, theme::ACCENT));
        last = pt;
    }
    p.line_segment([last, pos2(body.right(), last.y)], Stroke::new(1.5, theme::ACCENT));
    let resp = ui.interact(body, ui.id().with("tempolane"), Sense::click_and_drag());
    let mut hit = None;
    for (i, &(b, bpm)) in pts.iter().enumerate() {
        let c = pos2(x_of(b), y_of(bpm));
        p.circle_filled(c, 3.5, theme::ACCENT);
        if resp.interact_pointer_pos().is_some_and(|q| q.distance(c) < 7.0) || resp.hover_pos().is_some_and(|q| q.distance(c) < 7.0) {
            hit = Some(i);
            p.text(c + vec2(6.0, -8.0), Align2::LEFT_BOTTOM, format!("{bpm:.1}"), FontId::proportional(10.0), theme::TEXT);
        }
    }
    if resp.drag_started() {
        app.checkpoint();
        app.tempo_drag = hit;
    }
    if let (Some(i), Some(pos)) = (app.tempo_drag, resp.interact_pointer_pos())
        && resp.dragged()
        && i < app.project.tempo_automation.len()
    {
        app.project.tempo_automation[i] = (snap(beat_of(pos.x)), (bpm_of(pos.y) * 10.0).round() / 10.0);
        app.dirty = true;
    }
    if resp.drag_stopped() && app.tempo_drag.is_some() {
        app.tempo_drag = None;
        app.project.tempo_automation.sort_by(|a, b| a.0.total_cmp(&b.0));
        app.rebuild();
    }
    if resp.clicked()
        && hit.is_none()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        app.checkpoint();
        app.set_tempo_point(snap(beat_of(pos.x)), (bpm_of(pos.y) * 10.0).round() / 10.0, 1e-6);
        app.rebuild();
    }
    if resp.secondary_clicked()
        && let Some(i) = hit
    {
        app.checkpoint();
        app.project.tempo_automation.remove(i);
        app.rebuild();
    }
}

#[derive(Default)]
struct LaneActions {
    select: Option<ClipRef>,
    delete: Option<ClipRef>,
    create: Option<(usize, f64)>,
    start_drag: Option<ArrDrag>,
    split: Option<(usize, usize, f64)>,
    duplicate: Option<(usize, usize)>,
    consolidate: Option<(usize, usize)>,
    toggle_loop: Option<(usize, usize)>,
}

#[allow(clippy::too_many_arguments)]
fn track_lane(
    app: &mut App,
    ui: &mut egui::Ui,
    p: &egui::Painter,
    area: Rect,
    width: f32,
    lane: &Lane,
    st: &loopcraft_engine::EngineState,
    x_of: &dyn Fn(f64) -> f32,
    beat_of: &dyn Fn(f32) -> f64,
    act: &mut LaneActions,
) {
    let t = lane.track;
    let bar = app.project.time_signature.beats_per_bar();
    let y = area.top() + lane.y;
    let full_h = LANE_H + if lane.auto.is_some() { AUTO_H } else { 0.0 };
    let lane_r = Rect::from_min_size(pos2(area.left(), y), vec2(width, LANE_H));
    let track = &app.project.tracks[t];
    let following = st.tracks.get(t).map(|s| s.follows_arrangement.load(Relaxed)).unwrap_or(true);
    let recording = st.tracks.get(t).is_some_and(|s| s.recording.load(Relaxed));
    let playing_region = st.tracks.get(t).map_or(-1, |s| s.arrangement_clip.load(Relaxed));
    p.line_segment([pos2(area.left(), y + full_h), pos2(area.left() + width, y + full_h)], Stroke::new(1.0, theme::LINE));

    // Header.
    let head = Rect::from_min_size(lane_r.min, vec2(HEAD_W, full_h));
    p.rect_filled(head.shrink(1.0), 2.0, if recording { theme::dim(theme::REC, 0.35) } else { theme::PANEL });
    p.rect_filled(Rect::from_min_size(head.min + vec2(1.0, 1.0), vec2(4.0, full_h - 2.0)), 0.0, theme::rgb(track.color));
    let indent = app.project.depth(t) as f32 * 8.0;
    let is_group = track.kind == TrackKind::Group;
    let name = track.name.clone();
    let (armed, mute, solo, folded) = (track.armed, track.mixer.mute, track.mixer.solo, track.folded);
    p.text(head.left_top() + vec2(10.0 + indent + if is_group { 12.0 } else { 0.0 }, 6.0), Align2::LEFT_TOP, &name, FontId::proportional(12.0), theme::TEXT);
    if !following {
        p.text(head.left_top() + vec2(10.0 + indent, 22.0), Align2::LEFT_TOP, "playing Session", FontId::proportional(9.0), theme::ACCENT);
    }
    let h_resp = ui.interact(head, ui.id().with(("head", t)), Sense::click());
    // Small header buttons.
    let bx = head.right() - 4.0;
    let by = head.top() + LANE_H - 20.0;
    let mut btn = |i: f32, label: &str, on: bool, color: Color32, hint: &str| -> bool {
        let r = Rect::from_min_size(pos2(bx - (i + 1.0) * 20.0, by), vec2(18.0, 16.0));
        ui.put(
            r,
            egui::Button::new(RichText::new(label).size(10.0).color(if on { Color32::BLACK } else { theme::TEXT })).fill(if on { color } else { theme::SLOT }),
        )
        .on_hover_text(hint)
        .clicked()
    };
    let auto_click = btn(0.0, "A", lane.auto.is_some(), theme::ACCENT, "Show an automation lane");
    let solo_click = btn(1.0, "S", solo, theme::SOLO, "Solo");
    let mute_click = btn(2.0, "M", mute, theme::ACCENT, "Mute");
    let arm_click = !is_group && btn(3.0, "R", armed, theme::REC, "Arm");
    if is_group {
        let fr = Rect::from_min_size(head.min + vec2(8.0 + indent, 6.0), vec2(12.0, 12.0));
        let fresp = ui.interact(fr, ui.id().with(("afold", t)), Sense::click());
        let c = fr.center();
        let s = 4.0;
        let pts = if !folded {
            vec![c + vec2(-s, -s * 0.5), c + vec2(s, -s * 0.5), c + vec2(0.0, s * 0.7)]
        } else {
            vec![c + vec2(-s * 0.5, -s), c + vec2(-s * 0.5, s), c + vec2(s * 0.7, 0.0)]
        };
        p.add(egui::Shape::convex_polygon(pts, theme::TEXT, Stroke::NONE));
        if fresp.clicked() {
            app.project.tracks[t].folded = !folded;
        }
    }
    if auto_click {
        if lane.auto.is_some() {
            app.automation_view.remove(&t);
        } else {
            app.automation_view.insert(t, AutomationTarget::Volume);
        }
    }
    if solo_click {
        app.project.tracks[t].mixer.solo = !solo;
        app.send(Command::SetSolo { track: t, solo: !solo });
    }
    if mute_click {
        app.project.tracks[t].mixer.mute = !mute;
        app.send(Command::SetMute { channel: TrackRef::Track(t), mute: !mute });
    }
    if arm_click {
        let exclusive = !ui.input(|i| i.modifiers.command);
        app.set_arm(t, !armed, exclusive);
    }
    if h_resp.clicked() {
        app.selected_track = Some(t);
        app.device_target = TrackRef::Track(t);
    }
    h_resp.context_menu(|ui| {
        if ui.button("Delete track").clicked() {
            app.delete_track(t);
            ui.close();
        }
        if ui.button("Duplicate track").clicked() {
            app.duplicate_track(t);
            ui.close();
        }
    });
    if app.selected_track == Some(t) {
        p.rect_stroke(head.shrink(1.0), 2.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
    }
    if t >= app.project.tracks.len() {
        return;
    }

    // Body: double-click to create.
    let body = Rect::from_min_max(pos2(area.left() + HEAD_W, y), lane_r.max);
    let b_resp = ui.interact(body, ui.id().with(("lane", t)), Sense::click());
    if b_resp.double_clicked()
        && app.project.tracks[t].kind == TrackKind::Midi
        && let Some(pos) = b_resp.interact_pointer_pos()
    {
        act.create = Some((t, (beat_of(pos.x) / bar).floor() * bar));
    }

    // Group lanes summarize their children.
    if is_group {
        for c in 0..app.project.tracks.len() {
            if !app.project.is_inside(c, t) {
                continue;
            }
            for a in &app.project.tracks[c].arrangement {
                let r = Rect::from_min_max(pos2(x_of(a.start), y + 8.0), pos2(x_of(a.end()), y + LANE_H - 8.0));
                p.rect_filled(r, 2.0, theme::dim(theme::rgb(a.clip.color), 0.35));
            }
        }
        return;
    }

    let ppb = app.arr_pixels_per_beat;
    let regions = app.project.tracks[t].arrangement.clone();
    for (i, ac) in regions.iter().enumerate() {
        let r = Rect::from_min_max(pos2(x_of(ac.start), y + 3.0), pos2(x_of(ac.end()), y + LANE_H - 3.0));
        let cref = ClipRef::Arrangement { track: t, index: i };
        let selected = app.selected_clip == Some(cref);
        let base = theme::rgb(ac.clip.color);
        let active = following && playing_region == i as i32;
        p.rect_filled(
            r,
            3.0,
            if !following {
                theme::dim(base, 0.35)
            } else if active {
                base
            } else {
                theme::dim(base, 0.75)
            },
        );
        p.text(r.left_top() + vec2(4.0, 2.0), Align2::LEFT_TOP, &ac.clip.name, FontId::proportional(10.0), Color32::BLACK);
        draw_clip_preview(app, p, r.shrink2(vec2(2.0, 8.0)).translate(vec2(0.0, 4.0)), ac, ppb);
        if selected {
            p.rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
        }
        let c_resp = ui.interact(r, ui.id().with(("clip", t, i)), Sense::click_and_drag());
        let hover_x = c_resp.hover_pos().map(|q| q.x);
        let near_left = hover_x.is_some_and(|x| x - r.left() < EDGE);
        let near_right = hover_x.is_some_and(|x| r.right() - x < EDGE);
        if near_left || near_right {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if c_resp.clicked() {
            act.select = Some(cref);
        }
        if c_resp.drag_started()
            && let Some(pos) = c_resp.interact_pointer_pos()
        {
            let mode = if pos.x - r.left() < EDGE {
                ArrDragMode::Trim
            } else if r.right() - pos.x < EDGE {
                ArrDragMode::Resize
            } else {
                ArrDragMode::Move
            };
            act.select = Some(cref);
            act.start_drag =
                Some(ArrDrag { track: t, index: i, orig_start: ac.start, orig_dur: ac.duration(), orig_offset: ac.offset, press_beat: beat_of(pos.x), mode });
        }
        let at = c_resp.interact_pointer_pos().map(|q| beat_of(q.x));
        c_resp.context_menu(|ui| {
            if let Some(at) = at
                && ui.button("Split here").clicked()
            {
                act.split = Some((t, i, (at / 0.25).round() * 0.25));
                ui.close();
            }
            if ui.button("Duplicate  (Ctrl+D)").clicked() {
                act.duplicate = Some((t, i));
                ui.close();
            }
            if ui.button(if ac.clip.looping { "Loop off" } else { "Loop on" }).clicked() {
                act.toggle_loop = Some((t, i));
                ui.close();
            }
            if matches!(ac.clip.content, ClipContent::Midi { .. }) && ui.button("Consolidate").clicked() {
                act.consolidate = Some((t, i));
                ui.close();
            }
            if ui.button("Delete").clicked() {
                act.delete = Some(cref);
                ui.close();
            }
        });
    }
}

fn apply_lane_actions(app: &mut App, act: LaneActions, bar: f64) {
    if let Some(c) = act.select {
        app.selected_clip = Some(c);
        app.piano_selected = None;
        app.detail_tab = DetailTab::Clip;
        if let ClipRef::Arrangement { track, .. } = c {
            app.selected_track = Some(track);
            app.device_target = TrackRef::Track(track);
        }
    }
    if let Some(d) = act.start_drag {
        app.arr_drag = Some(d);
    }
    if let Some(c) = act.delete {
        app.delete_clip(c);
    }
    if let Some((t, i, at)) = act.split {
        app.split_arrangement_clip(t, i, at);
    }
    if let Some((t, i)) = act.duplicate {
        app.duplicate_arrangement_clip(t, i);
    }
    if let Some((t, i)) = act.consolidate {
        app.consolidate_region(t, i);
    }
    if let Some((t, i)) = act.toggle_loop {
        app.checkpoint();
        if let Some(a) = app.project.tracks.get_mut(t).and_then(|tr| tr.arrangement.get_mut(i)) {
            a.clip.looping = !a.clip.looping;
            if !a.clip.looping {
                a.duration = Some(a.duration().min(a.clip.length - a.offset));
            }
        }
        app.rebuild();
    }
    if let Some((t, at)) = act.create {
        app.checkpoint();
        let color = app.project.tracks[t].color;
        let clip = Clip { color, looping: false, ..Clip::midi("MIDI Clip", bar, Vec::new()) };
        app.clear_range(t, at, at + bar);
        app.project.tracks[t].arrangement.push(ArrangementClip::new(at, clip));
        app.sort_arrangement(t);
        let index = app.project.tracks[t].arrangement.iter().position(|a| (a.start - at).abs() < 1e-9).unwrap_or(0);
        app.selected_clip = Some(ClipRef::Arrangement { track: t, index });
        app.detail_tab = DetailTab::Clip;
        app.rebuild();
    }
}

#[allow(clippy::too_many_arguments)]
fn automation_lane(
    app: &mut App,
    ui: &mut egui::Ui,
    p: &egui::Painter,
    area: Rect,
    width: f32,
    t: usize,
    y_rel: f32,
    target: AutomationTarget,
    x_of: &dyn Fn(f64) -> f32,
    beat_of: &dyn Fn(f32) -> f64,
    snap: &dyn Fn(f64) -> f64,
) {
    let y = area.top() + y_rel;
    let (lo, hi, label) = app.automation_range(t, target);
    let head = Rect::from_min_size(pos2(area.left() + 8.0, y + 2.0), vec2(HEAD_W - 12.0, 18.0));
    let targets = app.automation_targets(t);
    let mut chosen = None;
    let mut clear = false;
    ui.put(head, |ui: &mut egui::Ui| {
        egui::ComboBox::from_id_salt(("autosel", t))
            .width(HEAD_W - 16.0)
            .selected_text(RichText::new(&label).size(10.0))
            .show_ui(ui, |ui| {
                for (tg, name) in &targets {
                    let has = app.project.tracks[t].automation.iter().any(|l| l.target == *tg && !l.points.is_empty());
                    if ui.selectable_label(*tg == target, if has { format!("* {name}") } else { name.clone() }).clicked() {
                        chosen = Some(*tg);
                    }
                }
                ui.separator();
                if ui.button("Clear this lane").clicked() {
                    clear = true;
                }
            })
            .response
    });
    if let Some(tg) = chosen {
        app.automation_view.insert(t, tg);
    }
    if clear {
        app.clear_automation(t, target);
    }
    let body = Rect::from_min_max(pos2(area.left() + HEAD_W, y + 2.0), pos2(area.left() + width, y + AUTO_H - 2.0));
    p.rect_filled(body, 0.0, theme::dim(theme::PANEL, 0.6));
    let y_of = |v: f32| body.bottom() - 3.0 - ((v - lo) / (hi - lo)).clamp(0.0, 1.0) * (body.height() - 6.0);
    let v_of = |yy: f32| lo + ((body.bottom() - 3.0 - yy) / (body.height() - 6.0)).clamp(0.0, 1.0) * (hi - lo);
    let pts: Vec<(f64, f32)> = app.project.tracks[t].automation.iter().find(|l| l.target == target).map(|l| l.points.clone()).unwrap_or_default();
    let color = theme::rgb(app.project.tracks[t].color);
    if pts.is_empty() {
        p.text(
            body.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            "Click to add breakpoints · drag to move · right-click to delete",
            FontId::proportional(10.0),
            theme::DIM,
        );
    } else {
        let mut last = pos2(body.left(), y_of(pts[0].1));
        for &(b, v) in &pts {
            let pt = pos2(x_of(b), y_of(v));
            p.line_segment([last, pt], Stroke::new(1.5, color));
            last = pt;
        }
        p.line_segment([last, pos2(body.right(), last.y)], Stroke::new(1.5, color));
    }
    let resp = ui.interact(body, ui.id().with(("autolane", t)), Sense::click_and_drag());
    let mut hit = None;
    for (i, &(b, v)) in pts.iter().enumerate() {
        let c = pos2(x_of(b), y_of(v));
        let near = resp.hover_pos().is_some_and(|q| q.distance(c) < 7.0) || resp.interact_pointer_pos().is_some_and(|q| q.distance(c) < 7.0);
        p.circle_filled(c, if near { 4.5 } else { 3.0 }, color);
        if near {
            hit = Some(i);
            p.text(c + vec2(6.0, -6.0), Align2::LEFT_BOTTOM, format!("{v:.2}"), FontId::proportional(10.0), theme::TEXT);
        }
    }
    if resp.drag_started() {
        app.checkpoint();
        app.auto_drag = hit.map(|i| (t, target, i));
    }
    if let (Some((dt, dtg, i)), Some(pos)) = (app.auto_drag, resp.interact_pointer_pos())
        && dt == t
        && dtg == target
        && resp.dragged()
        && let Some(lane) = app.lane_mut(t, target)
        && i < lane.points.len()
    {
        lane.points[i] = (snap(beat_of(pos.x)), v_of(pos.y));
        app.dirty = true;
    }
    if resp.drag_stopped() && app.auto_drag.is_some() {
        app.auto_drag = None;
        if let Some(lane) = app.lane_mut(t, target) {
            lane.points.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        app.rebuild();
    }
    if resp.clicked()
        && hit.is_none()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        app.checkpoint();
        app.set_automation_point(t, target, snap(beat_of(pos.x)), v_of(pos.y), 1e-6);
        app.rebuild();
    }
    if resp.secondary_clicked()
        && let Some(i) = hit
    {
        app.checkpoint();
        app.remove_automation_point(t, target, i);
        app.rebuild();
    }
}

fn draw_clip_preview(app: &App, p: &egui::Painter, r: Rect, ac: &ArrangementClip, ppb: f32) {
    let len = ac.clip.length.max(1e-6);
    let ClipContent::Midi { notes } = &ac.clip.content else {
        // Rough waveform from the cached file.
        if let ClipContent::Audio(a) = &ac.clip.content
            && let Some(buf) = loopcraft_dsp::SampleLoader::load(app.loader.as_ref(), &a.file)
            && r.height() > 6.0
        {
            let ch = &buf.channels[0];
            let w = r.width().max(1.0) as usize;
            let mid = r.center().y;
            for x in (0..w).step_by(2) {
                let beat = ac.offset + x as f64 / ppb as f64;
                let pos = if ac.clip.looping && beat >= len { ac.clip.loop_start + (beat - len) % (len - ac.clip.loop_start).max(1e-6) } else { beat };
                let s = (a.seconds_at(pos) * buf.sample_rate as f64) as usize;
                let span = ((a.seconds_per_beat_at(pos) * buf.sample_rate as f64) / ppb as f64 * 2.0) as usize;
                let (mut mx, mut i) = (0.0f32, s);
                while i < (s + span.max(1)).min(ch.len()) {
                    mx = mx.max(ch[i].abs());
                    i += (span / 32).max(1);
                }
                let h = mx * r.height() * 0.5;
                p.line_segment([pos2(r.left() + x as f32, mid - h), pos2(r.left() + x as f32, mid + h)], Stroke::new(1.0, Color32::from_black_alpha(140)));
            }
        }
        return;
    };
    if notes.is_empty() || r.height() < 6.0 {
        return;
    }
    let lo = notes.iter().map(|n| n.pitch).min().unwrap_or(60) as f32;
    let hi = notes.iter().map(|n| n.pitch).max().unwrap_or(60) as f32 + 1.0;
    let span = (hi - lo).max(1.0);
    let dur = ac.duration();
    // Walk passes from the region's offset.
    let mut song = 0.0;
    let mut pos = ac.offset;
    let mut guard = 0;
    while song < dur && guard < 4096 {
        guard += 1;
        let pass_end = len;
        if pos >= pass_end {
            if !ac.clip.looping {
                break;
            }
            pos = ac.clip.loop_start;
            continue;
        }
        let seg = (pass_end - pos).min(dur - song);
        for n in notes.iter().filter(|n| n.start >= pos && n.start < pos + seg) {
            let s = song + (n.start - pos);
            let e = (s + n.length.min(len - n.start)).min(dur);
            let x0 = r.left() + s as f32 * ppb;
            let x1 = (r.left() + e as f32 * ppb).max(x0 + 1.0);
            let y = r.bottom() - (n.pitch as f32 - lo + 0.5) / span * r.height();
            p.line_segment([pos2(x0, y), pos2(x1, y)], Stroke::new(2.0, Color32::from_black_alpha(150)));
        }
        song += seg;
        if !ac.clip.looping {
            break;
        }
        pos = ac.clip.loop_start;
    }
}
