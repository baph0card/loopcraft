//! Instrument and Audio Effect Racks: parallel device chains with eight
//! macro knobs that each drive any number of child parameters.
//!
//! Chains with different latencies are delay-compensated inside the rack,
//! and the rack reports the largest chain latency to the engine.

use crate::effects::MAX_BLOCK;
use crate::util::{DelayLine, pan_gains};
use crate::{Effect, Instrument, ParamInfo, ProcessContext};
use loopcraft_core::*;

pub(crate) const MACRO_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Macro 1", 0.0, 1.0, "").at("macros/0/value"),
    ParamInfo::new("Macro 2", 0.0, 1.0, "").at("macros/1/value"),
    ParamInfo::new("Macro 3", 0.0, 1.0, "").at("macros/2/value"),
    ParamInfo::new("Macro 4", 0.0, 1.0, "").at("macros/3/value"),
    ParamInfo::new("Macro 5", 0.0, 1.0, "").at("macros/4/value"),
    ParamInfo::new("Macro 6", 0.0, 1.0, "").at("macros/5/value"),
    ParamInfo::new("Macro 7", 0.0, 1.0, "").at("macros/6/value"),
    ParamInfo::new("Macro 8", 0.0, 1.0, "").at("macros/7/value"),
];

struct Chain {
    instrument: Option<Box<dyn Instrument>>,
    fx: Vec<Box<dyn Effect>>,
    gain_l: f32,
    gain_r: f32,
    mute: bool,
    keys: (u8, u8),
    vels: (u8, u8),
    l: Vec<f32>,
    r: Vec<f32>,
    latency: usize,
    comp: Option<(DelayLine, DelayLine, f32)>,
    /// Notes this chain is currently holding (so note-offs reach the right chains).
    held: [bool; 128],
}

impl Chain {
    fn new(spec: &RackChain, instrument: Option<Box<dyn Instrument>>, fx: Vec<Box<dyn Effect>>) -> Self {
        let (pl, pr) = pan_gains(spec.pan);
        let g = db_to_gain(spec.volume_db) * std::f32::consts::SQRT_2;
        let latency = fx.iter().map(|f| f.latency()).sum();
        Self {
            instrument,
            fx,
            gain_l: pl * g,
            gain_r: pr * g,
            mute: spec.mute,
            keys: spec.key_range,
            vels: spec.velocity_range,
            l: vec![0.0; MAX_BLOCK],
            r: vec![0.0; MAX_BLOCK],
            latency,
            comp: None,
            held: [false; 128],
        }
    }

    fn set_device_param(&mut self, device: usize, has_instrument: bool, param: usize, value: f32) {
        if has_instrument {
            if device == 0 {
                if let Some(i) = self.instrument.as_mut() {
                    i.set_param(param, value);
                }
            } else if let Some(f) = self.fx.get_mut(device - 1) {
                f.set_param(param, value);
            }
        } else if let Some(f) = self.fx.get_mut(device) {
            f.set_param(param, value);
        }
    }

    fn run_fx(&mut self, ctx: &ProcessContext, n: usize) {
        for f in self.fx.iter_mut() {
            f.process(ctx, &mut self.l[..n], &mut self.r[..n]);
        }
        if let Some((dl, dr, d)) = self.comp.as_mut() {
            for i in 0..n {
                dl.write(self.l[i]);
                dr.write(self.r[i]);
                self.l[i] = dl.read(*d);
                self.r[i] = dr.read(*d);
            }
        }
    }
}

fn compensate(chains: &mut [Chain]) -> usize {
    let max = chains.iter().map(|c| c.latency).max().unwrap_or(0);
    for c in chains.iter_mut() {
        let d = max - c.latency;
        if d > 0 {
            // DelayLine::read(1) is the most recent sample, so add one.
            c.comp = Some((DelayLine::new(d + 2), DelayLine::new(d + 2), d as f32));
        }
    }
    max
}

fn apply_macro(chains: &mut [Chain], macros: &mut [Macro], has_instrument: bool, i: usize, v: f32) {
    let Some(m) = macros.get_mut(i) else { return };
    m.value = v.clamp(0.0, 1.0);
    for map in m.mappings.iter() {
        let val = map.min + (map.max - map.min) * m.value;
        if let Some(c) = chains.get_mut(map.chain) {
            c.set_device_param(map.device, has_instrument, map.param, val);
        }
    }
}

pub struct AudioRack {
    name: String,
    chains: Vec<Chain>,
    macros: Vec<Macro>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    latency: usize,
}

impl AudioRack {
    /// `build` turns each saved chain into its running effects.
    pub fn new(p: &RackParams, mut build: impl FnMut(&DeviceSpec) -> Option<Box<dyn Effect>>) -> Self {
        let mut chains: Vec<Chain> = p.chains.iter().map(|c| Chain::new(c, None, c.devices.iter().filter_map(&mut build).collect())).collect();
        let latency = compensate(&mut chains);
        let mut rack = Self { name: if p.name.is_empty() { "Audio Effect Rack".into() } else { p.name.clone() }, chains, macros: p.macros.clone(), in_l: vec![0.0; MAX_BLOCK], in_r: vec![0.0; MAX_BLOCK], latency };
        for i in 0..rack.macros.len() {
            let v = rack.macros[i].value;
            apply_macro(&mut rack.chains, &mut rack.macros, false, i, v);
        }
        rack
    }
}

impl Effect for AudioRack {
    fn process(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(MAX_BLOCK);
        if self.chains.is_empty() {
            return;
        }
        self.in_l[..n].copy_from_slice(&left[..n]);
        self.in_r[..n].copy_from_slice(&right[..n]);
        left[..n].fill(0.0);
        right[..n].fill(0.0);
        for c in self.chains.iter_mut() {
            c.l[..n].copy_from_slice(&self.in_l[..n]);
            c.r[..n].copy_from_slice(&self.in_r[..n]);
            c.run_fx(ctx, n);
            if c.mute {
                continue;
            }
            for i in 0..n {
                left[i] += c.l[i] * c.gain_l;
                right[i] += c.r[i] * c.gain_r;
            }
        }
    }
    fn reset(&mut self) {
        for c in self.chains.iter_mut() {
            c.fx.iter_mut().for_each(|f| f.reset());
        }
    }
    fn latency(&self) -> usize {
        self.latency
    }
    fn params(&self) -> &'static [ParamInfo] {
        &MACRO_PARAMS[..self.macros.len().min(8)]
    }
    fn set_param(&mut self, i: usize, v: f32) {
        apply_macro(&mut self.chains, &mut self.macros, false, i, v);
    }
    fn sidechain(&mut self, l: &[f32], r: &[f32]) {
        for c in self.chains.iter_mut() {
            for f in c.fx.iter_mut().filter(|f| f.wants_sidechain().is_some()) {
                f.sidechain(l, r);
            }
        }
    }
    fn wants_sidechain(&self) -> Option<usize> {
        self.chains.iter().flat_map(|c| c.fx.iter()).find_map(|f| f.wants_sidechain())
    }
    fn name(&self) -> &str {
        &self.name
    }
}

pub struct InstrumentRack {
    name: String,
    chains: Vec<Chain>,
    macros: Vec<Macro>,
}

impl InstrumentRack {
    /// `build_inst` builds each chain's first device (the instrument);
    /// `build_fx` builds the rest.
    pub fn new(
        p: &RackParams,
        mut build_inst: impl FnMut(&DeviceSpec) -> Option<Box<dyn Instrument>>,
        mut build_fx: impl FnMut(&DeviceSpec) -> Option<Box<dyn Effect>>,
    ) -> Self {
        let mut chains: Vec<Chain> = p
            .chains
            .iter()
            .map(|c| {
                let inst = c.devices.first().and_then(&mut build_inst);
                let fx = c.devices.iter().skip(1).filter_map(&mut build_fx).collect();
                Chain::new(c, inst, fx)
            })
            .collect();
        compensate(&mut chains);
        let mut rack = Self { name: if p.name.is_empty() { "Instrument Rack".into() } else { p.name.clone() }, chains, macros: p.macros.clone() };
        for i in 0..rack.macros.len() {
            let v = rack.macros[i].value;
            apply_macro(&mut rack.chains, &mut rack.macros, true, i, v);
        }
        rack
    }
}

impl Instrument for InstrumentRack {
    fn note_on(&mut self, note: u8, velocity: u8) {
        for c in self.chains.iter_mut() {
            let ok = (c.keys.0..=c.keys.1).contains(&note) && (c.vels.0..=c.vels.1).contains(&velocity);
            if let (true, Some(i)) = (ok, c.instrument.as_mut()) {
                i.note_on(note, velocity);
                c.held[note as usize & 127] = true;
            }
        }
    }
    fn note_off(&mut self, note: u8) {
        for c in self.chains.iter_mut() {
            if c.held[note as usize & 127] {
                c.held[note as usize & 127] = false;
                if let Some(i) = c.instrument.as_mut() {
                    i.note_off(note);
                }
            }
        }
    }
    fn all_notes_off(&mut self) {
        for c in self.chains.iter_mut() {
            c.held = [false; 128];
            if let Some(i) = c.instrument.as_mut() {
                i.all_notes_off();
            }
        }
    }
    fn render(&mut self, ctx: &ProcessContext, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(MAX_BLOCK);
        for c in self.chains.iter_mut() {
            let Some(inst) = c.instrument.as_mut() else { continue };
            c.l[..n].fill(0.0);
            c.r[..n].fill(0.0);
            inst.render(ctx, &mut c.l[..n], &mut c.r[..n]);
            c.run_fx(ctx, n);
            if c.mute {
                continue;
            }
            for i in 0..n {
                left[i] += c.l[i] * c.gain_l;
                right[i] += c.r[i] * c.gain_r;
            }
        }
    }
    fn params(&self) -> &'static [ParamInfo] {
        &MACRO_PARAMS[..self.macros.len().min(8)]
    }
    fn set_param(&mut self, i: usize, v: f32) {
        apply_macro(&mut self.chains, &mut self.macros, true, i, v);
    }
    fn name(&self) -> &str {
        &self.name
    }
}
