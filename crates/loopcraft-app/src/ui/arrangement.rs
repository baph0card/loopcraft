//! Arrangement view: the song as a timeline of clips per track.

use super::theme;
use crate::app::{App, ClipRef, DetailTab};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use loopcraft_core::{ArrangementClip, Clip, ClipContent, TrackKind};
use loopcraft_engine::{Command, TrackRef};
use std::sync::atomic::Ordering::Relaxed;

const HEAD_W: f32 = 130.0;
const RULER_H: f32 = 22.0;
const LANE_H: f32 = 46.0;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let st = app.engine.state.clone();
    let bar = app.project.time_signature.beats_per_bar();

    ui.horizontal(|ui| {
        ui.label(RichText::new("Zoom").color(theme::DIM));
        if ui.button("−").clicked() {
            app.arr_pixels_per_beat = (app.arr_pixels_per_beat / 1.4).max(2.0);
        }
        if ui.button("+").clicked() {
            app.arr_pixels_per_beat = (app.arr_pixels_per_beat * 1.4).min(120.0);
        }
        ui.label(RichText::new("Click the ruler to move the playhead · right-click it for the loop · double-click a MIDI lane to add a clip · drag clips to move them").color(theme::DIM).size(11.0));
    });

    let ppb = app.arr_pixels_per_beat;
    let len = (app.project.arrangement_length() + 16.0 * bar).max(64.0 * bar);
    let width = HEAD_W + len as f32 * ppb;
    let height = RULER_H + app.project.tracks.len() as f32 * LANE_H + 20.0;

    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        let (area, _) = ui.allocate_exact_size(vec2(width, height.max(ui.available_height())), Sense::hover());
        let x_of = |beat: f64| area.left() + HEAD_W + beat as f32 * ppb;
        let beat_of = |x: f32| ((x - area.left() - HEAD_W) / ppb).max(0.0) as f64;
        let p = ui.painter().clone();
        p.rect_filled(area, 0.0, theme::BG);

        // Lanes and grid.
        let step_bars = if ppb * bar as f32 > 40.0 { 1.0 } else if ppb * bar as f32 > 12.0 { 4.0 } else { 8.0 };
        let mut b = 0.0;
        while b < len {
            let x = x_of(b);
            let strong = ((b / bar) % (step_bars * 4.0)).abs() < 1e-6;
            p.line_segment([pos2(x, area.top() + RULER_H), pos2(x, area.bottom())], Stroke::new(1.0, if strong { theme::LINE } else { theme::dim(theme::LINE, 0.6) }));
            b += bar * step_bars;
        }

        // Ruler.
        let ruler = Rect::from_min_size(pos2(area.left() + HEAD_W, area.top()), vec2(width - HEAD_W, RULER_H));
        let r_resp = ui.interact(ruler, ui.id().with("ruler"), Sense::click());
        p.rect_filled(ruler, 0.0, theme::PANEL);
        if let Some((ls, le)) = app.project.loop_region {
            let lr = Rect::from_min_max(pos2(x_of(ls), ruler.top()), pos2(x_of(le), ruler.bottom()));
            p.rect_filled(lr, 0.0, theme::dim(theme::ACCENT, 0.45));
        }
        let mut b = 0.0;
        while b < len {
            p.text(pos2(x_of(b) + 3.0, ruler.center().y), Align2::LEFT_CENTER, format!("{}", (b / bar) as i64 + 1), FontId::monospace(10.0), theme::DIM);
            b += bar * step_bars;
        }
        if r_resp.clicked() {
            if let Some(pos) = r_resp.interact_pointer_pos() {
                let beat = beat_of(pos.x).floor();
                app.send(Command::Seek(beat));
            }
        }
        let click_beat = r_resp.interact_pointer_pos().map(|p| (beat_of(p.x) / bar).floor() * bar);
        r_resp.context_menu(|ui| {
            let at = click_beat.unwrap_or(0.0);
            if ui.button("Loop 4 bars from here").clicked() {
                app.project.loop_region = Some((at, at + 4.0 * bar));
                app.send(Command::SetLoop(app.project.loop_region));
                ui.close();
            }
            if ui.button("Loop 8 bars from here").clicked() {
                app.project.loop_region = Some((at, at + 8.0 * bar));
                app.send(Command::SetLoop(app.project.loop_region));
                ui.close();
            }
            if ui.button("Loop off").clicked() {
                app.project.loop_region = None;
                app.send(Command::SetLoop(None));
                ui.close();
            }
        });

        // Track lanes.
        let mut start_drag: Option<(usize, usize, f64, f64)> = None;
        let mut select: Option<ClipRef> = None;
        let mut delete: Option<ClipRef> = None;
        let mut create: Option<(usize, f64)> = None;
        for t in 0..app.project.tracks.len() {
            let y = area.top() + RULER_H + t as f32 * LANE_H;
            let lane = Rect::from_min_size(pos2(area.left(), y), vec2(width, LANE_H));
            let track = &app.project.tracks[t];
            let following = st.tracks.get(t).map(|s| s.follows_arrangement.load(Relaxed)).unwrap_or(true);
            p.line_segment([lane.left_bottom(), lane.right_bottom()], Stroke::new(1.0, theme::LINE));

            // Header.
            let head = Rect::from_min_size(lane.min, vec2(HEAD_W, LANE_H));
            p.rect_filled(head.shrink(1.0), 2.0, theme::PANEL);
            p.rect_filled(Rect::from_min_size(head.min + vec2(1.0, 1.0), vec2(4.0, LANE_H - 2.0)), 0.0, theme::rgb(track.color));
            p.text(head.left_top() + vec2(10.0, 8.0), Align2::LEFT_TOP, &track.name, FontId::proportional(12.0), theme::TEXT);
            if !following {
                p.text(head.left_bottom() + vec2(10.0, -8.0), Align2::LEFT_BOTTOM, "playing Session", FontId::proportional(10.0), theme::ACCENT);
            }
            let h_resp = ui.interact(head, ui.id().with(("head", t)), Sense::click());
            if h_resp.clicked() {
                app.selected_track = Some(t);
                app.device_target = TrackRef::Track(t);
            }
            if app.selected_track == Some(t) {
                p.rect_stroke(head.shrink(1.0), 2.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
            }

            // Background for double-click-to-create.
            let body = Rect::from_min_max(pos2(area.left() + HEAD_W, y), lane.max);
            let b_resp = ui.interact(body, ui.id().with(("lane", t)), Sense::click());
            if b_resp.double_clicked() && track.kind == TrackKind::Midi {
                if let Some(pos) = b_resp.interact_pointer_pos() {
                    create = Some((t, (beat_of(pos.x) / bar).floor() * bar));
                }
            }

            for (i, ac) in track.arrangement.iter().enumerate() {
                let r = Rect::from_min_max(pos2(x_of(ac.start), y + 3.0), pos2(x_of(ac.end()), y + LANE_H - 3.0));
                let selected = app.selected_clip == Some(ClipRef::Arrangement { track: t, index: i });
                let base = theme::rgb(ac.clip.color);
                p.rect_filled(r, 3.0, if following { theme::dim(base, 0.75) } else { theme::dim(base, 0.35) });
                p.text(r.left_top() + vec2(4.0, 2.0), Align2::LEFT_TOP, &ac.clip.name, FontId::proportional(10.0), Color32::BLACK);
                draw_clip_preview(&p, r.shrink2(vec2(2.0, 8.0)).translate(vec2(0.0, 4.0)), ac, ppb);
                if selected {
                    p.rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
                }
                let c_resp = ui.interact(r, ui.id().with(("clip", t, i)), Sense::click_and_drag());
                if c_resp.clicked() {
                    select = Some(ClipRef::Arrangement { track: t, index: i });
                }
                if c_resp.drag_started() {
                    if let Some(pos) = c_resp.interact_pointer_pos() {
                        start_drag = Some((t, i, ac.start, beat_of(pos.x)));
                    }
                }
                c_resp.context_menu(|ui| {
                    if ui.button("Delete").clicked() {
                        delete = Some(ClipRef::Arrangement { track: t, index: i });
                        ui.close();
                    }
                });
            }
        }

        if let Some(c) = select {
            app.selected_clip = Some(c);
            app.piano_selected = None;
            app.detail_tab = DetailTab::Clip;
            if let ClipRef::Arrangement { track, .. } = c {
                app.selected_track = Some(track);
            }
        }
        if let Some(c) = delete {
            app.delete_clip(c);
        }
        if let Some((t, at)) = create {
            app.checkpoint();
            let color = app.project.tracks[t].color;
            let clip = Clip { color, ..Clip::midi("MIDI Clip", bar, Vec::new()) };
            app.project.tracks[t].arrangement.push(ArrangementClip::new(at, clip));
            let index = app.project.tracks[t].arrangement.len() - 1;
            app.selected_clip = Some(ClipRef::Arrangement { track: t, index });
            app.rebuild();
        }

        // Dragging clips (snapped to beats, or bars when zoomed out).
        if let Some(d) = start_drag {
            app.checkpoint();
            app.arr_drag = Some(d);
        }
        if let Some((t, i, orig, press)) = app.arr_drag {
            let snap = if ppb * bar as f32 > 60.0 { 1.0 } else { bar };
            if let Some(pos) = ui.ctx().pointer_interact_pos() {
                let delta = beat_of(pos.x) - press;
                let new_start = ((orig + delta) / snap).round().max(0.0) * snap;
                if let Some(ac) = app.project.tracks.get_mut(t).and_then(|tr| tr.arrangement.get_mut(i)) {
                    ac.start = new_start;
                }
            }
            if ui.input(|inp| inp.pointer.any_released()) {
                app.arr_drag = None;
                app.rebuild();
            }
        }

        // Playhead.
        let x = x_of(st.beat.load());
        p.line_segment([pos2(x, area.top()), pos2(x, area.bottom())], Stroke::new(1.5, theme::ACCENT));
        p.add(egui::Shape::convex_polygon(vec![Pos2::new(x - 5.0, area.top()), Pos2::new(x + 5.0, area.top()), Pos2::new(x, area.top() + 7.0)], theme::ACCENT, Stroke::NONE));
    });
}

fn draw_clip_preview(p: &egui::Painter, r: Rect, ac: &ArrangementClip, ppb: f32) {
    let ClipContent::Midi { notes } = &ac.clip.content else {
        p.text(r.left_center(), Align2::LEFT_CENTER, "audio", FontId::proportional(9.0), Color32::from_black_alpha(140));
        return;
    };
    if notes.is_empty() || r.height() < 6.0 {
        return;
    }
    let lo = notes.iter().map(|n| n.pitch).min().unwrap_or(60) as f32;
    let hi = notes.iter().map(|n| n.pitch).max().unwrap_or(60) as f32 + 1.0;
    let span = (hi - lo).max(1.0);
    let len = ac.clip.length.max(1e-6);
    let passes = if ac.clip.looping { (ac.duration() / len).ceil() as usize } else { 1 };
    for k in 0..passes {
        for n in notes {
            let s = k as f64 * len + n.start;
            if s >= ac.duration() {
                break;
            }
            let e = (s + n.length.min(len - n.start)).min(ac.duration());
            let x0 = r.left() + s as f32 * ppb;
            let x1 = (r.left() + e as f32 * ppb).max(x0 + 1.0);
            let y = r.bottom() - (n.pitch as f32 - lo + 0.5) / span * r.height();
            p.line_segment([pos2(x0, y), pos2(x1, y)], Stroke::new(2.0, Color32::from_black_alpha(150)));
        }
    }
}
