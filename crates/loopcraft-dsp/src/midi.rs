//! MIDI events and MIDI effects (Arpeggiator, Chord, Scale, Random, Note
//! Length, Velocity, Pitch).
//!
//! MIDI effects run on the audio thread between the clip and the
//! instrument. Like everything else in this crate they never allocate
//! after construction.

use crate::util::Noise;
use crate::{ParamInfo, ProcessContext};
use loopcraft_core::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MidiEvent {
    pub frame: u32,
    pub on: bool,
    pub note: u8,
    pub velocity: u8,
    /// Chance the note-on survives `EventList::resolve` (1.0 = always).
    pub probability: f32,
}

impl MidiEvent {
    pub fn on(frame: u32, note: u8, velocity: u8) -> Self {
        Self { frame, on: true, note, velocity, probability: 1.0 }
    }
    pub fn off(frame: u32, note: u8) -> Self {
        Self { frame, on: false, note, velocity: 0, probability: 1.0 }
    }

    /// Sort key: by frame, with note-offs before note-ons at the same frame
    /// so a note that ends where it restarts is retriggered cleanly.
    pub fn key(&self) -> (u32, bool) {
        (self.frame, self.on)
    }
}

/// Fixed-capacity event list. Pushing past capacity drops events rather
/// than allocating on the audio thread.
pub struct EventList {
    events: Vec<MidiEvent>,
}

impl EventList {
    pub const CAPACITY: usize = 1024;

    pub fn new() -> Self {
        Self { events: Vec::with_capacity(Self::CAPACITY) }
    }

    #[inline]
    pub fn push(&mut self, e: MidiEvent) {
        if self.events.len() < Self::CAPACITY {
            self.events.push(e);
        }
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    pub fn sort(&mut self) {
        self.events.sort_by_key(MidiEvent::key);
    }

    pub fn as_slice(&self) -> &[MidiEvent] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn extend_from(&mut self, other: &EventList) {
        for e in other.as_slice() {
            self.push(*e);
        }
    }

    /// Drop note-offs for notes that aren't sounding and note-ons that lose
    /// their probability roll, updating `active` as we go.
    pub fn resolve(&mut self, active: &mut [bool; 128], rng: &mut Noise) {
        self.sort();
        self.events.retain(|e| {
            let n = e.note as usize & 127;
            if e.on {
                let p = e.probability;
                if p < 1.0 && rng.unit() >= p {
                    return false;
                }
                active[n] = true;
                true
            } else if active[n] {
                active[n] = false;
                true
            } else {
                false
            }
        });
    }
}

impl Default for EventList {
    fn default() -> Self {
        Self::new()
    }
}

/// A note processor between clip and instrument.
pub trait MidiEffect: Send {
    /// Read `input` (sorted) for a block of `frames` starting at song beat
    /// `ctx.beat`, and write the result into `out` (already cleared).
    fn process(&mut self, ctx: &ProcessContext, frames: u32, input: &EventList, out: &mut EventList);
    /// Forget all state (sounding notes, held keys). The engine follows up
    /// with note-offs for every pitch, so nothing keeps ringing.
    fn reset(&mut self);
    fn params(&self) -> &'static [ParamInfo] {
        &[]
    }
    fn set_param(&mut self, _index: usize, _value: f32) {}
    fn name(&self) -> &str;
}

fn beats_per_frame(ctx: &ProcessContext) -> f64 {
    ctx.tempo / 60.0 / ctx.sample_rate as f64
}

/// Map input notes to sets of output notes, keeping note-offs consistent
/// even if parameters change while notes are held, and reference-counting
/// output pitches shared by several inputs.
struct NoteMapper {
    outs: [[u8; 12]; 128],
    counts: [u8; 128],
    refc: [u8; 128],
}

impl NoteMapper {
    fn new() -> Self {
        Self { outs: [[0; 12]; 128], counts: [0; 128], refc: [0; 128] }
    }

    fn on(&mut self, frame: u32, input: u8, vel: &[u8], targets: &[u8], out: &mut EventList) {
        // Re-pressing a held input first releases what it mapped to.
        self.off(frame, input, out);
        let i = input as usize & 127;
        let n = targets.len().min(12);
        for (k, &t) in targets.iter().take(n).enumerate() {
            self.outs[i][k] = t;
            self.refc[t as usize & 127] = self.refc[t as usize & 127].saturating_add(1);
            out.push(MidiEvent::on(frame, t, vel[k.min(vel.len() - 1)]));
        }
        self.counts[i] = n as u8;
    }

    fn off(&mut self, frame: u32, input: u8, out: &mut EventList) {
        let i = input as usize & 127;
        for k in 0..self.counts[i] as usize {
            let t = self.outs[i][k] as usize & 127;
            self.refc[t] = self.refc[t].saturating_sub(1);
            if self.refc[t] == 0 {
                out.push(MidiEvent::off(frame, t as u8));
            }
        }
        self.counts[i] = 0;
    }

    fn reset(&mut self) {
        self.counts = [0; 128];
        self.refc = [0; 128];
    }
}

// ---------------------------------------------------------------------------
// Chord / Scale / Pitch / Random / Velocity: per-note mappings
// ---------------------------------------------------------------------------

pub struct Chord {
    p: ChordParams,
    map: NoteMapper,
}

impl Chord {
    pub fn new(p: ChordParams) -> Self {
        Self { p, map: NoteMapper::new() }
    }
}

const CHORD_PARAMS: &[ParamInfo] = &[ParamInfo::new("Velocity", 0.0, 2.0, "").at("velocity")];

impl MidiEffect for Chord {
    fn process(&mut self, _ctx: &ProcessContext, _frames: u32, input: &EventList, out: &mut EventList) {
        for e in input.as_slice() {
            if e.on {
                let mut t = [0u8; 12];
                let mut v = [0u8; 12];
                t[0] = e.note;
                v[0] = e.velocity;
                let mut n = 1;
                for &iv in self.p.intervals.iter().take(11) {
                    let p = e.note as i32 + iv as i32;
                    if (0..=127).contains(&p) && iv != 0 {
                        t[n] = p as u8;
                        v[n] = ((e.velocity as f32 * self.p.velocity).round() as i32).clamp(1, 127) as u8;
                        n += 1;
                    }
                }
                self.map.on(e.frame, e.note, &v[..n], &t[..n], out);
            } else {
                self.map.off(e.frame, e.note, out);
            }
        }
    }
    fn reset(&mut self) {
        self.map.reset();
    }
    fn params(&self) -> &'static [ParamInfo] {
        CHORD_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        if i == 0 {
            self.p.velocity = v;
        }
    }
    fn name(&self) -> &str {
        "Chord"
    }
}

pub struct Scale {
    p: ScaleParams,
    map: NoteMapper,
}

impl Scale {
    pub fn new(p: ScaleParams) -> Self {
        Self { p, map: NoteMapper::new() }
    }
}

const SCALE_PARAMS: &[ParamInfo] = &[ParamInfo::new("Root", 0.0, 11.0, "").at("root"), ParamInfo::new("Transpose", -24.0, 24.0, "st").at("transpose")];

impl MidiEffect for Scale {
    fn process(&mut self, _ctx: &ProcessContext, _frames: u32, input: &EventList, out: &mut EventList) {
        for e in input.as_slice() {
            if e.on {
                let shifted = (e.note as i32 + self.p.transpose as i32).clamp(0, 127) as u8;
                let t = self.p.scale.snap(self.p.root % 12, shifted);
                self.map.on(e.frame, e.note, &[e.velocity], &[t], out);
            } else {
                self.map.off(e.frame, e.note, out);
            }
        }
    }
    fn reset(&mut self) {
        self.map.reset();
    }
    fn params(&self) -> &'static [ParamInfo] {
        SCALE_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.root = v.round().clamp(0.0, 11.0) as u8,
            1 => self.p.transpose = v.round() as i8,
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Scale"
    }
}

pub struct Pitch {
    p: PitchParams,
    map: NoteMapper,
}

impl Pitch {
    pub fn new(p: PitchParams) -> Self {
        Self { p, map: NoteMapper::new() }
    }
}

const PITCH_PARAMS: &[ParamInfo] = &[ParamInfo::new("Pitch", -48.0, 48.0, "st").at("semitones")];

impl MidiEffect for Pitch {
    fn process(&mut self, _ctx: &ProcessContext, _frames: u32, input: &EventList, out: &mut EventList) {
        for e in input.as_slice() {
            if e.on {
                let t = e.note as i32 + self.p.semitones as i32;
                let in_range = self.p.range.is_none_or(|(lo, hi)| (lo as i32..=hi as i32).contains(&t));
                if (0..=127).contains(&t) && in_range {
                    self.map.on(e.frame, e.note, &[e.velocity], &[t as u8], out);
                }
            } else {
                self.map.off(e.frame, e.note, out);
            }
        }
    }
    fn reset(&mut self) {
        self.map.reset();
    }
    fn params(&self) -> &'static [ParamInfo] {
        PITCH_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        if i == 0 {
            self.p.semitones = v.round().clamp(-48.0, 48.0) as i8;
        }
    }
    fn name(&self) -> &str {
        "Pitch"
    }
}

pub struct RandomNote {
    p: RandomNoteParams,
    map: NoteMapper,
    rng: Noise,
}

impl RandomNote {
    pub fn new(p: RandomNoteParams) -> Self {
        Self { p, map: NoteMapper::new(), rng: Noise::with_seed(0x51ed_270b) }
    }
}

const RANDOM_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Chance", 0.0, 1.0, "").at("chance"),
    ParamInfo::new("Choices", 1.0, 24.0, "").at("choices"),
    ParamInfo::new("Interval", 1.0, 24.0, "st").at("interval"),
];

impl MidiEffect for RandomNote {
    fn process(&mut self, _ctx: &ProcessContext, _frames: u32, input: &EventList, out: &mut EventList) {
        for e in input.as_slice() {
            if e.on {
                let mut t = e.note as i32;
                if self.rng.unit() < self.p.chance {
                    let k = 1 + (self.rng.next_u32() % self.p.choices.max(1) as u32) as i32;
                    t += k * self.p.interval as i32;
                }
                let mut t = t.clamp(0, 127) as u8;
                if self.p.scale != ScaleKind::Chromatic {
                    t = self.p.scale.snap(self.p.root % 12, t);
                }
                self.map.on(e.frame, e.note, &[e.velocity], &[t], out);
            } else {
                self.map.off(e.frame, e.note, out);
            }
        }
    }
    fn reset(&mut self) {
        self.map.reset();
    }
    fn params(&self) -> &'static [ParamInfo] {
        RANDOM_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.chance = v.clamp(0.0, 1.0),
            1 => self.p.choices = v.round().clamp(1.0, 24.0) as u8,
            2 => self.p.interval = v.round().clamp(1.0, 24.0) as u8,
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Random"
    }
}

pub struct Velocity {
    p: VelocityParams,
    rng: Noise,
}

impl Velocity {
    pub fn new(p: VelocityParams) -> Self {
        Self { p, rng: Noise::with_seed(0x0bad_cafe) }
    }

    pub fn map(&mut self, v: u8) -> u8 {
        let x = ((v.max(1) - 1) as f32 / 126.0).powf(self.p.curve.max(0.05));
        let lo = self.p.out_low.min(self.p.out_high) as f32;
        let hi = self.p.out_high.max(self.p.out_low) as f32;
        let mut y = lo + (hi - lo) * x;
        if self.p.random > 0 {
            y += self.rng.next() * self.p.random as f32;
        }
        y.round().clamp(1.0, 127.0) as u8
    }
}

const VELOCITY_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Out Low", 1.0, 127.0, "").at("out_low"),
    ParamInfo::new("Out High", 1.0, 127.0, "").at("out_high"),
    ParamInfo::new("Random", 0.0, 64.0, "").at("random"),
    ParamInfo::new("Curve", 0.25, 4.0, "").at("curve"),
];

impl MidiEffect for Velocity {
    fn process(&mut self, _ctx: &ProcessContext, _frames: u32, input: &EventList, out: &mut EventList) {
        for e in input.as_slice() {
            let mut e = *e;
            if e.on {
                e.velocity = self.map(e.velocity);
            }
            out.push(e);
        }
    }
    fn reset(&mut self) {}
    fn params(&self) -> &'static [ParamInfo] {
        VELOCITY_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.out_low = v.round().clamp(1.0, 127.0) as u8,
            1 => self.p.out_high = v.round().clamp(1.0, 127.0) as u8,
            2 => self.p.random = v.round().clamp(0.0, 64.0) as u8,
            3 => self.p.curve = v,
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Velocity"
    }
}

// ---------------------------------------------------------------------------
// Time-based: Note Length and Arpeggiator
// ---------------------------------------------------------------------------

/// Note-offs scheduled for a future song beat.
struct PendingOffs {
    items: [(f64, u8); 256],
    len: usize,
}

impl PendingOffs {
    fn new() -> Self {
        Self { items: [(0.0, 0); 256], len: 0 }
    }

    fn push(&mut self, beat: f64, note: u8) {
        // Replace an existing pending off for the same note.
        if let Some(it) = self.items[..self.len].iter_mut().find(|it| it.1 == note) {
            it.0 = beat;
            return;
        }
        if self.len < self.items.len() {
            self.items[self.len] = (beat, note);
            self.len += 1;
        }
    }

    fn take(&mut self, note: u8) -> bool {
        if let Some(i) = self.items[..self.len].iter().position(|it| it.1 == note) {
            self.items[i] = self.items[self.len - 1];
            self.len -= 1;
            true
        } else {
            false
        }
    }

    /// Emit offs due before `b1`.
    fn emit_due(&mut self, b0: f64, b1: f64, bpf: f64, frames: u32, out: &mut EventList) {
        let mut i = 0;
        while i < self.len {
            let (beat, note) = self.items[i];
            if beat < b1 {
                let f = (((beat - b0) / bpf).max(0.0).round() as u32).min(frames.saturating_sub(1));
                out.push(MidiEvent::off(f, note));
                self.items[i] = self.items[self.len - 1];
                self.len -= 1;
            } else {
                i += 1;
            }
        }
    }

    fn clear(&mut self) {
        self.len = 0;
    }
}

pub struct NoteLength {
    p: NoteLengthParams,
    pending: PendingOffs,
    held_vel: [u8; 128],
}

impl NoteLength {
    pub fn new(p: NoteLengthParams) -> Self {
        Self { p, pending: PendingOffs::new(), held_vel: [0; 128] }
    }
}

const NOTE_LENGTH_PARAMS: &[ParamInfo] = &[ParamInfo::new("Length", 0.03125, 16.0, "beats").at("length")];

impl MidiEffect for NoteLength {
    fn process(&mut self, ctx: &ProcessContext, frames: u32, input: &EventList, out: &mut EventList) {
        let bpf = beats_per_frame(ctx);
        let b0 = ctx.beat;
        for e in input.as_slice() {
            self.pending.emit_due(b0, b0 + e.frame as f64 * bpf + 1e-9, bpf, frames, out);
            let trigger = if self.p.on_release { !e.on } else { e.on };
            if e.on {
                self.held_vel[e.note as usize & 127] = e.velocity;
            }
            if trigger {
                if self.pending.take(e.note) {
                    out.push(MidiEvent::off(e.frame, e.note));
                }
                let vel = if e.on { e.velocity } else { self.held_vel[e.note as usize & 127].max(1) };
                out.push(MidiEvent::on(e.frame, e.note, vel));
                self.pending.push(b0 + e.frame as f64 * bpf + self.p.length.max(1.0 / 128.0), e.note);
            }
        }
        self.pending.emit_due(b0, b0 + frames as f64 * bpf, bpf, frames, out);
        out.sort();
    }
    fn reset(&mut self) {
        self.pending.clear();
    }
    fn params(&self) -> &'static [ParamInfo] {
        NOTE_LENGTH_PARAMS
    }
    fn set_param(&mut self, i: usize, v: f32) {
        if i == 0 {
            self.p.length = v as f64;
        }
    }
    fn name(&self) -> &str {
        "Note Length"
    }
}

pub struct Arpeggiator {
    p: ArpParams,
    /// Held notes in the order played: (pitch, velocity).
    held: [(u8, u8); 32],
    n_held: usize,
    /// Keys physically down (for hold mode).
    down: usize,
    /// Latched notes should be replaced on the next press (hold mode).
    latch_stale: bool,
    step: usize,
    pending: PendingOffs,
    rng: Noise,
    last_step_beat: f64,
}

impl Arpeggiator {
    pub fn new(p: ArpParams) -> Self {
        Self {
            p,
            held: [(0, 0); 32],
            n_held: 0,
            down: 0,
            latch_stale: false,
            step: 0,
            pending: PendingOffs::new(),
            rng: Noise::with_seed(0xa4b1_c3d5),
            last_step_beat: -1.0,
        }
    }

    fn add(&mut self, note: u8, vel: u8) {
        if self.p.hold && self.latch_stale {
            self.n_held = 0;
            self.latch_stale = false;
            self.step = 0;
        }
        if self.held[..self.n_held].iter().any(|h| h.0 == note) {
            return;
        }
        if self.n_held < self.held.len() {
            self.held[self.n_held] = (note, vel);
            self.n_held += 1;
        }
    }

    fn remove(&mut self, note: u8) {
        if self.p.hold {
            return;
        }
        if let Some(i) = self.held[..self.n_held].iter().position(|h| h.0 == note) {
            for k in i..self.n_held - 1 {
                self.held[k] = self.held[k + 1];
            }
            self.n_held -= 1;
        }
    }

    /// The `k`-th note of the pattern sequence (with octaves) and its length.
    fn sequence_note(&mut self, k: usize) -> Option<(u8, u8)> {
        let n = self.n_held;
        if n == 0 {
            return None;
        }
        let oct = self.p.octaves.max(1) as usize;
        let mut sorted = [(0u8, 0u8); 32];
        sorted[..n].copy_from_slice(&self.held[..n]);
        if self.p.style != ArpStyle::AsPlayed {
            sorted[..n].sort_by_key(|h| h.0);
        }
        let total = n * oct;
        let idx_up = |i: usize| {
            let (p, v) = sorted[i % n];
            ((p as usize + 12 * (i / n)).min(127) as u8, v)
        };
        let pick = match self.p.style {
            ArpStyle::Up | ArpStyle::AsPlayed | ArpStyle::Chord => idx_up(k % total),
            ArpStyle::Down => idx_up(total - 1 - k % total),
            ArpStyle::UpDown | ArpStyle::DownUp => {
                let cycle = if total > 1 { 2 * total - 2 } else { 1 };
                let mut i = k % cycle;
                if i >= total {
                    i = cycle - i;
                }
                if self.p.style == ArpStyle::DownUp {
                    i = total - 1 - i;
                }
                idx_up(i)
            }
            ArpStyle::Random => idx_up((self.rng.next_u32() as usize) % total),
        };
        Some(pick)
    }
}

const ARP_PARAMS: &[ParamInfo] = &[
    ParamInfo::new("Rate", 0.0625, 2.0, "beats").at("rate"),
    ParamInfo::new("Gate", 0.05, 2.0, "").at("gate"),
    ParamInfo::new("Octaves", 1.0, 4.0, "").at("octaves"),
];

impl MidiEffect for Arpeggiator {
    fn process(&mut self, ctx: &ProcessContext, frames: u32, input: &EventList, out: &mut EventList) {
        let bpf = beats_per_frame(ctx);
        let b0 = ctx.beat;
        let b1 = b0 + frames as f64 * bpf;
        let rate = self.p.rate.max(1.0 / 64.0);
        let mut ev = input.as_slice().iter().peekable();

        // Walk the block, stopping at each input event and each step line.
        let mut cursor = b0;
        loop {
            let next_step = (cursor / rate - 1e-9).ceil() * rate;
            let next_ev_beat = ev.peek().map(|e| b0 + e.frame as f64 * bpf);
            match next_ev_beat {
                Some(eb) if eb <= next_step || next_step >= b1 => {
                    self.pending.emit_due(b0, eb + 1e-9, bpf, frames, out);
                    let e = ev.next().expect("peeked");
                    if e.on {
                        self.add(e.note, e.velocity);
                        self.down += 1;
                    } else {
                        self.down = self.down.saturating_sub(1);
                        self.remove(e.note);
                        if self.p.hold && self.down == 0 {
                            self.latch_stale = true;
                        }
                    }
                    cursor = eb;
                }
                _ => {
                    if next_step >= b1 {
                        break;
                    }
                    self.pending.emit_due(b0, next_step + 1e-9, bpf, frames, out);
                    if (next_step - self.last_step_beat).abs() > 1e-9 {
                        self.last_step_beat = next_step;
                        let f = (((next_step - b0) / bpf).round().max(0.0) as u32).min(frames.saturating_sub(1));
                        let off_at = next_step + rate * self.p.gate.clamp(0.01, 4.0) as f64;
                        if self.p.style == ArpStyle::Chord {
                            for i in 0..self.n_held {
                                let (note, v) = self.held[i];
                                let vel = self.p.velocity.unwrap_or(v);
                                if self.pending.take(note) {
                                    out.push(MidiEvent::off(f, note));
                                }
                                out.push(MidiEvent::on(f, note, vel));
                                self.pending.push(off_at, note);
                            }
                        } else if let Some((note, v)) = self.sequence_note(self.step) {
                            let vel = self.p.velocity.unwrap_or(v);
                            if self.pending.take(note) {
                                out.push(MidiEvent::off(f, note));
                            }
                            out.push(MidiEvent::on(f, note, vel));
                            self.pending.push(off_at, note);
                            self.step += 1;
                        }
                        if self.n_held == 0 {
                            self.step = 0;
                        }
                    }
                    cursor = next_step + 1e-9;
                }
            }
        }
        self.pending.emit_due(b0, b1, bpf, frames, out);
        out.sort();
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.n_held = 0;
        self.down = 0;
        self.step = 0;
        self.last_step_beat = -1.0;
    }

    fn params(&self) -> &'static [ParamInfo] {
        ARP_PARAMS
    }

    fn set_param(&mut self, i: usize, v: f32) {
        match i {
            0 => self.p.rate = v as f64,
            1 => self.p.gate = v,
            2 => self.p.octaves = v.round().clamp(1.0, 4.0) as u8,
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "Arpeggiator"
    }
}

pub fn build_midi_effect(spec: &DeviceSpec) -> Option<Box<dyn MidiEffect>> {
    Some(match spec {
        DeviceSpec::Arpeggiator(p) => Box::new(Arpeggiator::new(p.clone())),
        DeviceSpec::Chord(p) => Box::new(Chord::new(p.clone())),
        DeviceSpec::Scale(p) => Box::new(Scale::new(p.clone())),
        DeviceSpec::RandomNote(p) => Box::new(RandomNote::new(p.clone())),
        DeviceSpec::NoteLength(p) => Box::new(NoteLength::new(p.clone())),
        DeviceSpec::Velocity(p) => Box::new(Velocity::new(p.clone())),
        DeviceSpec::Pitch(p) => Box::new(Pitch::new(p.clone())),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(beat: f64) -> ProcessContext {
        // 120 bpm at 1 kHz: 500 frames per beat, easy arithmetic.
        ProcessContext { sample_rate: 1000.0, tempo: 120.0, beat, playing: true }
    }

    fn list(evs: &[MidiEvent]) -> EventList {
        let mut l = EventList::new();
        for e in evs {
            l.push(*e);
        }
        l
    }

    fn run(fx: &mut dyn MidiEffect, beat: f64, frames: u32, evs: &[MidiEvent]) -> Vec<MidiEvent> {
        let mut out = EventList::new();
        fx.process(&ctx(beat), frames, &list(evs), &mut out);
        out.as_slice().to_vec()
    }

    #[test]
    fn chord_adds_and_releases_intervals() {
        let mut c = Chord::new(ChordParams { intervals: vec![4, 7], velocity: 1.0 });
        let on = run(&mut c, 0.0, 100, &[MidiEvent::on(10, 60, 100)]);
        assert_eq!(on.iter().map(|e| e.note).collect::<Vec<_>>(), vec![60, 64, 67]);
        // Change the chord while held: the off must still release 64 and 67.
        c.p.intervals = vec![3];
        let off = run(&mut c, 0.0, 100, &[MidiEvent::off(20, 60)]);
        assert_eq!(off.iter().filter(|e| !e.on).count(), 3);
    }

    #[test]
    fn shared_chord_notes_are_refcounted() {
        let mut c = Chord::new(ChordParams { intervals: vec![4], velocity: 1.0 });
        // C -> C,E and E -> E,G#: E is shared.
        run(&mut c, 0.0, 100, &[MidiEvent::on(0, 60, 100), MidiEvent::on(1, 64, 100)]);
        let off = run(&mut c, 0.0, 100, &[MidiEvent::off(2, 60)]);
        // Releasing C must not cut E, which the other key still holds.
        assert_eq!(off.iter().map(|e| e.note).collect::<Vec<_>>(), vec![60]);
    }

    #[test]
    fn scale_snaps_to_key() {
        let mut s = Scale::new(ScaleParams { root: 0, scale: ScaleKind::Major, transpose: 0 });
        let out = run(&mut s, 0.0, 100, &[MidiEvent::on(0, 61, 100), MidiEvent::on(0, 66, 100)]);
        assert_eq!(out.iter().map(|e| e.note).collect::<Vec<_>>(), vec![60, 65]);
        assert_eq!(ScaleKind::MinorPentatonic.snap(9, 70), 69);
    }

    #[test]
    fn arpeggiator_steps_on_the_grid() {
        let mut a = Arpeggiator::new(ArpParams { style: ArpStyle::Up, rate: 0.25, gate: 0.5, octaves: 1, hold: false, velocity: None });
        // Hold E and C; one beat is 500 frames, so steps every 125 frames.
        let out = run(&mut a, 0.0, 500, &[MidiEvent::on(0, 64, 90), MidiEvent::on(0, 60, 90)]);
        let ons: Vec<(u32, u8)> = out.iter().filter(|e| e.on).map(|e| (e.frame, e.note)).collect();
        assert_eq!(ons, vec![(0, 60), (125, 64), (250, 60), (375, 64)]);
        let offs: Vec<u32> = out.iter().filter(|e| !e.on).map(|e| e.frame).collect();
        assert_eq!(offs, vec![63, 188, 313, 438]);
    }

    #[test]
    fn arpeggiator_up_down_with_octaves() {
        let mut a = Arpeggiator::new(ArpParams { style: ArpStyle::UpDown, rate: 0.25, gate: 0.5, octaves: 2, hold: false, velocity: Some(100) });
        let out = run(&mut a, 0.0, 1000, &[MidiEvent::on(0, 60, 90), MidiEvent::on(0, 64, 90)]);
        let notes: Vec<u8> = out.iter().filter(|e| e.on).map(|e| e.note).collect();
        assert_eq!(notes, vec![60, 64, 72, 76, 72, 64, 60, 64]);
        assert!(out.iter().filter(|e| e.on).all(|e| e.velocity == 100));
    }

    #[test]
    fn arpeggiator_stops_when_keys_released_and_holds_when_latched() {
        let mut a = Arpeggiator::new(ArpParams { rate: 0.25, ..Default::default() });
        run(&mut a, 0.0, 250, &[MidiEvent::on(0, 60, 90)]);
        let after = run(&mut a, 0.5, 500, &[MidiEvent::off(0, 60)]);
        assert!(after.iter().all(|e| !e.on), "arp kept playing after release");

        let mut h = Arpeggiator::new(ArpParams { rate: 0.25, hold: true, ..Default::default() });
        run(&mut h, 0.0, 250, &[MidiEvent::on(0, 60, 90), MidiEvent::off(10, 60)]);
        let latched = run(&mut h, 0.5, 500, &[]);
        assert!(latched.iter().any(|e| e.on && e.note == 60), "hold mode should keep arpeggiating");
    }

    #[test]
    fn note_length_schedules_offs_across_blocks() {
        let mut n = NoteLength::new(NoteLengthParams { length: 1.0, on_release: false });
        let a = run(&mut n, 0.0, 250, &[MidiEvent::on(100, 60, 100), MidiEvent::off(120, 60)]);
        assert_eq!(a.len(), 1, "input note-off must be swallowed");
        // Off due at beat 0.2 + 1.0 = 1.2 -> second block starting at beat 1.0, frame 100.
        let b = run(&mut n, 1.0, 250, &[]);
        assert_eq!(b, vec![MidiEvent::off(100, 60)]);
    }

    #[test]
    fn velocity_rescales() {
        let mut v = Velocity::new(VelocityParams { out_low: 64, out_high: 100, random: 0, curve: 1.0 });
        assert_eq!(v.map(1), 64);
        assert_eq!(v.map(127), 100);
    }

    #[test]
    fn pitch_transposes_and_filters() {
        let mut p = Pitch::new(PitchParams { semitones: 12, range: Some((0, 80)) });
        let out = run(&mut p, 0.0, 10, &[MidiEvent::on(0, 60, 100), MidiEvent::on(0, 70, 100)]);
        assert_eq!(out.iter().map(|e| e.note).collect::<Vec<_>>(), vec![72]);
    }
}
