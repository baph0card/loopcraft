//! Rendering: the per-chunk signal flow.

use super::track::*;
use super::*;
use crate::state::NONE;
use loopcraft_dsp::ProcessContext;
use std::sync::atomic::Ordering::Relaxed;

impl Core {
    /// Render into separate main and cue buffers of equal length. Input (if
    /// any) has been placed in `self.input` by the caller, starting at frame
    /// `in_offset`.
    pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32], cue_l: &mut [f32], cue_r: &mut [f32], in_offset: usize) {
        let total = out_l.len().min(out_r.len());
        let started = std::time::Instant::now();
        let mut done = 0;
        while done < total {
            let max = (total - done).min(CHUNK);
            let n = self.frames_until_boundary(max);
            self.render_chunk(in_offset + done, n);
            out_l[done..done + n].copy_from_slice(&self.master.l[..n]);
            out_r[done..done + n].copy_from_slice(&self.master.r[..n]);
            if done + n <= cue_l.len() {
                cue_l[done..done + n].copy_from_slice(&self.cue_l[..n]);
                cue_r[done..done + n].copy_from_slice(&self.cue_r[..n]);
            }
            done += n;
        }
        let budget = total as f32 / self.sr;
        self.state.cpu_load.store(started.elapsed().as_secs_f32() / budget.max(1e-6));
    }

    /// Fill each track's `in_l/in_r` for this chunk from its input source.
    fn gather_inputs(&mut self, in_off: usize, n: usize) {
        let have = self.input_frames;
        let chans = self.input_channels;
        for i in 0..self.tracks.len() {
            let src = self.tracks[i].input;
            let needs = self.tracks[i].kind == TrackKind::Audio && (self.tracks[i].armed || self.tracks[i].monitor == Monitor::In || self.recordings.iter().any(|r| r.track == i));
            if !needs {
                self.tracks[i].has_input = false;
                continue;
            }
            let (mut l, mut r) = (std::mem::take(&mut self.tracks[i].in_l), std::mem::take(&mut self.tracks[i].in_r));
            l[..n].fill(0.0);
            r[..n].fill(0.0);
            match src {
                InputSource::None => {}
                InputSource::Stereo(p) => {
                    let (a, b) = (2 * p as usize, 2 * p as usize + 1);
                    if in_off + n <= have {
                        if a < chans {
                            l[..n].copy_from_slice(&self.input[a][in_off..in_off + n]);
                        }
                        if b < chans {
                            r[..n].copy_from_slice(&self.input[b][in_off..in_off + n]);
                        } else {
                            r[..n].copy_from_slice(&l[..n]);
                        }
                    }
                }
                InputSource::Mono(c) => {
                    let c = c as usize;
                    if in_off + n <= have && c < chans {
                        l[..n].copy_from_slice(&self.input[c][in_off..in_off + n]);
                        r[..n].copy_from_slice(&l[..n]);
                    }
                }
                InputSource::Track(t) => {
                    // Previous chunk's post-fader output (one chunk of latency).
                    if let Some(src) = self.tracks.get(t).filter(|_| t != i) {
                        l[..n].copy_from_slice(&src.out_l[..n]);
                        r[..n].copy_from_slice(&src.out_r[..n]);
                    }
                }
                InputSource::Master => {
                    l[..n].copy_from_slice(&self.master_prev_l[..n]);
                    r[..n].copy_from_slice(&self.master_prev_r[..n]);
                }
            }
            let pk = loopcraft_dsp::peak(&l[..n]).max(loopcraft_dsp::peak(&r[..n]));
            let ts = &self.state.tracks[i];
            ts.input_peak.store(pk.max(ts.input_peak.load() * 0.85));
            self.tracks[i].in_l = l;
            self.tracks[i].in_r = r;
            self.tracks[i].has_input = true;
        }
    }

    /// Stream recorded audio to the UI.
    fn write_audio_recordings(&mut self, n: usize) {
        let now = self.beat;
        for rec in self.recordings.iter_mut() {
            if now + EPS < rec.start {
                continue;
            }
            let RecKind::Audio(a) = &mut rec.kind else { continue };
            let Some(t) = self.tracks.get(rec.track) else { continue };
            if !t.has_input {
                continue;
            }
            let room = a.tx.slots() / 2;
            let k = n.min(room);
            if let Ok(mut chunk) = a.tx.write_chunk(k * 2) {
                let (s1, s2) = chunk.as_mut_slices();
                for f in 0..k {
                    let (l, r) = (t.in_l[f], t.in_r[f]);
                    let i0 = 2 * f;
                    for (j, v) in [(i0, l), (i0 + 1, r)] {
                        if j < s1.len() {
                            s1[j] = v;
                        } else {
                            s2[j - s1.len()] = v;
                        }
                    }
                }
                chunk.commit_all();
            }
            a.frames += k as u64;
            a.dropped += (n - k) as u64;
        }
    }

    fn compute_solo(&mut self) -> bool {
        let n = self.tracks.len();
        let any = self.tracks.iter().any(|t| t.solo);
        if !any {
            return false;
        }
        for i in 0..n {
            let mut rel = self.tracks[i].solo;
            // Ancestor soloed?
            let mut cur = self.tracks[i].parent;
            while let (false, Some(p)) = (rel, cur) {
                rel = self.tracks[p].solo;
                cur = self.tracks[p].parent;
            }
            self.solo_rel[i] = rel;
        }
        // Groups whose descendants are soloed must pass audio too.
        for i in 0..n {
            if self.tracks[i].solo {
                let mut cur = self.tracks[i].parent;
                while let Some(p) = cur {
                    self.solo_rel[p] = true;
                    cur = self.tracks[p].parent;
                }
            }
        }
        true
    }

    pub(crate) fn render_chunk(&mut self, in_off: usize, n: usize) {
        let bpf = self.beats_per_frame();

        // Count-in: only the metronome sounds; the transport is frozen.
        let counting = if let Some((done, total)) = self.count_in {
            let b0 = done;
            self.master.clear(n);
            self.cue_l[..n].fill(0.0);
            self.cue_r[..n].fill(0.0);
            self.render_click(b0, bpf, n, false);
            let done = done + n as f64 * bpf;
            if done >= total - EPS {
                self.count_in = None;
                self.playing = true;
            } else {
                self.count_in = Some((done, total));
            }
            self.state.count_in_left.store((total - done).max(0.0));
            true
        } else {
            false
        };
        if counting {
            for t in self.tracks.iter_mut() {
                t.chan.clear(n);
            }
            self.free_beat += n as f64 * bpf;
            self.publish();
            return;
        }
        self.state.count_in_left.store(0.0);

        if self.playing {
            self.apply_due_events();
        }
        let bpf = self.beats_per_frame();
        let b0 = self.beat;
        let b1 = if self.playing { b0 + n as f64 * bpf } else { b0 };
        let ctx = ProcessContext { sample_rate: self.sr, tempo: self.tempo, beat: b0, playing: self.playing };
        // Devices keep a running clock even while stopped (arps, LFOs).
        let fx_ctx = ProcessContext { beat: if self.playing { b0 } else { self.free_beat }, ..ctx };

        self.gather_inputs(in_off, n);

        // Phase A: sources (clips, instruments, monitored input).
        for t in self.tracks.iter_mut() {
            t.chan.clear(n);
            if t.kind == TrackKind::Group {
                continue;
            }
            render_track_source(t, &ctx, &fx_ctx, b0, b1, n, self.playing);
            if t.has_input && t.monitoring() {
                for k in 0..n {
                    t.chan.l[k] += t.in_l[k];
                    t.chan.r[k] += t.in_r[k];
                }
            }
            if self.playing && t.follows_arrangement && t.session.is_none() {
                apply_automation(t, b0);
            }
            if t.is_key_source {
                t.key_l[..n].copy_from_slice(&t.chan.l[..n]);
                t.key_r[..n].copy_from_slice(&t.chan.r[..n]);
            }
        }
        if self.playing {
            self.write_audio_recordings(n);
        }

        // Phase B: insert chains, faders and routing, children before groups.
        for r in self.returns.iter_mut() {
            r.clear(n);
        }
        self.master.clear(n);
        self.bus_l[..n].fill(0.0);
        self.bus_r[..n].fill(0.0);
        self.cue_l[..n].fill(0.0);
        self.cue_r[..n].fill(0.0);
        let any_solo = self.compute_solo();
        let x = (self.crossfader + 1.0) * 0.5;
        let (xa, xb) = ((2.0 * (1.0 - x)).min(1.0), (2.0 * x).min(1.0));
        for oi in 0..self.order.len() {
            let i = self.order[oi];
            if self.tracks[i].kind == TrackKind::Group && self.tracks[i].is_key_source {
                let t = &mut self.tracks[i];
                t.key_l[..n].copy_from_slice(&t.chan.l[..n]);
                t.key_r[..n].copy_from_slice(&t.chan.r[..n]);
            }
            // Sidechain keys.
            for e in 0..self.tracks[i].chan.effects.len() {
                let Some(src) = self.tracks[i].chan.effects[e].fx.wants_sidechain() else { continue };
                if src < self.tracks.len() && self.tracks[src].is_key_source {
                    let (kl, kr) = (self.tracks[src].key_l.as_ptr(), self.tracks[src].key_r.as_ptr());
                    // SAFETY: key buffers are never resized after construction,
                    // and are distinct allocations from the effect being called.
                    let (kl, kr) = unsafe { (std::slice::from_raw_parts(kl, n), std::slice::from_raw_parts(kr, n)) };
                    self.tracks[i].chan.effects[e].fx.sidechain(kl, kr);
                }
            }
            let t = &mut self.tracks[i];
            t.chan.run_effects(&fx_ctx, n);
            let audible = !any_solo || self.solo_rel[i] || self.cue_mode;
            let xf = match t.crossfade {
                CrossfadeAssign::None => 1.0,
                CrossfadeAssign::A => xa,
                CrossfadeAssign::B => xb,
            };
            let (pl, pr) = t.chan.apply_fader(n, !audible, xf);
            self.state.tracks[i].channel.set_peaks(pl, pr);
            t.out_l[..n].copy_from_slice(&t.chan.l[..n]);
            t.out_r[..n].copy_from_slice(&t.chan.r[..n]);
            // Post-fader sends.
            for (ri, ret) in self.returns.iter_mut().enumerate() {
                let g = t.sends.get(ri).copied().unwrap_or(0.0);
                if g > 0.0 {
                    for k in 0..n {
                        ret.l[k] += t.chan.l[k] * g;
                        ret.r[k] += t.chan.r[k] * g;
                    }
                }
            }
            if self.cue_mode && t.solo {
                for k in 0..n {
                    self.cue_l[k] += t.chan.l[k];
                    self.cue_r[k] += t.chan.r[k];
                }
            }
            let parent = t.parent;
            let (src_l, src_r) = (t.chan.l.as_ptr(), t.chan.r.as_ptr());
            match parent {
                Some(p) if p < self.tracks.len() && p != i => {
                    // SAFETY: parent and child are different tracks, so the
                    // buffers are distinct; neither is resized here.
                    let (sl, sr) = unsafe { (std::slice::from_raw_parts(src_l, n), std::slice::from_raw_parts(src_r, n)) };
                    let pc = &mut self.tracks[p].chan;
                    for k in 0..n {
                        pc.l[k] += sl[k];
                        pc.r[k] += sr[k];
                    }
                }
                _ => {
                    let (sl, sr) = (&self.tracks[i].chan.l, &self.tracks[i].chan.r);
                    for k in 0..n {
                        self.bus_l[k] += sl[k];
                        self.bus_r[k] += sr[k];
                    }
                }
            }
        }

        // Align the track bus with the slowest return.
        if let Some(d) = self.bus_pdc.as_mut() {
            d.process(&mut self.bus_l[..n], &mut self.bus_r[..n]);
        }
        self.master.l[..n].copy_from_slice(&self.bus_l[..n]);
        self.master.r[..n].copy_from_slice(&self.bus_r[..n]);

        // Returns (they may also send to later returns).
        for ri in 0..self.returns.len() {
            self.returns[ri].run_effects(&fx_ctx, n);
            let (pl, pr) = self.returns[ri].apply_fader(n, false, 1.0);
            self.state.returns[ri].set_peaks(pl, pr);
            for rj in ri + 1..self.returns.len() {
                let g = self.return_sends.get(ri).and_then(|s| s.get(rj)).copied().unwrap_or(0.0);
                if g > 0.0 {
                    let (a, b) = self.returns.split_at_mut(rj);
                    for k in 0..n {
                        b[0].l[k] += a[ri].l[k] * g;
                        b[0].r[k] += a[ri].r[k] * g;
                    }
                }
            }
            let ret = &self.returns[ri];
            for k in 0..n {
                self.master.l[k] += ret.l[k];
                self.master.r[k] += ret.r[k];
            }
        }
        if self.metronome && self.playing {
            self.render_click(b0, bpf, n, self.cue_mode);
        }
        self.master.run_effects(&fx_ctx, n);
        let (pl, pr) = self.master.apply_fader(n, false, 1.0);
        self.state.master.set_peaks(pl, pr);
        self.master_prev_l[..n].copy_from_slice(&self.master.l[..n]);
        self.master_prev_r[..n].copy_from_slice(&self.master.r[..n]);
        let cg = self.cue_gain;
        let (mut cpl, mut cpr) = (0.0f32, 0.0f32);
        for k in 0..n {
            self.cue_l[k] *= cg;
            self.cue_r[k] *= cg;
            cpl = cpl.max(self.cue_l[k].abs());
            cpr = cpr.max(self.cue_r[k].abs());
        }
        self.state.cue.set_peaks(cpl, cpr);

        // Advance the transport.
        self.free_beat += n as f64 * bpf;
        if self.playing {
            self.beat = b1;
            if let Some((ls, le)) = self.loop_region
                && self.beat >= le - EPS
            {
                self.beat = ls + (self.beat - le).max(0.0);
                for t in self.tracks.iter_mut().filter(|t| t.follows_arrangement && t.session.is_none()) {
                    t.flush_notes = true;
                    t.arr_active = None;
                }
            }
        }
        self.publish();
    }

    /// Metronome clicks for beats in `[b0, b0 + n*bpf)`, into master (or cue).
    pub(crate) fn render_click(&mut self, b0: Beats, bpf: f64, n: usize, to_cue: bool) {
        let bar = self.sig.beats_per_bar();
        let mut k = (b0 - 1e-9).ceil();
        let mut next_click = if (k - b0) / bpf < n as f64 { Some(((k - b0) / bpf).round() as usize) } else { None };
        for i in 0..n {
            if Some(i) == next_click {
                let accent = (k / bar).fract().abs() < 1e-6;
                self.click =
                    Click { t: 0, len: (0.03 * self.sr) as usize, freq: if accent { 1_600.0 } else { 1_000.0 }, phase: 0.0, amp: if accent { 0.5 } else { 0.3 } };
                k += 1.0;
                next_click = if (k - b0) / bpf < n as f64 { Some(((k - b0) / bpf).round() as usize) } else { None };
            }
            let c = &mut self.click;
            if c.t < c.len {
                let env = 1.0 - c.t as f32 / c.len as f32;
                let s = (c.phase * std::f32::consts::TAU).sin() * c.amp * env * env;
                c.phase = (c.phase + c.freq / self.sr) % 1.0;
                c.t += 1;
                if to_cue {
                    self.cue_l[i] += s;
                    self.cue_r[i] += s;
                } else {
                    self.master.l[i] += s;
                    self.master.r[i] += s;
                }
            }
        }
    }

    pub(crate) fn publish(&self) {
        let st = &self.state;
        st.playing.store(self.playing, Relaxed);
        st.beat.store(self.beat);
        st.tempo.store(self.tempo);
        st.tempo_follows_arrangement.store(self.tempo_auto, Relaxed);
        st.next_scene.store(self.scene_follow.map_or(NONE, |(_, s)| s as i32), Relaxed);
        st.input_channels.store(self.input_channels as u32, Relaxed);
        for (t, ts) in self.tracks.iter().zip(st.tracks.iter()) {
            ts.playing_slot.store(t.session.map_or(NONE, |s| s.slot as i32), Relaxed);
            match t.pending {
                Some(Pending { kind: PendingKind::Launch(s), .. }) => {
                    ts.queued_slot.store(s as i32, Relaxed);
                    ts.stop_queued.store(false, Relaxed);
                }
                Some(Pending { kind: PendingKind::Stop, .. }) => {
                    ts.queued_slot.store(NONE, Relaxed);
                    ts.stop_queued.store(true, Relaxed);
                }
                None => {
                    ts.queued_slot.store(NONE, Relaxed);
                    ts.stop_queued.store(false, Relaxed);
                }
            }
            ts.clip_position.store(t.clip_pos);
            ts.follows_arrangement.store(t.follows_arrangement, Relaxed);
            ts.arrangement_clip.store(if t.session.is_none() { t.arr_active.map_or(NONE, |a| a as i32) } else { NONE }, Relaxed);
        }
    }
}
