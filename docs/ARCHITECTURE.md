# Architecture

## Threads

```
 UI thread (egui)                               Audio thread (cpal callback)
 ─────────────────                              ────────────────────────────
 Project  ──edit──► EngineHandle ──Command──►   Engine
   ▲                    ▲   (rtrb SPSC ring)      │  drain commands
   │                    │                         │  render chunks
   │              EngineEvent ◄─────────────────  │  post events (garbage, recordings)
   │              (rtrb SPSC ring)                │
   └─────── EngineState (atomics: playhead, meters, playing slots) ◄── publish
```

- The **project** (`loopcraft-core::Project`) is plain data and the source of truth. It's what gets saved.
- The **engine** (`loopcraft-engine::Core`) is built from a project on the UI thread, then moved to the audio thread.
- **Small edits** (fader, parameter, note edits) update the project and send a targeted `Command` (`SetVolume`, `SetDeviceParam`, `ReplaceClip`).
- **Structural edits** (add a track or device) update the project, build a new `Core`, and send `SwapCore`. The engine carries the transport position, tempo and playing clips across the swap, so playback doesn't stop.

## Real-time rules

The audio thread must never block. In practice:

- No allocation: event lists, scratch buffers and the outbox are preallocated with fixed capacity; overflow drops events rather than growing.
- No freeing: anything replaced on the audio thread (a clip, a whole `Core`) is sent back through the event ring and dropped on the UI thread. If that ring is full the object is leaked, never freed in the callback.
- No locks, no I/O. Audio files are decoded and resampled when the graph is built.

## Sample-accurate scheduling

Each host buffer is processed in chunks of at most 256 frames. Before each chunk the engine computes the next musical event (a quantized launch or stop, a scene tempo change, an arrangement clip edge, the loop end, a recording boundary) and shortens the chunk so the event lands exactly on the next chunk's first sample. Within a chunk, MIDI notes carry frame offsets, and instruments are rendered in sub-segments between events. The result: a clip launched at "next bar" starts on that bar's exact sample, which the test suite checks.

MIDI scheduling (`clip::schedule_midi`) treats note-ons as `[start, end)` and note-offs as `(start, end]` within each loop pass, with a tiny tolerance so boundary values shared by consecutive chunks tile without gaps. A note that ends on the loop point is therefore released before the loop retriggers it.

## Audio clips and warping

An audio clip's position is always derived from song beats: `source_sample = (start_offset + clip_beat) × 60 / original_bpm × sample_rate`. That mapping is what keeps loops locked to the grid at any tempo.

- **Repitch** reads the source at that position directly, so speed and pitch follow tempo.
- **Stretch** feeds the mapped position to a WSOLA stretcher: every 30 ms it starts a 60 ms Hann-windowed grain near the mapped position, nudged within ±8 ms to the offset whose waveform best continues the grain that's fading out. Grains read at the original speed, so pitch is preserved.
- **Off** advances in real seconds instead of beats.

## Devices

Built-in devices implement `Instrument` or `Effect` from `loopcraft-dsp`. Each exposes a static parameter table (`ParamInfo`: name, range, unit). `dsp::specs` reads and writes the same parameters on the saved `DeviceSpec`, and a test asserts the two tables match, so UI, automation and save files always agree on what parameter N means.

Plugins come in through the engine's `PluginFactory` trait. `loopcraft-plugin::HostFactory` implements it for CLAP; VST3 and AU will implement the same trait.

## File format

`.loopcraft` files are versioned, pretty-printed JSON of `Project`. Unknown fields default, so older files open in newer builds; files from a newer format version are rejected with a clear message rather than misread. Audio is referenced by path relative to the project file.
