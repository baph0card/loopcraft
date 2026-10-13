//! Device chains: add, remove, reorder, bypass, parameters, racks, macros
//! and presets.

use super::{App, DetailTab, MacroLearn};
use loopcraft_core::*;
use loopcraft_engine::{Command, DeviceRef, TrackRef};
use std::path::{Path, PathBuf};

/// Walk into nested racks: each step is (chain, device index in chain).
pub fn nested_mut<'a>(spec: &'a mut DeviceSpec, path: &[(usize, usize)]) -> Option<&'a mut DeviceSpec> {
    let Some(&(c, d)) = path.first() else { return Some(spec) };
    match spec {
        DeviceSpec::AudioRack(r) | DeviceSpec::InstrumentRack(r) => nested_mut(r.chains.get_mut(c)?.devices.get_mut(d)?, &path[1..]),
        _ => None,
    }
}

pub fn nested<'a>(spec: &'a DeviceSpec, path: &[(usize, usize)]) -> Option<&'a DeviceSpec> {
    let Some(&(c, d)) = path.first() else { return Some(spec) };
    match spec {
        DeviceSpec::AudioRack(r) | DeviceSpec::InstrumentRack(r) => nested(r.chains.get(c)?.devices.get(d)?, &path[1..]),
        _ => None,
    }
}

pub fn rack_mut(spec: &mut DeviceSpec) -> Option<&mut RackParams> {
    match spec {
        DeviceSpec::AudioRack(r) | DeviceSpec::InstrumentRack(r) => Some(r),
        _ => None,
    }
}

impl App {
    pub fn channel(&self, ch: TrackRef) -> Option<&Track> {
        match ch {
            TrackRef::Track(i) => self.project.tracks.get(i),
            TrackRef::Return(i) => self.project.returns.get(i),
            TrackRef::Master => Some(&self.project.master),
        }
    }

    pub fn channel_mut(&mut self, ch: TrackRef) -> Option<&mut Track> {
        match ch {
            TrackRef::Track(i) => self.project.tracks.get_mut(i),
            TrackRef::Return(i) => self.project.returns.get_mut(i),
            TrackRef::Master => Some(&mut self.project.master),
        }
    }

    pub fn spec(&self, ch: TrackRef, dev: DeviceRef) -> Option<&DeviceSpec> {
        let t = self.channel(ch)?;
        match dev {
            DeviceRef::Instrument => t.instrument.as_ref(),
            DeviceRef::Effect(i) => t.effects.get(i),
            DeviceRef::MidiEffect(i) => t.midi_effects.get(i),
        }
    }

    pub fn spec_mut(&mut self, ch: TrackRef, dev: DeviceRef) -> Option<&mut DeviceSpec> {
        let t = self.channel_mut(ch)?;
        match dev {
            DeviceRef::Instrument => t.instrument.as_mut(),
            DeviceRef::Effect(i) => t.effects.get_mut(i),
            DeviceRef::MidiEffect(i) => t.midi_effects.get_mut(i),
        }
    }

    /// Add a device where it belongs: instruments replace the track's
    /// instrument, MIDI effects go before it, audio effects at `at` (or the end).
    pub fn add_device_at(&mut self, ch: TrackRef, spec: DeviceSpec, at: Option<usize>) {
        let midi_track = matches!(ch, TrackRef::Track(t) if self.project.tracks.get(t).is_some_and(|t| t.kind == TrackKind::Midi));
        if (spec.is_instrument() || spec.is_midi_effect()) && !midi_track {
            self.status = format!("{} goes on a MIDI track", spec.name());
            return;
        }
        self.checkpoint();
        let name = spec.name().to_string();
        let t = self.channel_mut(ch).expect("checked above");
        if spec.is_instrument() {
            t.instrument = Some(spec);
            t.instrument_bypassed = false;
        } else if spec.is_midi_effect() {
            let at = at.unwrap_or(t.midi_effects.len()).min(t.midi_effects.len());
            t.midi_effects.insert(at, spec);
            shift_indices(&mut t.midi_bypassed, at, true);
        } else {
            let at = at.unwrap_or(t.effects.len()).min(t.effects.len());
            t.effects.insert(at, spec);
            shift_indices(&mut t.bypassed, at, true);
            shift_automation(&mut t.automation, at, true);
        }
        self.device_target = ch;
        self.detail_tab = DetailTab::Devices;
        self.rebuild();
        self.status = format!("Added {name}");
    }

    pub fn add_device(&mut self, ch: TrackRef, spec: DeviceSpec) {
        self.add_device_at(ch, spec, None);
    }

    pub fn remove_device(&mut self, ch: TrackRef, dev: DeviceRef) {
        if self.spec(ch, dev).is_none() {
            return;
        }
        self.checkpoint();
        let t = self.channel_mut(ch).expect("checked");
        match dev {
            DeviceRef::Instrument => t.instrument = None,
            DeviceRef::Effect(i) => {
                t.effects.remove(i);
                t.bypassed.retain(|&b| b != i);
                shift_indices(&mut t.bypassed, i, false);
                t.automation.retain(|l| !matches!(l.target, AutomationTarget::Effect { index, .. } if index == i));
                shift_automation(&mut t.automation, i, false);
            }
            DeviceRef::MidiEffect(i) => {
                t.midi_effects.remove(i);
                t.midi_bypassed.retain(|&b| b != i);
                shift_indices(&mut t.midi_bypassed, i, false);
            }
        }
        // Mappings pointing at effects on this track by index would now be off by one.
        if let (TrackRef::Track(tr), DeviceRef::Effect(i)) = (ch, dev) {
            self.project.mappings.retain_mut(|m| match &mut m.target {
                MappingTarget::DeviceParam { track, device, .. } if *track == tr => {
                    if *device == i {
                        return false;
                    }
                    if *device > i {
                        *device -= 1;
                    }
                    true
                }
                _ => true,
            });
        }
        self.collapsed_devices.retain(|d| d.0 != ch);
        self.rebuild();
    }

    pub fn move_device(&mut self, ch: TrackRef, dev: DeviceRef, left: bool) {
        let (len, i) = match (self.channel(ch), dev) {
            (Some(t), DeviceRef::Effect(i)) => (t.effects.len(), i),
            (Some(t), DeviceRef::MidiEffect(i)) => (t.midi_effects.len(), i),
            _ => return,
        };
        let j = if left { i.checked_sub(1) } else { Some(i + 1).filter(|&j| j < len) };
        let Some(j) = j else { return };
        self.checkpoint();
        let t = self.channel_mut(ch).expect("checked");
        let swap_in = |v: &mut Vec<usize>| {
            for b in v.iter_mut() {
                if *b == i {
                    *b = j
                } else if *b == j {
                    *b = i
                }
            }
        };
        match dev {
            DeviceRef::Effect(_) => {
                t.effects.swap(i, j);
                swap_in(&mut t.bypassed);
                for l in t.automation.iter_mut() {
                    if let AutomationTarget::Effect { index, .. } = &mut l.target {
                        if *index == i {
                            *index = j
                        } else if *index == j {
                            *index = i
                        }
                    }
                }
            }
            DeviceRef::MidiEffect(_) => {
                t.midi_effects.swap(i, j);
                swap_in(&mut t.midi_bypassed);
            }
            DeviceRef::Instrument => {}
        }
        if let TrackRef::Track(tr) = ch {
            for m in self.project.mappings.iter_mut() {
                if let MappingTarget::DeviceParam { track, device, .. } = &mut m.target
                    && *track == tr
                    && matches!(dev, DeviceRef::Effect(_))
                {
                    if *device == i {
                        *device = j
                    } else if *device == j {
                        *device = i
                    }
                }
            }
        }
        self.rebuild();
    }

    pub fn is_bypassed(&self, ch: TrackRef, dev: DeviceRef) -> bool {
        let Some(t) = self.channel(ch) else { return false };
        match dev {
            DeviceRef::Instrument => t.instrument_bypassed,
            DeviceRef::Effect(i) => t.bypassed.contains(&i),
            DeviceRef::MidiEffect(i) => t.midi_bypassed.contains(&i),
        }
    }

    pub fn set_bypass(&mut self, ch: TrackRef, dev: DeviceRef, bypass: bool) {
        let Some(t) = self.channel_mut(ch) else { return };
        let set = |v: &mut Vec<usize>, i: usize| {
            v.retain(|&b| b != i);
            if bypass {
                v.push(i);
            }
        };
        match dev {
            DeviceRef::Instrument => t.instrument_bypassed = bypass,
            DeviceRef::Effect(i) => set(&mut t.bypassed, i),
            DeviceRef::MidiEffect(i) => set(&mut t.midi_bypassed, i),
        }
        self.dirty = true;
        self.send(Command::SetDeviceBypass { channel: ch, device: dev, bypass });
    }

    /// Change a top-level device parameter: write it into the project and the engine.
    pub fn set_param(&mut self, channel: TrackRef, device: DeviceRef, param: usize, value: f32) {
        if let Some(spec) = self.spec_mut(channel, device) {
            loopcraft_dsp::specs::set_param(spec, param, value);
            self.dirty = true;
        }
        self.send(Command::SetDeviceParam { channel, device, param, value });
    }

    /// Change a parameter of a device nested inside a rack. The running
    /// rack only exposes its macros, so this edits the spec and schedules a
    /// rebuild for when the mouse is released.
    pub fn set_nested_param(&mut self, ch: TrackRef, dev: DeviceRef, path: &[(usize, usize)], param: usize, value: f32) {
        if path.is_empty() {
            return self.set_param(ch, dev, param, value);
        }
        if let Some(spec) = self.spec_mut(ch, dev).and_then(|s| nested_mut(s, path)) {
            loopcraft_dsp::specs::set_param(spec, param, value);
            self.dirty = true;
            self.rebuild_pending = true;
        }
    }

    /// Edit a rack (chains, mappings, names) in place, then rebuild later.
    pub fn edit_rack(&mut self, ch: TrackRef, dev: DeviceRef, path: &[(usize, usize)], f: impl FnOnce(&mut RackParams)) {
        if let Some(r) = self.spec_mut(ch, dev).and_then(|s| nested_mut(s, path)).and_then(rack_mut) {
            f(r);
            self.dirty = true;
            self.rebuild_pending = true;
        }
    }

    pub fn add_rack_chain(&mut self, ch: TrackRef, dev: DeviceRef, path: &[(usize, usize)]) {
        self.checkpoint();
        self.edit_rack(ch, dev, path, |r| {
            let n = r.chains.len() + 1;
            r.chains.push(RackChain { name: format!("Chain {n}"), ..Default::default() });
        });
    }

    pub fn remove_rack_chain(&mut self, ch: TrackRef, dev: DeviceRef, path: &[(usize, usize)], chain: usize) {
        self.checkpoint();
        self.edit_rack(ch, dev, path, |r| {
            if chain < r.chains.len() && r.chains.len() > 1 {
                r.chains.remove(chain);
                for m in r.macros.iter_mut() {
                    m.mappings.retain(|mm| mm.chain != chain);
                    for mm in m.mappings.iter_mut().filter(|mm| mm.chain > chain) {
                        mm.chain -= 1;
                    }
                }
            }
        });
    }

    pub fn add_to_rack_chain(&mut self, ch: TrackRef, dev: DeviceRef, path: &[(usize, usize)], chain: usize, spec: DeviceSpec) {
        let is_inst_rack = matches!(self.spec(ch, dev).and_then(|s| nested(s, path)), Some(DeviceSpec::InstrumentRack(_)));
        if spec.is_midi_effect() || (spec.is_instrument() && !is_inst_rack) {
            self.status = format!("{} can't go in this rack", spec.name());
            return;
        }
        self.checkpoint();
        self.edit_rack(ch, dev, path, |r| {
            if let Some(c) = r.chains.get_mut(chain) {
                if spec.is_instrument() {
                    // Instrument rack chains keep their instrument first.
                    if c.devices.first().is_some_and(|d| d.is_instrument()) {
                        c.devices[0] = spec;
                    } else {
                        c.devices.insert(0, spec);
                    }
                } else {
                    c.devices.push(spec);
                }
            }
        });
        self.rebuild_pending = false;
        self.rebuild();
    }

    pub fn remove_from_rack_chain(&mut self, ch: TrackRef, dev: DeviceRef, path: &[(usize, usize)], chain: usize, index: usize) {
        self.checkpoint();
        self.edit_rack(ch, dev, path, |r| {
            if let Some(c) = r.chains.get_mut(chain)
                && index < c.devices.len()
            {
                c.devices.remove(index);
                for m in r.macros.iter_mut() {
                    m.mappings.retain(|mm| !(mm.chain == chain && mm.device == index));
                    for mm in m.mappings.iter_mut().filter(|mm| mm.chain == chain && mm.device > index) {
                        mm.device -= 1;
                    }
                }
            }
        });
        self.rebuild_pending = false;
        self.rebuild();
    }

    /// Put a device into a new rack of the right kind ("Group" in Live).
    pub fn wrap_in_rack(&mut self, ch: TrackRef, dev: DeviceRef) {
        let Some(spec) = self.spec(ch, dev).cloned() else { return };
        if spec.is_midi_effect() || matches!(spec, DeviceSpec::AudioRack(_) | DeviceSpec::InstrumentRack(_)) {
            return;
        }
        self.checkpoint();
        let mut rack = RackParams { name: format!("{} Rack", spec.name()), ..Default::default() };
        rack.chains[0].name = spec.name().to_string();
        let instrument = spec.is_instrument();
        rack.chains[0].devices.push(spec);
        let new = if instrument { DeviceSpec::InstrumentRack(rack) } else { DeviceSpec::AudioRack(rack) };
        if let Some(s) = self.spec_mut(ch, dev) {
            *s = new;
        }
        // Automation and mappings of the wrapped device no longer apply.
        if let (TrackRef::Track(t), DeviceRef::Effect(i)) = (ch, dev) {
            self.project.tracks[t].automation.retain(|l| !matches!(l.target, AutomationTarget::Effect { index, .. } if index == i));
        }
        if let (TrackRef::Track(t), DeviceRef::Instrument) = (ch, dev) {
            self.project.tracks[t].automation.retain(|l| !matches!(l.target, AutomationTarget::Instrument { .. }));
        }
        self.rebuild();
    }

    /// Start mapping a macro: the next parameter clicked becomes its target.
    pub fn start_macro_learn(&mut self, ch: TrackRef, dev: DeviceRef, macro_index: usize) {
        self.macro_learn = if self.macro_learn.is_some_and(|m| m.channel == ch && m.device == dev && m.macro_index == macro_index) {
            None
        } else {
            Some(MacroLearn { channel: ch, device: dev, macro_index })
        };
    }

    /// Finish a macro mapping onto (chain, device, param) inside that rack.
    pub fn map_macro(&mut self, chain: usize, device: usize, param: usize) {
        let Some(learn) = self.macro_learn.take() else { return };
        let info = self
            .spec(learn.channel, learn.device)
            .and_then(|s| nested(s, &[(chain, device)]))
            .and_then(|s| loopcraft_dsp::specs::param_infos(s).get(param).copied());
        let Some(info) = info else { return };
        self.checkpoint();
        self.edit_rack(learn.channel, learn.device, &[], |r| {
            if let Some(m) = r.macros.get_mut(learn.macro_index) {
                m.mappings.retain(|mm| !(mm.chain == chain && mm.device == device && mm.param == param));
                m.mappings.push(MacroMapping { chain, device, param, min: info.min, max: info.max });
            }
        });
        self.rebuild_pending = false;
        self.rebuild();
        self.status = format!("Mapped macro {} to {}", learn.macro_index + 1, info.name);
    }

    // ------------------------------------------------------------------
    // Presets
    // ------------------------------------------------------------------

    pub fn preset_dir(&self) -> PathBuf {
        self.settings.library().join("Presets")
    }

    pub fn save_preset(&mut self, spec: &DeviceSpec, name: &str) {
        let dir = self.preset_dir().join(sanitize(spec.name()));
        let path = dir.join(format!("{}.json", sanitize(name)));
        let res = std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|_| serde_json::to_string_pretty(spec).map_err(|e| e.to_string())).and_then(|s| std::fs::write(&path, s).map_err(|e| e.to_string()));
        self.status = match res {
            Ok(()) => format!("Saved preset {}", path.display()),
            Err(e) => format!("Couldn't save preset: {e}"),
        };
    }

    /// (device folder, preset name, path), sorted.
    pub fn presets(&self) -> Vec<(String, String, PathBuf)> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.preset_dir()) else { return out };
        for d in rd.flatten().filter(|d| d.path().is_dir()) {
            let cat = d.file_name().to_string_lossy().into_owned();
            if let Ok(files) = std::fs::read_dir(d.path()) {
                for f in files.flatten() {
                    let p = f.path();
                    if p.extension().is_some_and(|e| e == "json") {
                        let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        out.push((cat.clone(), name, p));
                    }
                }
            }
        }
        out.sort();
        out
    }

    pub fn load_preset(&mut self, path: &Path) -> Option<DeviceSpec> {
        match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str::<DeviceSpec>(&s).map_err(|e| e.to_string())) {
            Ok(s) => Some(s),
            Err(e) => {
                self.status = format!("Couldn't load preset: {e}");
                None
            }
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect::<String>().trim().to_string()
}

/// Keep stored indices valid after inserting (or removing) at `at`.
fn shift_indices(v: &mut [usize], at: usize, insert: bool) {
    for b in v.iter_mut() {
        if insert && *b >= at {
            *b += 1;
        } else if !insert && *b > at {
            *b -= 1;
        }
    }
}

fn shift_automation(lanes: &mut [AutomationLane], at: usize, insert: bool) {
    for l in lanes.iter_mut() {
        if let AutomationTarget::Effect { index, .. } = &mut l.target {
            if insert && *index >= at {
                *index += 1;
            } else if !insert && *index > at {
                *index -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_paths_reach_rack_devices() {
        let mut rack = RackParams::default();
        rack.chains[0].devices.push(DeviceSpec::Delay(Default::default()));
        let mut spec = DeviceSpec::AudioRack(rack);
        assert!(matches!(nested(&spec, &[(0, 0)]), Some(DeviceSpec::Delay(_))));
        assert!(nested(&spec, &[(0, 1)]).is_none());
        let d = nested_mut(&mut spec, &[(0, 0)]).unwrap();
        assert!(loopcraft_dsp::specs::set_param(d, 0, 0.3));
    }

    #[test]
    fn index_shifting() {
        let mut v = vec![0, 2, 3];
        shift_indices(&mut v, 2, true);
        assert_eq!(v, vec![0, 3, 4]);
        shift_indices(&mut v, 1, false);
        assert_eq!(v, vec![0, 2, 3]);
    }
}
