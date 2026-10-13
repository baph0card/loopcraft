//! MIDI input (keyboards and controllers) and controller mappings:
//! MIDI learn, computer-key mappings, and applying mapped values.

use super::{App, MapMode};
use loopcraft_core::*;
use loopcraft_engine::{Command, DeviceRef, TrackRef};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};

pub struct MidiIn {
    _conns: Vec<midir::MidiInputConnection<()>>,
    rx: Receiver<(usize, [u8; 3])>,
    _tx: Sender<(usize, [u8; 3])>,
    /// Ports we're listening to.
    pub ports: Vec<String>,
    /// Every input port the system offers.
    pub available: Vec<String>,
    pub error: Option<String>,
    /// Last value per (channel, cc), for button-style CC mappings.
    cc_last: HashMap<(u8, u8), f32>,
    /// Shown briefly in the transport as activity.
    pub last_event: Option<(std::time::Instant, String)>,
}

impl MidiIn {
    pub fn none() -> Self {
        let (tx, rx) = channel();
        Self { _conns: Vec::new(), rx, _tx: tx, ports: Vec::new(), available: Vec::new(), error: None, cc_last: HashMap::new(), last_event: None }
    }

    /// Open the named ports (all ports when `wanted` is empty).
    pub fn open(wanted: &[String]) -> Self {
        let mut me = Self::none();
        let probe = match midir::MidiInput::new("LoopCraft") {
            Ok(m) => m,
            Err(e) => {
                me.error = Some(format!("MIDI unavailable: {e}"));
                return me;
            }
        };
        let ports = probe.ports();
        me.available = ports.iter().filter_map(|p| probe.port_name(p).ok()).collect();
        drop(probe);
        for (i, name) in me.available.clone().into_iter().enumerate() {
            if !wanted.is_empty() && !wanted.contains(&name) {
                continue;
            }
            // Skip our own / the system's pass-through ports.
            if name.contains("Midi Through") {
                continue;
            }
            let Ok(mut input) = midir::MidiInput::new("LoopCraft") else { continue };
            input.ignore(midir::Ignore::All);
            let Some(port) = input.ports().into_iter().nth(i) else { continue };
            let tx = me._tx.clone();
            let idx = me.ports.len();
            match input.connect(
                &port,
                "loopcraft-in",
                move |_, msg, _| {
                    let mut m = [0u8; 3];
                    for (d, s) in m.iter_mut().zip(msg.iter()) {
                        *d = *s;
                    }
                    let _ = tx.send((idx, m));
                },
                (),
            ) {
                Ok(c) => {
                    me._conns.push(c);
                    me.ports.push(name);
                }
                Err(e) => me.error = Some(format!("Couldn't open {name}: {e}")),
            }
        }
        me
    }
}

/// A raw MIDI message, decoded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MidiMsg {
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8 },
    Cc { channel: u8, cc: u8, value: u8 },
    PitchBend { channel: u8, value: i16 },
    Other,
}

pub fn decode(m: [u8; 3]) -> MidiMsg {
    let ch = m[0] & 0x0f;
    match m[0] & 0xf0 {
        0x90 if m[2] > 0 => MidiMsg::NoteOn { channel: ch, note: m[1] & 127, velocity: m[2] & 127 },
        0x90 | 0x80 => MidiMsg::NoteOff { channel: ch, note: m[1] & 127 },
        0xb0 => MidiMsg::Cc { channel: ch, cc: m[1] & 127, value: m[2] & 127 },
        0xe0 => MidiMsg::PitchBend { channel: ch, value: (((m[2] as i16) << 7) | m[1] as i16) - 8192 },
        _ => MidiMsg::Other,
    }
}

/// Is this target a button (trigger / toggle) rather than a continuous control?
pub fn is_button(t: &MappingTarget) -> bool {
    matches!(
        t,
        MappingTarget::LaunchClip { .. } | MappingTarget::LaunchScene(_) | MappingTarget::StopTrack(_) | MappingTarget::Play | MappingTarget::Stop | MappingTarget::Mute(_) | MappingTarget::Solo(_) | MappingTarget::Arm(_)
    )
}

impl App {
    /// Natural output range for a mapping target.
    pub fn mapping_range(&self, t: &MappingTarget) -> (f32, f32) {
        match *t {
            MappingTarget::Volume(_) => (-60.0, 6.0),
            MappingTarget::Pan(_) | MappingTarget::Crossfader => (-1.0, 1.0),
            MappingTarget::Send { .. } => (-60.0, 0.0),
            MappingTarget::Tempo => (60.0, 200.0),
            MappingTarget::DeviceParam { track, device, param } => self
                .project
                .tracks
                .get(track)
                .and_then(|t| t.effects.get(device))
                .and_then(|s| loopcraft_dsp::specs::param_infos(s).get(param))
                .map_or((0.0, 1.0), |p| (p.min, p.max)),
            MappingTarget::InstrumentParam { track, param } => self
                .project
                .tracks
                .get(track)
                .and_then(|t| t.instrument.as_ref())
                .and_then(|s| loopcraft_dsp::specs::param_infos(s).get(param))
                .map_or((0.0, 1.0), |p| (p.min, p.max)),
            _ => (0.0, 1.0),
        }
    }

    pub fn mapping_label(&self, t: &MappingTarget) -> String {
        let tn = |i: usize| self.project.tracks.get(i).map(|t| t.name.clone()).unwrap_or_else(|| "?".into());
        match *t {
            MappingTarget::Volume(i) => format!("{} Volume", tn(i)),
            MappingTarget::Pan(i) => format!("{} Pan", tn(i)),
            MappingTarget::Send { track, ret } => format!("{} Send {}", tn(track), self.project.returns.get(ret).map(|r| r.name.as_str()).unwrap_or("?")),
            MappingTarget::DeviceParam { track, device, param } => {
                let s = self.project.tracks.get(track).and_then(|t| t.effects.get(device));
                let p = s.and_then(|s| loopcraft_dsp::specs::param_infos(s).get(param)).map(|p| p.name).unwrap_or("?");
                format!("{} {} {}", tn(track), s.map(|s| s.name()).unwrap_or("?"), p)
            }
            MappingTarget::InstrumentParam { track, param } => {
                let s = self.project.tracks.get(track).and_then(|t| t.instrument.as_ref());
                let p = s.and_then(|s| loopcraft_dsp::specs::param_infos(s).get(param)).map(|p| p.name).unwrap_or("?");
                format!("{} {} {}", tn(track), s.map(|s| s.name()).unwrap_or("?"), p)
            }
            MappingTarget::Tempo => "Tempo".into(),
            MappingTarget::Crossfader => "Crossfader".into(),
            MappingTarget::LaunchClip { track, slot } => format!("Launch {} slot {}", tn(track), slot + 1),
            MappingTarget::LaunchScene(s) => format!("Launch scene {}", s + 1),
            MappingTarget::StopTrack(i) => format!("Stop {}", tn(i)),
            MappingTarget::Play => "Play".into(),
            MappingTarget::Stop => "Stop".into(),
            MappingTarget::Mute(i) => format!("{} Mute", tn(i)),
            MappingTarget::Solo(i) => format!("{} Solo", tn(i)),
            MappingTarget::Arm(i) => format!("{} Arm", tn(i)),
        }
    }

    /// In map mode, a click on a control picks it as the mapping target.
    pub fn pick_map_target(&mut self, t: MappingTarget) {
        self.map_target = Some(t);
        self.status = format!("{}: now move a MIDI control or press a key", self.mapping_label(&t));
    }

    fn add_mapping(&mut self, source: MappingSource) {
        let Some(target) = self.map_target.take() else { return };
        self.checkpoint();
        // One source drives one target; re-learning replaces.
        self.project.mappings.retain(|m| m.source != source && m.target != target);
        let (min, max) = self.mapping_range(&target);
        self.project.mappings.push(Mapping { source, target, min, max });
        self.status = format!("Mapped {:?} to {}", source, self.mapping_label(&target));
    }

    pub fn remove_mapping(&mut self, i: usize) {
        if i < self.project.mappings.len() {
            self.checkpoint();
            self.project.mappings.remove(i);
        }
    }

    /// Drive a mapped target with `v` in 0..1.
    pub fn apply_mapping(&mut self, m: Mapping, v: f32) {
        let x = m.min + (m.max - m.min) * v.clamp(0.0, 1.0);
        match m.target {
            MappingTarget::Volume(t) => {
                if let Some(mx) = self.mixer_mut(TrackRef::Track(t)) {
                    mx.volume_db = x;
                    self.send(Command::SetVolume { channel: TrackRef::Track(t), db: x });
                }
            }
            MappingTarget::Pan(t) => {
                if let Some(mx) = self.mixer_mut(TrackRef::Track(t)) {
                    mx.pan = x;
                    self.send(Command::SetPan { channel: TrackRef::Track(t), pan: x });
                }
            }
            MappingTarget::Send { track, ret } => {
                if let Some(s) = self.project.tracks.get_mut(track).and_then(|t| t.sends.get_mut(ret)) {
                    *s = x;
                    self.send(Command::SetSend { track, ret, db: x });
                }
            }
            MappingTarget::DeviceParam { track, device, param } => self.set_param(TrackRef::Track(track), DeviceRef::Effect(device), param, x),
            MappingTarget::InstrumentParam { track, param } => self.set_param(TrackRef::Track(track), DeviceRef::Instrument, param, x),
            MappingTarget::Tempo => {
                self.project.tempo = x as f64;
                self.send(Command::SetTempo(x as f64));
            }
            MappingTarget::Crossfader => {
                self.project.crossfader = x;
                self.send(Command::SetCrossfader(x));
            }
            _ => {
                if v >= 0.5 {
                    self.trigger(m.target);
                }
            }
        }
        self.dirty = true;
    }

    /// Fire a button-style target.
    pub fn trigger(&mut self, t: MappingTarget) {
        match t {
            MappingTarget::LaunchClip { track, slot } => self.send(Command::LaunchClip { track, slot }),
            MappingTarget::LaunchScene(scene) => self.send(Command::LaunchScene { scene }),
            MappingTarget::StopTrack(track) => self.send(Command::StopTrack { track }),
            MappingTarget::Play => self.send(Command::Play),
            MappingTarget::Stop => self.send(Command::Stop),
            MappingTarget::Mute(t) => {
                if let Some(mx) = self.mixer_mut(TrackRef::Track(t)) {
                    mx.mute = !mx.mute;
                    let mute = mx.mute;
                    self.send(Command::SetMute { channel: TrackRef::Track(t), mute });
                }
            }
            MappingTarget::Solo(t) => {
                if let Some(mx) = self.mixer_mut(TrackRef::Track(t)) {
                    mx.solo = !mx.solo;
                    let solo = mx.solo;
                    self.send(Command::SetSolo { track: t, solo });
                }
            }
            MappingTarget::Arm(t) => {
                let armed = !self.project.tracks.get(t).is_some_and(|x| x.armed);
                self.set_arm(t, armed, false);
            }
            _ => {}
        }
    }

    /// A computer key was pressed outside text fields. Returns true if a
    /// mapping (or key-map learn) consumed it.
    pub fn key_mapping(&mut self, c: char) -> bool {
        if self.map_mode == Some(MapMode::Key) {
            if self.map_target.is_some() {
                self.add_mapping(MappingSource::Key(c));
                return true;
            }
            return false;
        }
        let maps: Vec<Mapping> = self.project.mappings.iter().filter(|m| m.source == MappingSource::Key(c)).copied().collect();
        for m in &maps {
            if is_button(&m.target) {
                self.trigger(m.target);
            } else {
                // Keys toggle continuous targets between min and max.
                let cur = self.mapped_value(m);
                self.apply_mapping(*m, if cur > 0.5 { 0.0 } else { 1.0 });
            }
        }
        !maps.is_empty()
    }

    /// Where a continuous target currently sits, 0..1 within its mapping range.
    fn mapped_value(&self, m: &Mapping) -> f32 {
        let x = match m.target {
            MappingTarget::Volume(t) => self.project.tracks.get(t).map_or(0.0, |t| t.mixer.volume_db),
            MappingTarget::Pan(t) => self.project.tracks.get(t).map_or(0.0, |t| t.mixer.pan),
            MappingTarget::Send { track, ret } => self.project.tracks.get(track).and_then(|t| t.sends.get(ret)).copied().unwrap_or(-60.0),
            MappingTarget::Tempo => self.project.tempo as f32,
            MappingTarget::Crossfader => self.project.crossfader,
            MappingTarget::DeviceParam { track, device, param } => {
                self.project.tracks.get(track).and_then(|t| t.effects.get(device)).and_then(|s| loopcraft_dsp::specs::get_param(s, param)).unwrap_or(0.0)
            }
            MappingTarget::InstrumentParam { track, param } => {
                self.project.tracks.get(track).and_then(|t| t.instrument.as_ref()).and_then(|s| loopcraft_dsp::specs::get_param(s, param)).unwrap_or(0.0)
            }
            _ => 0.0,
        };
        if (m.max - m.min).abs() < 1e-9 { 0.0 } else { (x - m.min) / (m.max - m.min) }
    }

    /// Route incoming MIDI: learn, mappings, then notes to the live tracks.
    pub(crate) fn poll_midi(&mut self) {
        let mut msgs = Vec::new();
        while let Ok((_, m)) = self.midi.rx.try_recv() {
            msgs.push(decode(m));
        }
        for msg in msgs {
            self.midi.last_event = Some((std::time::Instant::now(), format!("{msg:?}")));
            match msg {
                MidiMsg::NoteOn { channel, note, velocity } => {
                    let src = MappingSource::Note { channel, note };
                    if self.map_mode == Some(MapMode::Midi) && self.map_target.is_some() {
                        self.add_mapping(src);
                        continue;
                    }
                    let maps: Vec<Mapping> = self.project.mappings.iter().filter(|m| m.source == src).copied().collect();
                    if !maps.is_empty() {
                        for m in maps {
                            self.apply_mapping(m, velocity as f32 / 127.0);
                        }
                        continue;
                    }
                    for t in self.live_targets() {
                        self.send(Command::NoteOn { track: t, note, velocity });
                    }
                }
                MidiMsg::NoteOff { channel, note } => {
                    let src = MappingSource::Note { channel, note };
                    if self.project.mappings.iter().any(|m| m.source == src) {
                        continue;
                    }
                    for t in self.live_targets() {
                        self.send(Command::NoteOff { track: t, note });
                    }
                }
                MidiMsg::Cc { channel, cc, value } => {
                    let src = MappingSource::Cc { channel, cc };
                    if self.map_mode == Some(MapMode::Midi) && self.map_target.is_some() {
                        self.add_mapping(src);
                        continue;
                    }
                    let v = value as f32 / 127.0;
                    let prev = self.midi.cc_last.insert((channel, cc), v).unwrap_or(0.0);
                    let maps: Vec<Mapping> = self.project.mappings.iter().filter(|m| m.source == src).copied().collect();
                    for m in maps {
                        if is_button(&m.target) {
                            // Fire on the rising edge only.
                            if v >= 0.5 && prev < 0.5 {
                                self.trigger(m.target);
                            }
                        } else {
                            self.apply_mapping(m, v);
                        }
                    }
                    // Sustain pedal and other CCs aren't forwarded to instruments yet.
                }
                MidiMsg::PitchBend { .. } | MidiMsg::Other => {}
            }
        }
    }

    /// Re-open MIDI ports after the selection changed in Settings.
    pub fn reopen_midi(&mut self) {
        self.midi = MidiIn::open(&self.settings.midi_inputs);
        self.settings.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_messages() {
        assert_eq!(decode([0x91, 60, 100]), MidiMsg::NoteOn { channel: 1, note: 60, velocity: 100 });
        assert_eq!(decode([0x90, 60, 0]), MidiMsg::NoteOff { channel: 0, note: 60 });
        assert_eq!(decode([0x80, 61, 30]), MidiMsg::NoteOff { channel: 0, note: 61 });
        assert_eq!(decode([0xb2, 7, 127]), MidiMsg::Cc { channel: 2, cc: 7, value: 127 });
        assert_eq!(decode([0xe0, 0, 0x40]), MidiMsg::PitchBend { channel: 0, value: 0 });
        assert_eq!(decode([0xf8, 0, 0]), MidiMsg::Other);
    }
}
