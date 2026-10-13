# Roadmap: the road to Live parity

This is an honest map of where LoopCraft stands against Ableton Live 12 Suite, area by area. It doubles as the to-do list for contributors.

**Legend:** ✅ done · 🟡 partial (see note) · ❌ not started

"Done" means implemented, wired into the app, and covered by automated tests where a test makes sense: 134 tests at the time of writing, plus a device audit that renders every built-in device through the engine. It does **not** mean listening-tested on real hardware. The development machine had no audio or MIDI devices, so every real-time device path has only been run against the silent fallback driver. Treat sound quality and latency on real interfaces as unverified until someone reports back.

---

## Session view

| Feature | Status | Notes |
|---|---|---|
| Clip slot grid, launch / stop, scenes | ✅ | |
| Scene tempo / time signature | ✅ | Applied on the scene's launch boundary |
| Global and per-clip launch quantization | ✅ | None … 8 bars, 1/32 |
| Launch modes Trigger / Gate / Toggle | ✅ | Gate releases when the mouse (or mapped note) is released |
| Launch modes Repeat, Legato | ❌ | |
| Follow actions (clips and scenes) | ✅ | A/B with chance, linked, jump; groups are runs of filled slots |
| Group tracks: fold, group slots launch the row | ✅ | Nested groups supported |
| MIDI recording into slots | ✅ | Open-ended, quantized start/stop, loops seamlessly afterwards |
| MIDI overdub | ✅ | Session Record on an armed track that's playing a MIDI clip |
| Audio recording into slots | ✅ | Written as 32-bit float WAV to `Samples/Recorded` |
| Capture MIDI | ❌ | |
| Session → Arrangement recording | 🟡 | Arrangement Record captures armed tracks' *input*; recording the Session's clip launches onto the timeline isn't done |
| Clip / track / scene colors and renaming | ✅ | |
| Copy / paste / duplicate clips | ✅ | |

## Arrangement view

| Feature | Status | Notes |
|---|---|---|
| Timeline playback (MIDI + audio) | ✅ | |
| Regions: move (across tracks), resize, trim left edge | ✅ | Regions can start partway into a clip (offset) |
| Split, duplicate, consolidate | ✅ | Consolidate unrolls loops (MIDI) |
| Insert / delete time | ✅ | Shifts clips, automation, tempo points, locators |
| Loop brace | ✅ | Drag on the ruler |
| Automation playback and drawing | ✅ | Volume, pan, sends, every built-in device parameter, plugin parameters |
| Tempo automation | ✅ | Linear ramps between breakpoints |
| Time-signature automation | ❌ | |
| Locators | ✅ | Add, jump, delete |
| Arrangement recording (MIDI and audio) | ✅ | Overwrites what it covers |
| Take lanes / comping | ❌ | |
| Freeze / flatten | ❌ | |

## Audio

| Feature | Status | Notes |
|---|---|---|
| Import WAV, AIFF, FLAC, MP3, Ogg Vorbis | ✅ | symphonia; resampled to the engine rate on load |
| Warp: Repitch, Stretch (WSOLA), Beats (transient slices), Complex (phase vocoder), Off | ✅ | Complex is a phase-locked vocoder, not Live's Complex Pro |
| Warp markers | ✅ | Add, drag, delete in the clip view |
| Transient detection | ✅ | Used by Beats mode |
| Automatic tempo detection | 🟡 | Loop-length guess on import only |
| Clip gain, start offset, transpose, reverse, fades | ✅ | |
| Crossfades between adjacent regions | ❌ | |
| Audio input, monitoring (In / Auto / Off), input meters | ✅ | Untested on real interfaces |
| Input latency compensation for recordings | ❌ | Recorded audio isn't shifted by the device's input latency yet |
| Resampling (record another track or the master) | ✅ | |
| Audio-to-MIDI, stem separation | ❌ | |

## MIDI

| Feature | Status | Notes |
|---|---|---|
| Piano roll: add / move / resize / delete | ✅ | |
| Velocity lane, note chance | ✅ | |
| Quantize, transpose, duplicate loop | ✅ | |
| Multi-note selection, marquee, scale highlighting | ❌ | Single-note selection only |
| Groove pool | ✅ | Swing templates, amount, commit to notes |
| MIDI effects: Arpeggiator, Chord, Scale, Random, Note Length, Velocity, Pitch | ✅ | |
| Hardware MIDI input | ✅ | Notes to armed (or the selected) tracks; untested with real devices |
| MIDI file import / export | 🟡 | CLI only; not in the app's browser yet |
| Computer keyboard as piano | ✅ | Octave and velocity keys |
| MPE, MIDI clock, Ableton Link | ❌ | Link is GPL-2, so it would be an opt-in feature |

## Instruments

| Device | Status | Notes |
|---|---|---|
| Poly Synth (subtractive) | ✅ | |
| Wavetable | ✅ | Generated tables, position morphing, unison, mod envelope, LFO |
| FM Synth (4-operator, 8 algorithms) | ✅ | |
| Sampler | ✅ | Single sample, chromatic |
| Multi Sampler | ✅ | Key and velocity zones, loops, glide |
| Drum Rack | 🟡 | Synth or sample pads, choke groups, tuning; no per-pad effect chains or sends |
| Instrument Rack | ✅ | Parallel chains with key/velocity zones, 8 macros |
| Physical modelling, Drift-style, granular instruments | ❌ | |

## Audio effects

| Device | Status |
|---|---|
| EQ, Compressor (sidechain), Glue Compressor, Limiter, Gate, Multiband Dynamics | ✅ |
| Delay, Reverb, Convolution Reverb (synthesized IRs or a file) | ✅ |
| Saturator, Drum Buss, Amp + cabinet, Redux | ✅ |
| Chorus, Flanger, Phaser, Auto Pan, Auto Filter | ✅ |
| Beat Repeat, Grain Delay, Vocoder | ✅ |
| Utility | ✅ |
| Audio Effect Rack (parallel chains, macros) | ✅ |
| Echo-style character delay, Corpus, Spectral Resonator / Time, Pedal, Shifter | ❌ |

## Mixing and routing

| Feature | Status | Notes |
|---|---|---|
| Volume, pan, mute, solo, meters | ✅ | |
| Sends / returns (returns can feed later returns) | 🟡 | Post-fader only |
| Group tracks | ✅ | |
| Track inputs: interface channels, other tracks, master | ✅ | |
| Track output routing to arbitrary tracks | ❌ | Tracks go to their group or the master |
| Plugin / device delay compensation | ✅ | Including inside racks and for CLAP plugins that report latency |
| Crossfader with A/B assignment | ✅ | |
| Cue / pre-listen on outputs 3/4 | ✅ | Needs a 4-output interface |

## Plugins and extensibility

| Feature | Status | Notes |
|---|---|---|
| CLAP: scan, load, audio, notes | ✅ | |
| CLAP: parameters (edit, automate, map, save) | ✅ | Via the params extension |
| CLAP: latency reporting | ✅ | |
| CLAP: state chunks, plugin editor windows | ❌ | Without GUIs, parameters are the only way a plugin's state changes, and those are saved |
| CLAP: sidechain and multi-bus ports | ❌ | |
| VST3, Audio Units | ❌ | VST3 would be an opt-in GPL build feature |
| Scripted devices (a Max for Live alternative) | ❌ | Plan: WASM-sandboxed devices |

## Control and performance

| Feature | Status | Notes |
|---|---|---|
| MIDI Map mode (CC and notes), Key Map mode | ✅ | Ranges editable in the Mappings window |
| Macro mapping inside racks | ✅ | |
| Tap tempo | ✅ | |
| Count-in | ✅ | |
| Control surface scripts, Push-style grid controllers | ❌ | |
| Tempo follower | ❌ | |

## Files and workflow

| Feature | Status | Notes |
|---|---|---|
| Project save / open (`.loopcraft`), recent files | ✅ | Versioned JSON |
| Collect All and Save | ✅ | |
| Undo / redo | ✅ | Snapshot-based, 200 steps |
| Export WAV (16/24/32f), FLAC, AIFF; 44.1–96 kHz; stems | ✅ | MP3 export not done |
| Browser: devices, presets, plugins, samples, search | ✅ | |
| Sample preview in the browser, collections | ❌ | |
| Device presets | ✅ | JSON files in the library folder |
| Templates | ❌ | |
| Audio / MIDI settings | ✅ | Device, rate, buffer, MIDI ports |
| `.als` import | ❌ | Clean-room only, from documented behaviour of user files |

---

## What's next

The largest remaining gaps, roughly in order of how much they would matter to someone trying to use this for real work:

1. **Listening and hardware testing.** Run it on real interfaces and controllers, measure latency, and fix what that turns up.
2. **CLAP GUIs and state**, then VST3 behind an opt-in feature. Most people's sounds live in plugins.
3. **Piano-roll ergonomics:** multi-select, marquee, copy/paste notes, scale highlighting.
4. **Session → Arrangement recording, Capture MIDI, take lanes.**
5. **Output routing, pre-fader sends, Drum Rack pad chains.**
6. **MIDI clock / Link, control-surface scripting.**
7. **More devices:** Echo, spectral tools, physical models.
