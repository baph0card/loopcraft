# Roadmap: the road to Live parity

This is an honest map of where LoopCraft stands against Ableton Live 12 Suite, area by area. It doubles as the to-do list for contributors.

**Legend:** ✅ done · 🟡 partial (see note) · ❌ not started

"Done" means implemented, wired into the app, and covered by automated tests where a test makes sense: 159 tests at the time of writing, plus a device audit that renders every built-in device through the engine. It does **not** mean listening-tested on real hardware. The development machine had no audio or MIDI devices, so every real-time device path has only been run against the silent fallback driver. Treat sound quality and latency on real interfaces as unverified until someone reports back.

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
| Capture MIDI | ✅ | Takes the last phrase played (stopped or playing), sets the loop length and tempo-matched start, puts it in a slot or on the timeline |
| Session → Arrangement recording | ✅ | With Arrangement Record on, Session launches and stops are written to the timeline as regions |
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
| Take lanes / comping | ✅ | Loop recording makes one take per pass; drag across a take, or click it, to comp it onto the timeline |
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
| Multi-note selection, marquee, copy / paste, scale highlighting, fold | ✅ | Shift-click, marquee, Ctrl+A / C / V / D, arrow-key nudge and transpose; Alt disables snapping |
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
| Sends / returns (returns can feed later returns) | ✅ | Each return can be pre- or post-fader |
| Group tracks | ✅ | |
| Track inputs: interface channels, other tracks, master | ✅ | |
| Track output routing to other tracks | ✅ | Master, group, any other track, or sends only; routes that would loop are refused |
| Plugin / device delay compensation | ✅ | Including inside racks, along output routes, and for plugins that report latency |
| Crossfader with A/B assignment | ✅ | |
| Cue / pre-listen on outputs 3/4 | ✅ | Needs a 4-output interface |

## Plugins and extensibility

| Feature | Status | Notes |
|---|---|---|
| CLAP and VST3: scan, load, audio, notes | ✅ | VST3 bundles on Linux; macOS bundle loading is written but untested |
| CLAP and VST3: parameters (edit, automate, map, save) | ✅ | Edits made in a plugin's own editor flow back into the project |
| CLAP and VST3: latency reporting | ✅ | Latency changes trigger a rebuild |
| CLAP and VST3: state saving | ✅ | Base64 in the project; survives graph rebuilds, undo, duplicate, save / reopen, export |
| Plugin editor windows | 🟡 | Embedded in a host window on Linux / X11 (CLAP and VST3, with the plugin's timers and fds on the host run loop). CLAP falls back to the plugin's floating window elsewhere. No embedding on Windows or macOS yet |
| Tested against | 🟡 | Only the test plugins in this repo (`examples/`). No commercial or third-party plugin has been tried |
| Sidechain and multi-bus ports | ❌ | |
| Audio Units | ❌ | macOS-only; can't be built or tested on the development machine |
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
2. **Real plugins.** Try common CLAP and VST3 plugins (Surge XT, Vital, the u-he and FabFilter Linux builds) and fix what breaks; then editor embedding on Windows and macOS, Audio Units, sidechain inputs.
3. **Drum Rack pad chains**, Repeat / Legato launch modes, crossfades between regions.
4. **MIDI clock / Link, control-surface scripting.**
5. **Freeze / flatten, time-signature automation, templates.**
6. **More devices:** Echo, spectral tools, physical models.
