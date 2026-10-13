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

## Signal flow

Per chunk: each track renders its source (MIDI effects → instrument, or audio clip / live input), then its insert chain, then its fader, crossfader gain and post-fader sends. Children render before their groups, so a group's input is the sum of its children. Returns render next (a return can send to later returns), then the master. With cue mode on, soloed tracks also go to outputs 3/4.

**Delay compensation.** Each effect reports its latency. When the graph is built, every channel's path latency is computed, and siblings that feed the same bus get delay lines (`FixedDelay`) so they arrive aligned. Returns are compensated against each other, and the bus is delayed by the largest return latency. Racks do the same between their chains. Offline renders drop the leading latency so exports start on time.

## Recording

Recording buffers are allocated on the UI thread and handed to the engine inside the command, so the audio thread never allocates:
- **MIDI** takes are a `RecordBuffer` with capacity for 8192 notes.
- **Audio** takes are an SPSC ring that the UI drains every frame into memory, then writes to a WAV file when the take ends.

Takes start on the next quantization boundary (or immediately, or when the count-in ends) and stop on the next boundary after Stop. Finished takes come back as events, and the UI turns them into clips. A MIDI take that started while a clip was playing in the same slot comes back as an overdub, with note times already wrapped into the clip's loop.

## Audio clips and warping

An audio clip's position is always derived from song beats: `source_seconds = seconds_at(clip_beat)`. Without warp markers this is `(start_offset + clip_beat) × 60 / original_bpm`; with markers it's piecewise linear between them. That mapping is what keeps loops locked to the grid at any tempo.

- **Repitch** reads the source at that position directly, so speed and pitch follow tempo.
- **Stretch** feeds the mapped position to a WSOLA stretcher: every 30 ms it starts a 60 ms Hann-windowed grain near the mapped position, nudged within ±8 ms to the offset whose waveform best continues the grain that's fading out. Grains read at the original speed, so pitch is preserved.
- **Beats** cuts the source at detected transients and starts each slice at its mapped beat, playing at the original speed. This keeps drums tight.
- **Complex** uses a phase vocoder (2048-point frames, hop 512) with identity phase locking. Pitch shifting moves whole spectral-peak regions.
- **Off** advances in real seconds instead of beats.

Arrangement regions add an `offset` (the clip position at the region's left edge), so splitting and left-trimming never rewrite clip contents.

## Devices

Built-in devices implement `Instrument`, `MidiEffect` or `Effect` from `loopcraft-dsp`. Each exposes a static parameter table (`ParamInfo`: name, range, unit, and a JSON-pointer `path` into the saved params). `dsp::specs` uses that path to read and write the same parameter on the saved `DeviceSpec`, and a test checks every catalog device, so the UI, automation and save files always agree on what parameter N means.

Racks are devices too. An Audio or Instrument Rack builds its chains from nested specs and exposes only its 8 macros as parameters. Moving a macro sweeps the mapped parameters of devices inside its chains.

Plugins come in through the engine's `PluginFactory` trait, which `loopcraft-plugin::HostFactory` implements for CLAP:
- **Parameter tables** are read through the params extension and registered in `dsp::plugin_params`, so a saved `PluginRef` can be edited by index like any built-in device.
- **Parameter values** are sent to the plugin as `CLAP_EVENT_PARAM_VALUE` events.
- **Latency** is read through the latency extension and feeds delay compensation.

VST3 and AU would implement the same trait.

## File format

`.loopcraft` files are versioned, pretty-printed JSON of `Project`. Unknown fields default, so older files open in newer builds; files from a newer format version are rejected with a clear message rather than misread. Audio is referenced by path relative to the project file.
