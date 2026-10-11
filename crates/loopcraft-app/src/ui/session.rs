//! Session view: a grid of clip slots (tracks x scenes) with a mixer below.

use super::{Icon, icon_button, meter, small_toggle, theme};
use crate::app::{App, ClipRef, DetailTab, Rename};
use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Stroke, StrokeKind, vec2};
use loopcraft_core::{ClipContent, TrackKind};
use loopcraft_engine::{Command, EngineState, TrackRef};
use std::sync::atomic::Ordering::Relaxed;

const COL_W: f32 = 112.0;
const SLOT_H: f32 = 22.0;
const HEADER_H: f32 = 22.0;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let st = app.engine.state.clone();
    let blink = (ui.input(|i| i.time) * 4.0) as i64 % 2 == 0;
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for t in 0..app.project.tracks.len() {
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
                return_column(app, ui, r, &st);
            }
            master_column(app, ui, &st);
        });
    });
}

fn header(app: &mut App, ui: &mut egui::Ui, label: &str, color: Color32, selected: bool, rename: Option<Rename>) -> egui::Response {
    if let (Some(target), Some((cur, text))) = (rename, app.renaming.as_mut())
        && *cur == target
    {
        let r = ui.add(egui::TextEdit::singleline(text).desired_width(COL_W - 8.0));
        r.request_focus();
        if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let (target, text) = app.renaming.take().expect("renaming");
            app.checkpoint();
            match target {
                Rename::Track(t) => app.project.tracks[t].name = text,
                Rename::Scene(s) => app.project.scenes[s].name = text,
            }
        }
        return r;
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(COL_W, HEADER_H), Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, if selected { color } else { theme::dim(color, 0.75) });
    p.text(rect.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(12.0), Color32::BLACK);
    if selected {
        p.rect_stroke(rect, 2.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
    }
    resp
}

fn track_column(app: &mut App, ui: &mut egui::Ui, t: usize, st: &EngineState, blink: bool) {
    let Some(ts) = st.tracks.get(t) else { return };
    let track = &app.project.tracks[t];
    let color = theme::rgb(track.color);
    let name = track.name.clone();
    let kind = track.kind;
    let playing_slot = ts.playing_slot.load(Relaxed);
    let queued_slot = ts.queued_slot.load(Relaxed);
    let stop_queued = ts.stop_queued.load(Relaxed);
    let rec_slot = ts.recording_slot.load(Relaxed);
    let clip_pos = ts.clip_position.load();
    let has_clips = track.slots.iter().any(Option::is_some);

    ui.vertical(|ui| {
        ui.set_width(COL_W);
        let resp = header(app, ui, &name, color, app.selected_track == Some(t), Some(Rename::Track(t)));
        if resp.clicked() {
            app.selected_track = Some(t);
            app.device_target = TrackRef::Track(t);
        }
        if resp.double_clicked() {
            app.renaming = Some((Rename::Track(t), name.clone()));
        }
        resp.context_menu(|ui| {
            if ui.button("Rename").clicked() {
                app.renaming = Some((Rename::Track(t), name.clone()));
                ui.close();
            }
            if ui.button("Duplicate track").clicked() {
                app.duplicate_track(t);
                ui.close();
            }
            if ui.button("Delete track").clicked() {
                app.delete_track(t);
                ui.close();
            }
        });
        if t >= app.project.tracks.len() {
            return;
        }

        for s in 0..app.project.scenes.len() {
            let (rect, resp) = ui.allocate_exact_size(vec2(COL_W, SLOT_H), Sense::click());
            let clip = app.project.tracks[t].slots.get(s).and_then(|c| c.as_ref());
            let selected = app.selected_clip == Some(ClipRef::Session { track: t, slot: s });
            let p = ui.painter();
            let inner = rect.shrink2(vec2(1.0, 1.0));
            match clip {
                Some(c) => {
                    let is_playing = playing_slot == s as i32;
                    let is_queued = queued_slot == s as i32;
                    let base = theme::rgb(c.color);
                    p.rect_filled(inner, 2.0, if is_playing { base } else { theme::dim(base, 0.6) });
                    // Launch triangle.
                    let tri_color = if is_playing || (is_queued && blink) { theme::PLAY } else { Color32::from_black_alpha(160) };
                    let c0 = inner.left_center() + vec2(8.0, 0.0);
                    p.add(egui::Shape::convex_polygon(vec![c0 + vec2(-4.0, -5.0), c0 + vec2(-4.0, 5.0), c0 + vec2(4.0, 0.0)], tri_color, Stroke::NONE));
                    let label = match &c.content {
                        ClipContent::Audio(_) => format!("♪ {}", c.name),
                        _ => c.name.clone(),
                    };
                    p.text(inner.left_center() + vec2(16.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(11.0), Color32::BLACK);
                    if is_playing {
                        let frac = (clip_pos / c.length.max(1e-6)).clamp(0.0, 1.0) as f32;
                        let bar = egui::Rect::from_min_size(egui::pos2(inner.left(), inner.bottom() - 3.0), vec2(inner.width() * frac, 3.0));
                        p.rect_filled(bar, 0.0, Color32::from_black_alpha(140));
                    }
                }
                None => {
                    let rec_here = rec_slot == s as i32;
                    p.rect_filled(inner, 2.0, if resp.hovered() { theme::SLOT_HOVER } else { theme::SLOT });
                    if rec_here {
                        p.circle_filled(inner.center(), 5.0, if blink { theme::REC } else { theme::dim(theme::REC, 0.5) });
                    } else if has_clips && kind != TrackKind::Return {
                        let sq = egui::Rect::from_center_size(inner.center(), vec2(7.0, 7.0));
                        p.rect_filled(sq, 0.0, theme::LINE);
                    }
                }
            }
            if selected {
                p.rect_stroke(inner, 2.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
            }

            let filled = clip.is_some();
            if resp.clicked() {
                app.selected_track = Some(t);
                app.device_target = TrackRef::Track(t);
                app.selected_clip = Some(ClipRef::Session { track: t, slot: s });
                app.piano_selected = None;
                if filled {
                    app.detail_tab = DetailTab::Clip;
                }
                app.send(Command::LaunchClip { track: t, slot: s });
            }
            if resp.double_clicked() && !filled {
                app.new_midi_clip(t, s);
            }
            resp.context_menu(|ui| {
                if filled {
                    if ui.button("Duplicate to next slot  (Ctrl+D)").clicked() {
                        app.duplicate_clip_down(t, s);
                        ui.close();
                    }
                    if ui.button("Copy to Arrangement at playhead").clicked() {
                        app.clip_to_arrangement(t, s);
                        ui.close();
                    }
                    if ui.button("Delete clip").clicked() {
                        app.delete_clip(ClipRef::Session { track: t, slot: s });
                        ui.close();
                    }
                } else if kind == TrackKind::Midi {
                    if ui.button("Insert MIDI clip").clicked() {
                        app.new_midi_clip(t, s);
                        ui.close();
                    }
                } else {
                    ui.label(RichText::new("Add audio from the Samples browser").color(theme::DIM));
                }
            });
        }

        // Track stop button.
        if icon_button(ui, Icon::Stop, stop_queued && blink, theme::ACCENT, vec2(COL_W, 18.0)).on_hover_text("Stop this track").clicked() {
            app.send(Command::StopTrack { track: t });
        }
        ui.add_space(6.0);
        mixer_strip(app, ui, TrackRef::Track(t), st);
    });
}

fn return_column(app: &mut App, ui: &mut egui::Ui, r: usize, st: &EngineState) {
    ui.vertical(|ui| {
        ui.set_width(COL_W);
        let name = app.project.returns[r].name.clone();
        let resp = header(app, ui, &name, theme::DIM, app.device_target == TrackRef::Return(r), None);
        if resp.clicked() {
            app.device_target = TrackRef::Return(r);
            app.detail_tab = DetailTab::Devices;
        }
        let h = app.project.scenes.len() as f32 * SLOT_H + 18.0 + ui.spacing().item_spacing.y;
        let (rect, _) = ui.allocate_exact_size(vec2(COL_W, h), Sense::hover());
        let fx: Vec<&str> = app.project.returns[r].effects.iter().map(|d| d.name()).collect();
        ui.painter().text(rect.center_top() + vec2(0.0, 12.0), Align2::CENTER_CENTER, fx.join(", "), FontId::proportional(10.0), theme::DIM);
        ui.add_space(6.0);
        mixer_strip(app, ui, TrackRef::Return(r), st);
    });
}

fn master_column(app: &mut App, ui: &mut egui::Ui, st: &EngineState) {
    ui.vertical(|ui| {
        ui.set_width(COL_W + 20.0);
        let resp = header(app, ui, "Master", theme::ACCENT, app.device_target == TrackRef::Master, None);
        if resp.clicked() {
            app.device_target = TrackRef::Master;
            app.detail_tab = DetailTab::Devices;
        }
        for s in 0..app.project.scenes.len() {
            let scene = &app.project.scenes[s];
            let label = match scene.tempo {
                Some(bpm) => format!("▶ {}   {bpm:.0}", scene.name),
                None => format!("▶ {}", scene.name),
            };
            let name = scene.name.clone();
            if app.renaming.as_ref().is_some_and(|(r, _)| *r == Rename::Scene(s)) {
                header(app, ui, "", theme::SLOT, false, Some(Rename::Scene(s)));
                continue;
            }
            let resp = ui.add(egui::Button::new(RichText::new(label).size(11.0)).min_size(vec2(COL_W + 20.0, SLOT_H - 2.0)).fill(theme::SLOT));
            if resp.clicked() {
                app.send(Command::LaunchScene { scene: s });
            }
            if resp.double_clicked() {
                app.renaming = Some((Rename::Scene(s), name.clone()));
            }
            resp.context_menu(|ui| {
                if ui.button("Rename").clicked() {
                    app.renaming = Some((Rename::Scene(s), name.clone()));
                    ui.close();
                }
                let mut has_tempo = app.project.scenes[s].tempo.is_some();
                if ui.checkbox(&mut has_tempo, "Scene sets tempo").changed() {
                    app.checkpoint();
                    app.project.scenes[s].tempo = has_tempo.then_some(app.project.tempo);
                    app.rebuild();
                }
                if let Some(mut bpm) = app.project.scenes[s].tempo {
                    if ui.add(egui::DragValue::new(&mut bpm).range(20.0..=999.0).speed(0.1).suffix(" bpm")).changed() {
                        app.project.scenes[s].tempo = Some(bpm);
                        app.dirty = true;
                    }
                    if ui.button("Apply").clicked() {
                        app.rebuild();
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("Insert scene above").clicked() {
                    app.insert_scene(s);
                    ui.close();
                }
                if ui.button("Delete scene").clicked() {
                    app.delete_scene(s);
                    ui.close();
                }
            });
        }
        if ui.add(egui::Button::new(RichText::new("Stop All Clips").size(11.0)).min_size(vec2(COL_W + 20.0, 18.0))).clicked() {
            app.send(Command::StopAllClips);
        }
        ui.add_space(6.0);
        mixer_strip(app, ui, TrackRef::Master, st);
    });
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
        // Sends
        if let TrackRef::Track(t) = ch {
            for r in 0..app.project.returns.len() {
                let letter = (b'A' + r as u8) as char;
                let mut db = app.project.tracks[t].sends.get(r).copied().unwrap_or(-70.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(letter.to_string()).size(10.0).color(theme::DIM));
                    let resp = ui.add(egui::Slider::new(&mut db, -70.0..=6.0).show_value(false));
                    if resp.changed() {
                        if let Some(s) = app.project.tracks[t].sends.get_mut(r) {
                            *s = db;
                        }
                        app.dirty = true;
                        app.send(Command::SetSend { track: t, ret: r, db });
                    }
                    resp.on_hover_text(format!("Send {letter}: {db:.1} dB"));
                });
            }
        }
        // Pan
        let mut pan = mix.pan;
        let resp = ui.add(egui::Slider::new(&mut pan, -1.0..=1.0).show_value(false)).on_hover_text("Pan (double-click to center)");
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
            meter(ui, pl, pr, 100.0);
            ui.vertical(|ui| {
                let active = !mix.mute;
                if small_toggle(ui, active, "On", theme::ACCENT).on_hover_text("Track activator").clicked() {
                    if let Some(m) = app.mixer_mut(ch) {
                        m.mute = active;
                    }
                    app.send(Command::SetMute { channel: ch, mute: active });
                }
                if let TrackRef::Track(t) = ch {
                    if small_toggle(ui, mix.solo, "S", theme::SOLO).on_hover_text("Solo").clicked() {
                        let solo = !mix.solo;
                        app.project.tracks[t].mixer.solo = solo;
                        app.send(Command::SetSolo { track: t, solo });
                    }
                    let armed = app.selected_track == Some(t);
                    if icon_button(ui, Icon::Record, armed, theme::REC, vec2(22.0, 18.0)).on_hover_text("Arm for recording / computer keys").clicked() {
                        app.selected_track = Some(t);
                    }
                }
            });
        });
        ui.label(RichText::new(format!("{:+.1} dB", mix.volume_db)).size(10.0).color(theme::DIM));
    });
}
