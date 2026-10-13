//! The device chain: MIDI effects → instrument → audio effects, with racks
//! (chains + macros), bypass, presets and a generic editor for the
//! non-numeric settings every device has.

use super::{knob, mappable, small_toggle, theme};
use crate::app::{App, View};
use eframe::egui::{self, RichText, Stroke};
use loopcraft_core::*;
use loopcraft_dsp::specs;
use loopcraft_engine::{DeviceRef, TrackRef};
use serde_json::Value;

type Path = Vec<(usize, usize)>;

pub fn device_chain(app: &mut App, ui: &mut egui::Ui) {
    let ch = app.device_target;
    let Some(t) = app.channel(ch) else {
        ui.label(RichText::new("Select a track.").color(theme::DIM));
        return;
    };
    let midi_fx = t.midi_effects.clone();
    let instrument = t.instrument.clone().filter(|_| t.kind == TrackKind::Midi);
    let effects = t.effects.clone();
    let is_group = t.kind == TrackKind::Group;
    egui::ScrollArea::both().id_salt("chain").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for (i, spec) in midi_fx.iter().enumerate() {
                device_box(app, ui, ch, DeviceRef::MidiEffect(i), &Vec::new(), spec, Some((i, midi_fx.len())));
            }
            if let Some(spec) = instrument {
                device_box(app, ui, ch, DeviceRef::Instrument, &Vec::new(), &spec, None);
            } else if matches!(ch, TrackRef::Track(_)) && !is_group && midi_fx.is_empty() && effects.is_empty() {
                ui.label(RichText::new("Add devices from the browser on the left.").color(theme::DIM));
            }
            for (i, spec) in effects.iter().enumerate() {
                device_box(app, ui, ch, DeviceRef::Effect(i), &Vec::new(), spec, Some((i, effects.len())));
            }
            if effects.is_empty() && !matches!(ch, TrackRef::Track(_)) {
                ui.label(RichText::new("Add audio effects from the browser on the left.").color(theme::DIM));
            }
        });
    });
}

fn title_color(spec: &DeviceSpec) -> egui::Color32 {
    match spec.kind() {
        DeviceKind::Instrument => theme::ACCENT,
        DeviceKind::MidiEffect => theme::MIDI_MAP,
        DeviceKind::AudioEffect => theme::TEXT,
    }
}

fn box_key(ch: TrackRef, dev: DeviceRef, path: &Path) -> String {
    format!("{ch:?}/{dev:?}/{path:?}")
}

/// One device. `pos` is (index, count) for reorderable top-level devices.
fn device_box(app: &mut App, ui: &mut egui::Ui, ch: TrackRef, dev: DeviceRef, path: &Path, spec: &DeviceSpec, pos: Option<(usize, usize)>) {
    let top = path.is_empty();
    let bypassed = top && app.is_bypassed(ch, dev);
    let collapsed = top && app.collapsed_devices.contains(&(ch, dev));
    let stroke = if app.macro_learn.is_some_and(|m| m.channel == ch && m.device == dev) && top {
        Stroke::new(1.5, theme::ACCENT)
    } else {
        Stroke::new(1.0, theme::LINE)
    };
    egui::Frame::new().fill(if bypassed { theme::dim(theme::BG, 0.8) } else { theme::BG }).stroke(stroke).corner_radius(4).inner_margin(6).show(ui, |ui| {
        ui.vertical(|ui| {
            let w = match spec {
                DeviceSpec::AudioRack(_) | DeviceSpec::InstrumentRack(_) => 300.0,
                _ if collapsed => 90.0,
                _ => 236.0,
            };
            ui.set_min_width(w);
            ui.set_max_width(w);
            ui.horizontal(|ui| {
                if top {
                    if small_toggle(ui, !bypassed, "On", theme::ACCENT).on_hover_text("Device on/off").clicked() {
                        app.set_bypass(ch, dev, !bypassed);
                    }
                    if ui.small_button(if collapsed { "+" } else { "-" }).on_hover_text("Fold").clicked() {
                        if collapsed {
                            app.collapsed_devices.retain(|d| *d != (ch, dev));
                        } else {
                            app.collapsed_devices.push((ch, dev));
                        }
                    }
                }
                ui.label(RichText::new(spec.name()).strong().color(if bypassed { theme::DIM } else { title_color(spec) }));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if top {
                        if dev != DeviceRef::Instrument || matches!(ch, TrackRef::Track(_)) {
                            if ui.small_button("x").on_hover_text("Remove").clicked() {
                                app.remove_device(ch, dev);
                            }
                        }
                        if let Some((i, n)) = pos {
                            if i + 1 < n && ui.small_button("▶").on_hover_text("Move right").clicked() {
                                app.move_device(ch, dev, false);
                            }
                            if i > 0 && ui.small_button("◀").on_hover_text("Move left").clicked() {
                                app.move_device(ch, dev, true);
                            }
                        }
                        ui.menu_button("...", |ui| {
                            if ui.button("Save preset…").clicked() {
                                app.open_preset_dialog(spec.clone());
                                ui.close();
                            }
                            if !spec.is_midi_effect()
                                && !matches!(spec, DeviceSpec::AudioRack(_) | DeviceSpec::InstrumentRack(_))
                                && ui.button("Group into a rack").clicked()
                            {
                                app.wrap_in_rack(ch, dev);
                                ui.close();
                            }
                        });
                    }
                });
            });
            if collapsed {
                return;
            }
            ui.separator();
            if let DeviceSpec::Plugin(p) = spec {
                ui.label(RichText::new(format!("CLAP · {}", p.id)).size(10.0).color(theme::DIM));
                if specs::param_infos(spec).is_empty() {
                    ui.label(RichText::new("No parameters exposed (or the plugin failed to load).").size(10.0).color(theme::DIM));
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt(("plugparams", box_key(ch, dev, path)))
                        .max_height(220.0)
                        .show(ui, |ui| params(app, ui, ch, dev, path, spec));
                }
                ui.label(RichText::new("Plugin editor windows aren't supported yet.").size(9.0).color(theme::DIM));
                return;
            }
            if let DeviceSpec::AudioRack(r) | DeviceSpec::InstrumentRack(r) = spec {
                rack_body(app, ui, ch, dev, path, r, matches!(spec, DeviceSpec::InstrumentRack(_)));
                return;
            }
            params(app, ui, ch, dev, path, spec);
            options_editor(app, ui, ch, dev, path, spec);
            match spec {
                DeviceSpec::DrumRack(d) => drum_pads(app, ui, ch, dev, path, d),
                DeviceSpec::MultiSampler(m) => zones(app, ui, ch, dev, path, m),
                _ => {}
            }
        });
    });
}

fn params(app: &mut App, ui: &mut egui::Ui, ch: TrackRef, dev: DeviceRef, path: &Path, spec: &DeviceSpec) {
    let learn = app.macro_learn.filter(|m| m.channel == ch && m.device == dev && path.len() == 1);
    for (i, info) in specs::param_infos(spec).iter().enumerate() {
        let Some(mut v) = specs::get_param(spec, i) else { continue };
        let log = matches!(info.unit, "Hz" | "ms" | "s") && info.min > 0.0;
        let row = ui.horizontal(|ui| {
            let label =
                ui.add_sized([70.0, 16.0], egui::Label::new(RichText::new(info.name).size(11.0).color(theme::DIM)).truncate().sense(egui::Sense::click()));
            ui.spacing_mut().slider_width = 92.0;
            let slider = egui::Slider::new(&mut v, info.min..=info.max).logarithmic(log).suffix(format!(" {}", info.unit)).max_decimals(2);
            if ui.add(slider).changed() {
                if path.is_empty() {
                    app.set_param(ch, dev, i, v);
                } else {
                    app.set_nested_param(ch, dev, path, i, v);
                }
            }
            if let Some(m) = learn
                && ui.small_button("M").on_hover_text(format!("Map Macro {} here", m.macro_index + 1)).clicked()
            {
                let (c, d) = path[0];
                app.map_macro(c, d, i);
            }
            label
        });
        let label = row.inner;
        // Automation and mapping only for top-level devices on regular tracks.
        if let TrackRef::Track(t) = ch
            && path.is_empty()
        {
            let auto = match dev {
                DeviceRef::Instrument => Some(AutomationTarget::Instrument { param: i }),
                DeviceRef::Effect(e) => Some(AutomationTarget::Effect { index: e, param: i }),
                DeviceRef::MidiEffect(_) => None,
            };
            if let Some(target) = auto {
                label.context_menu(|ui| {
                    if ui.button("Show automation lane").clicked() {
                        app.automation_view.insert(t, target);
                        app.view = View::Arrangement;
                        ui.close();
                    }
                });
            }
            let map = match dev {
                DeviceRef::Instrument => Some(MappingTarget::InstrumentParam { track: t, param: i }),
                DeviceRef::Effect(e) => Some(MappingTarget::DeviceParam { track: t, device: e, param: i }),
                DeviceRef::MidiEffect(_) => None,
            };
            if let Some(m) = map {
                mappable(app, ui, row.response.rect, m);
            }
        }
    }
}

fn rack_body(app: &mut App, ui: &mut egui::Ui, ch: TrackRef, dev: DeviceRef, path: &Path, rack: &RackParams, inst: bool) {
    let top = path.is_empty();
    // Macros.
    ui.horizontal_wrapped(|ui| {
        for (i, m) in rack.macros.iter().enumerate().take(8) {
            ui.vertical(|ui| {
                ui.set_width(62.0);
                let mut v = m.value;
                let learning = app.macro_learn.is_some_and(|l| l.channel == ch && l.device == dev && l.macro_index == i) && top;
                let r = knob(
                    ui,
                    &mut v,
                    0.0,
                    1.0,
                    0.0,
                    30.0,
                    if learning {
                        theme::REC
                    } else if m.mappings.is_empty() {
                        theme::DIM
                    } else {
                        theme::ACCENT
                    },
                );
                if r.changed() {
                    if top {
                        app.set_param(ch, dev, i, v);
                    } else {
                        app.set_nested_param(ch, dev, path, i, v);
                    }
                }
                r.on_hover_text(format!("{}: {:.0}% · {} mapping(s)", m.name, v * 100.0, m.mappings.len())).context_menu(|ui| {
                    let mut name = m.name.clone();
                    if ui.text_edit_singleline(&mut name).changed() {
                        let p = path.clone();
                        app.edit_rack(ch, dev, &p, |r| r.macros[i].name = name.clone());
                    }
                    for (j, mm) in m.mappings.iter().enumerate() {
                        let target = rack.chains.get(mm.chain).and_then(|c| c.devices.get(mm.device));
                        let pname = target.and_then(|s| specs::param_infos(s).get(mm.param)).map(|p| p.name).unwrap_or("?");
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} › {}", target.map(|s| s.name()).unwrap_or("?"), pname)).size(10.0));
                            let (mut lo, mut hi) = (mm.min, mm.max);
                            let a = ui.add(egui::DragValue::new(&mut lo).speed(0.01).prefix("min "));
                            let b = ui.add(egui::DragValue::new(&mut hi).speed(0.01).prefix("max "));
                            let p = path.clone();
                            if a.changed() || b.changed() {
                                app.edit_rack(ch, dev, &p, |r| {
                                    r.macros[i].mappings[j].min = lo;
                                    r.macros[i].mappings[j].max = hi;
                                });
                            }
                            if ui.small_button("x").clicked() {
                                app.checkpoint();
                                app.edit_rack(ch, dev, &p, |r| {
                                    r.macros[i].mappings.remove(j);
                                });
                                ui.close();
                            }
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&m.name).size(9.0).color(theme::DIM));
                    if top && small_toggle(ui, learning, "Map", theme::REC).on_hover_text("Then click M next to a parameter in a chain").clicked() {
                        app.start_macro_learn(ch, dev, i);
                        let key = box_key(ch, dev, path);
                        app.open_chain.entry(key).or_insert(0);
                    }
                });
            });
        }
    });
    ui.separator();
    // Chains.
    let key = box_key(ch, dev, path);
    let open = app.open_chain.get(&key).copied().unwrap_or(0).min(rack.chains.len().saturating_sub(1));
    let mut select = None;
    for (c, chain) in rack.chains.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui.selectable_label(open == c, RichText::new(&chain.name).size(11.0)).clicked() {
                select = Some(c);
            }
            let p = path.clone();
            if small_toggle(ui, !chain.mute, "On", theme::ACCENT).clicked() {
                app.checkpoint();
                app.edit_rack(ch, dev, &p, |r| r.chains[c].mute = !r.chains[c].mute);
            }
            let mut vol = chain.volume_db;
            if knob(ui, &mut vol, -60.0, 6.0, 0.0, 18.0, theme::DIM).on_hover_text(format!("Chain volume {vol:.1} dB")).changed() {
                app.edit_rack(ch, dev, &p, |r| r.chains[c].volume_db = vol);
            }
            let mut pan = chain.pan;
            if knob(ui, &mut pan, -1.0, 1.0, 0.0, 18.0, theme::DIM).on_hover_text(format!("Chain pan {pan:+.2}")).changed() {
                app.edit_rack(ch, dev, &p, |r| r.chains[c].pan = pan);
            }
            if inst {
                let (mut lo, mut hi) = chain.key_range;
                let a = ui.add(egui::DragValue::new(&mut lo).range(0..=127).prefix("key "));
                let b = ui.add(egui::DragValue::new(&mut hi).range(0..=127).prefix("to "));
                let (mut vlo, mut vhi) = chain.velocity_range;
                let c1 = ui.add(egui::DragValue::new(&mut vlo).range(1..=127).prefix("vel "));
                let c2 = ui.add(egui::DragValue::new(&mut vhi).range(1..=127).prefix("to "));
                if a.changed() || b.changed() || c1.changed() || c2.changed() {
                    app.edit_rack(ch, dev, &p, |r| {
                        r.chains[c].key_range = (lo.min(hi), hi.max(lo));
                        r.chains[c].velocity_range = (vlo.min(vhi), vhi.max(vlo));
                    });
                }
            }
            if rack.chains.len() > 1 && ui.small_button("x").on_hover_text("Delete chain").clicked() {
                app.remove_rack_chain(ch, dev, &p, c);
            }
        });
    }
    if let Some(c) = select {
        app.open_chain.insert(key.clone(), c);
    }
    ui.horizontal(|ui| {
        if ui.small_button("+ Chain").clicked() {
            let p = path.clone();
            app.add_rack_chain(ch, dev, &p);
        }
        if top {
            let adding = app.rack_insert == Some((ch, dev, open));
            if small_toggle(ui, adding, "Add from browser", theme::ACCENT).on_hover_text("Browser clicks add devices into the selected chain").clicked() {
                app.rack_insert = if adding { None } else { Some((ch, dev, open)) };
            }
        }
    });
    // The open chain's devices.
    if let Some(chain) = rack.chains.get(open) {
        ui.separator();
        if chain.devices.is_empty() {
            ui.label(RichText::new(if inst { "Empty chain: add an instrument" } else { "Empty chain: add effects" }).size(10.0).color(theme::DIM));
        }
        ui.horizontal_top(|ui| {
            for (d, spec) in chain.devices.iter().enumerate() {
                let mut sub = path.clone();
                sub.push((open, d));
                ui.vertical(|ui| {
                    device_box(app, ui, ch, dev, &sub, spec, None);
                    if top && ui.small_button("Remove").clicked() {
                        app.remove_from_rack_chain(ch, dev, &[], open, d);
                    }
                });
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Generic editor for non-numeric settings
// ---------------------------------------------------------------------------

fn enum_names<T: serde::Serialize>(v: &[T]) -> Vec<String> {
    v.iter().filter_map(|x| serde_json::to_value(x).ok()).filter_map(|j| j.as_str().map(str::to_string)).collect()
}

fn enum_lists() -> Vec<(&'static [&'static str], Vec<String>)> {
    use loopcraft_core::{
        AmpModel as A, ArpStyle as S, EqBandKind as E, FilterMode as F, LfoShape as L, VocoderCarrier as V, Waveform as W, WavetableKind as T,
    };
    vec![
        (&["table1", "table2", "table"], enum_names(&[T::Basic, T::Harmonics, T::Pulse, T::Formant, T::Sync])),
        (&["osc1", "osc2", "waveform", "wave"], enum_names(&[W::Saw, W::Square, W::Triangle, W::Sine, W::Noise])),
        (&["filter_mode", "mode", "filter"], enum_names(&[F::LowPass, F::HighPass, F::BandPass])),
        (&["style"], enum_names(&[S::Up, S::Down, S::UpDown, S::DownUp, S::AsPlayed, S::Random, S::Chord])),
        (&["scale"], enum_names(&ScaleKind::ALL)),
        (&["kind"], enum_names(&[E::LowCut, E::LowShelf, E::Peak, E::HighShelf, E::HighCut, E::Notch])),
        (&["model"], enum_names(&[A::Clean, A::Crunch, A::Lead, A::Bass])),
        (&["shape", "lfo_shape"], enum_names(&[L::Sine, L::Triangle, L::Square, L::SawUp, L::SawDown])),
        (&["carrier"], enum_names(&[V::Saw, V::Noise, V::External])),
    ]
}

const SKIP: &[&str] = &["chains", "macros", "pads", "zones", "name", "state"];

struct Ctx<'a> {
    covered: Vec<String>,
    lists: Vec<(&'static [&'static str], Vec<String>)>,
    tracks: Vec<String>,
    samples: &'a [String],
    id: String,
    changed: bool,
    /// Second pass: plain numbers only (shown under "More").
    numbers: bool,
}

fn label_of(key: &str) -> String {
    let s = key.replace('_', " ");
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn walk(ui: &mut egui::Ui, cx: &mut Ctx, v: &mut Value, path: &str, key: &str, label: &str) {
    if SKIP.contains(&key) && !path.contains('/') {
        return;
    }
    if cx.covered.iter().any(|c| c == path) {
        return;
    }
    match v {
        Value::Object(m) => {
            for (k, child) in m.iter_mut() {
                let p = if path.is_empty() { k.clone() } else { format!("{path}/{k}") };
                let l = if label.is_empty() { label_of(k) } else { format!("{label} {}", label_of(k).to_lowercase()) };
                walk(ui, cx, child, &p, k, &l);
            }
        }
        Value::Array(items) if items.iter().all(|x| x.is_number()) && !items.is_empty() => {
            if cx.numbers != (key != "intervals") {
                return;
            }
            // Small numeric tuples (ranges) or lists (chord intervals).
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(11.0).color(theme::DIM));
                for x in items.iter_mut() {
                    let mut f = x.as_f64().unwrap_or(0.0);
                    if ui.add(egui::DragValue::new(&mut f).speed(0.2).max_decimals(if x.is_f64() { 2 } else { 0 })).changed() {
                        *x = if x.is_f64() { Value::from(f) } else { Value::from(f.round() as i64) };
                        cx.changed = true;
                    }
                }
                if key == "intervals" {
                    if ui.small_button("+").clicked() {
                        items.push(Value::from(12));
                        cx.changed = true;
                    }
                    if items.len() > 1 && ui.small_button("−").clicked() {
                        items.pop();
                        cx.changed = true;
                    }
                }
            });
        }
        Value::Array(items) => {
            for (i, child) in items.iter_mut().enumerate() {
                let p = format!("{path}/{i}");
                walk(ui, cx, child, &p, key, &format!("{} {}", label, i + 1));
            }
        }
        Value::Bool(_) | Value::String(_) | Value::Null if cx.numbers => {}
        Value::Number(_) if cx.numbers != (key != "sidechain_track") => {}
        Value::Bool(b) => {
            if ui.checkbox(b, RichText::new(label).size(11.0)).changed() {
                cx.changed = true;
            }
        }
        Value::String(s) => {
            if key.ends_with("file") {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(label).size(11.0).color(theme::DIM));
                    egui::ComboBox::from_id_salt((&cx.id, path))
                        .width(150.0)
                        .selected_text(RichText::new(if s.is_empty() { "(none)" } else { s.as_str() }).size(10.0))
                        .show_ui(ui, |ui| {
                            for f in cx.samples {
                                if ui.selectable_label(s == f, f).clicked() {
                                    *s = f.clone();
                                    cx.changed = true;
                                }
                            }
                        });
                });
                return;
            }
            let list =
                cx.lists.iter().find(|(keys, vals)| keys.contains(&key) && vals.contains(s)).or_else(|| cx.lists.iter().find(|(_, vals)| vals.contains(s)));
            if let Some((_, vals)) = list {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(label).size(11.0).color(theme::DIM));
                    egui::ComboBox::from_id_salt((&cx.id, path)).width(110.0).selected_text(s.as_str()).show_ui(ui, |ui| {
                        for opt in vals {
                            if ui.selectable_label(s == opt, opt).clicked() {
                                *s = opt.clone();
                                cx.changed = true;
                            }
                        }
                    });
                });
            }
        }
        Value::Null | Value::Number(_) if key == "sidechain_track" => {
            let cur = v.as_u64().map(|x| x as usize);
            let name = cur.and_then(|i| cx.tracks.get(i).cloned()).unwrap_or_else(|| "Off".into());
            let mut sel = cur;
            ui.horizontal(|ui| {
                ui.label(RichText::new("Sidechain").size(11.0).color(theme::DIM));
                egui::ComboBox::from_id_salt((&cx.id, path)).width(120.0).selected_text(name).show_ui(ui, |ui| {
                    ui.selectable_value(&mut sel, None, "Off");
                    for (i, n) in cx.tracks.iter().enumerate() {
                        ui.selectable_value(&mut sel, Some(i), n);
                    }
                });
            });
            if sel != cur {
                *v = sel.map_or(Value::Null, |i| Value::from(i as u64));
                cx.changed = true;
            }
        }
        Value::Number(n) => {
            // Numbers the parameter table doesn't cover (root keys, voices, ...).
            let (is_u, is_i) = (n.is_u64(), n.is_i64());
            let int = is_u || is_i;
            let mut f = n.as_f64().unwrap_or(0.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(11.0).color(theme::DIM));
                if ui.add(egui::DragValue::new(&mut f).speed(if int { 0.2 } else { 0.01 }).max_decimals(if int { 0 } else { 3 })).changed() {
                    *v = if is_u {
                        Value::from(f.round().max(0.0) as u64)
                    } else if is_i {
                        Value::from(f.round() as i64)
                    } else {
                        Value::from(f)
                    };
                    cx.changed = true;
                }
            });
        }
        _ => {}
    }
}

fn write_back(app: &mut App, ch: TrackRef, dev: DeviceRef, path: &Path, new: DeviceSpec) {
    app.checkpoint();
    if let Some(s) = app.spec_mut(ch, dev).and_then(|s| crate::app::nested_mut(s, path)) {
        *s = new;
    }
    app.rebuild_pending = true;
}

fn options_editor(app: &mut App, ui: &mut egui::Ui, ch: TrackRef, dev: DeviceRef, path: &Path, spec: &DeviceSpec) {
    let Ok(mut v) = serde_json::to_value(spec) else { return };
    let Some((variant, inner)) = v.as_object_mut().and_then(|o| o.iter_mut().next()) else { return };
    let variant = variant.clone();
    let samples = app.sample_files();
    let mut cx = Ctx {
        covered: specs::param_infos(spec).iter().map(|p| p.path.to_string()).collect(),
        lists: enum_lists(),
        tracks: app.project.tracks.iter().map(|t| t.name.clone()).collect(),
        samples: &samples,
        id: box_key(ch, dev, path),
        changed: false,
        numbers: false,
    };
    walk(ui, &mut cx, inner, "", "", "");
    cx.numbers = true;
    egui::CollapsingHeader::new(RichText::new("More").size(10.0).color(theme::DIM)).id_salt(("more", &cx.id)).show(ui, |ui| {
        walk(ui, &mut cx, inner, "", "", "");
    });
    if cx.changed
        && let Ok(new) = serde_json::from_value::<DeviceSpec>(serde_json::json!({ variant: inner.clone() }))
    {
        write_back(app, ch, dev, path, new);
    }
}

fn drum_pads(app: &mut App, ui: &mut egui::Ui, ch: TrackRef, dev: DeviceRef, path: &Path, d: &DrumRackParams) {
    let samples = app.sample_files();
    let mut d2 = d.clone();
    let mut changed = false;
    egui::CollapsingHeader::new(RichText::new(format!("Pads ({})", d.pads.len())).size(11.0)).id_salt(("pads", box_key(ch, dev, path))).show(ui, |ui| {
        let mut remove = None;
        for (i, pad) in d2.pads.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                changed |= ui.add(egui::DragValue::new(&mut pad.note).range(0..=127)).on_hover_text("MIDI note").changed();
                let label = match &pad.source {
                    PadSource::Synth(v) => format!("{v:?}"),
                    PadSource::Sample(f) => std::path::Path::new(f).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                };
                egui::ComboBox::from_id_salt(("padsrc", i, box_key(ch, dev, path))).width(90.0).selected_text(RichText::new(label).size(10.0)).show_ui(
                    ui,
                    |ui| {
                        use DrumVoice::*;
                        for v in [Kick, Snare, ClosedHat, OpenHat, Clap, Tom, Rim, Cowbell] {
                            if ui.selectable_label(matches!(&pad.source, PadSource::Synth(x) if *x == v), format!("{v:?}")).clicked() {
                                pad.source = PadSource::Synth(v);
                                changed = true;
                            }
                        }
                        ui.separator();
                        for f in &samples {
                            if ui.selectable_label(matches!(&pad.source, PadSource::Sample(x) if x == f), f).clicked() {
                                pad.source = PadSource::Sample(f.clone());
                                changed = true;
                            }
                        }
                    },
                );
                changed |= ui.add(egui::DragValue::new(&mut pad.gain_db).range(-40.0..=12.0).speed(0.1).suffix("dB")).changed();
                changed |= ui.add(egui::DragValue::new(&mut pad.tune_semitones).range(-24.0..=24.0).speed(0.1).suffix("st")).changed();
                let mut choke = pad.choke_group.unwrap_or(0);
                if ui.add(egui::DragValue::new(&mut choke).range(0..=8).prefix("ch ")).on_hover_text("Choke group (0 = none)").changed() {
                    pad.choke_group = (choke > 0).then_some(choke);
                    changed = true;
                }
                if ui.small_button("x").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            d2.pads.remove(i);
            changed = true;
        }
        if ui.small_button("+ Pad").clicked() {
            let note = d2.pads.iter().map(|p| p.note).max().map_or(36, |n| (n + 1).min(127));
            d2.pads.push(DrumPad { note, ..Default::default() });
            changed = true;
        }
    });
    if changed {
        write_back(app, ch, dev, path, DeviceSpec::DrumRack(d2));
    }
}

fn zones(app: &mut App, ui: &mut egui::Ui, ch: TrackRef, dev: DeviceRef, path: &Path, m: &MultiSamplerParams) {
    let samples = app.sample_files();
    let mut m2 = m.clone();
    let mut changed = false;
    egui::CollapsingHeader::new(RichText::new(format!("Zones ({})", m.zones.len())).size(11.0))
        .id_salt(("zones", box_key(ch, dev, path)))
        .default_open(m.zones.is_empty())
        .show(ui, |ui| {
            let mut remove = None;
            for (i, z) in m2.zones.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let name = std::path::Path::new(&z.file).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.label(RichText::new(name).size(10.0));
                    changed |= ui.add(egui::DragValue::new(&mut z.root_key).range(0..=127).prefix("root ")).changed();
                    changed |= ui.add(egui::DragValue::new(&mut z.key_range.0).range(0..=127).prefix("keys ")).changed();
                    changed |= ui.add(egui::DragValue::new(&mut z.key_range.1).range(0..=127).prefix("to ")).changed();
                    if ui.small_button("x").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                m2.zones.remove(i);
                changed = true;
            }
            egui::ComboBox::from_id_salt(("addzone", box_key(ch, dev, path))).selected_text("+ Zone from sample…").width(160.0).show_ui(ui, |ui| {
                for f in &samples {
                    if ui.selectable_label(false, f).clicked() {
                        // Spread new zones up the keyboard, one octave each.
                        let root = 48 + 12 * m2.zones.len() as u8 % 72;
                        m2.zones.push(SampleZone {
                            file: f.clone(),
                            root_key: root,
                            key_range: (root.saturating_sub(6), (root + 5).min(127)),
                            ..Default::default()
                        });
                        changed = true;
                    }
                }
            });
        });
    if changed {
        write_back(app, ch, dev, path, DeviceSpec::MultiSampler(m2));
    }
}
