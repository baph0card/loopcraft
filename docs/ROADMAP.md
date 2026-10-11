# Roadmap: the road to Live parity

This is the honest map of where LoopCraft stands against Ableton Live 12 Suite's feature set, area by area. It is the to-do list for contributors.

**Legend:** ✅ done · 🟡 partial (see note) · ❌ not started

Live is roughly 25 years of work by a large team. v0.1 deliberately builds the parts everything else depends on (a real-time-safe engine, sample-accurate launching, the project model, device and plugin interfaces) so later features slot in rather than force rewrites.

---

## Session view

| Feature | Status | Notes |
|---|---|---|
| Clip slot grid, launch / stop | ✅ | |
| Scenes, scene launch | ✅ | |
| Scene tempo / time signature | ✅ | Applied on the scene's launch boundary |
| Global launch quantization | ✅ | None … 8 bars, 1/32 |
| Per-clip launch quantization | ✅ | |
| Launch modes Trigger / Gate / Toggle | ✅ | Gate release wired to engine; UI uses Trigger behaviour on click |
| Launch mode Repeat, Legato | ❌ | |
| Follow actions (clip and scene) | ❌ | Phase 1 |
| Stop buttons, Stop All | ✅ | |
| Back to Arrangement | ✅ | |
| MIDI recording into slots | 🟡 | New clips, fixed 4-bar length; no overdub yet |
| Audio recording | ❌ | Phase 1 (needs audio input) |
| Capture MIDI | ❌ | |
| Session → Arrangement recording | 🟡 | Manual "Copy to Arrangement"; no live arrangement record |
| Clip and scene colors, renaming | 🟡 | Renaming yes; color picker no |

## Arrangement view

| Feature | Status | Notes |
|---|---|---|
| Timeline playback (MIDI + audio) | ✅ | |
| Looping regions | ✅ | Clip loops inside a longer region |
| Loop brace | ✅ | Set from the ruler's context menu |
| Move clips | ✅ | Snap to beat / bar |
| Split, consolidate, trim, time selection editing | ❌ | Phase 2 |
| Automation playback | ✅ | Volume, pan, sends, any built-in device parameter |
| Automation drawing in UI | ❌ | Phase 1 |
| Tempo / time signature automation | ❌ | |
| Locators, markers | ❌ | |
| Take lanes / comping | ❌ | |
| Arrangement recording | ❌ | |

## Audio

| Feature | Status | Notes |
|---|---|---|
| WAV import, auto-resample | ✅ | Any rate, 8–32-bit int / float |
| AIFF, FLAC, MP3, OGG import | ❌ | Phase 1 (symphonia) |
| Warp: Repitch | ✅ | |
| Warp: Stretch (WSOLA) | ✅ | Comparable to Live's Texture/Tones territory |
| Warp: Beats (transient-aware) | ❌ | Phase 2 |
| Warp: Complex / Complex Pro (phase vocoder) | ❌ | Phase 2 |
| Warp markers, transient detection, tempo detection | 🟡 | Loop-length tempo guess on import only |
| Clip gain, start offset | ✅ | |
| Clip transpose | ❌ | Stretcher already supports a pitch ratio; needs UI + model field |
| Clip fades, crossfades | 🟡 | 2 ms declick on launch only |
| Reverse, audio-to-MIDI, stem separation | ❌ | Later |
| Audio input, monitoring, recording | ❌ | Phase 1 |

## MIDI

| Feature | Status | Notes |
|---|---|---|
| Piano roll: add / move / resize / delete | ✅ | |
| Velocity editing | ❌ | Phase 1 |
| Note probability | 🟡 | Engine + file format; no UI |
| Velocity range, chance, conditions | ❌ | |
| MIDI transformations / generators | ❌ | Phase 2 |
| Scale awareness, fold to scale | ❌ | |
| MPE | ❌ | Later |
| MIDI file import / export | ✅ | CLI; app import pending |
| Hardware MIDI input | ❌ | Phase 1 (midir) |
| Computer keyboard as piano | ✅ | |
| Groove pool, quantize | ❌ | Phase 2 |
| MIDI clock, Ableton Link | ❌ | Phase 1; Link is GPL-2, so behind an opt-in feature |

## Instruments

| Device | Status | Notes |
|---|---|---|
| Poly Synth (subtractive) | ✅ | 2 osc + sub, SVF, filter env, glide |
| Sampler | 🟡 | Single sample, chromatic; no zones, slicing, modulation |
| Drum Rack | 🟡 | Synth or sample pads, choke; no per-pad chains/sends |
| Wavetable, FM (Operator-style), Drift-style | ❌ | Phase 2 |
| Physical modelling (Collision / Tension / Electric) | ❌ | Later |
| Instrument / Drum / Audio / MIDI Racks with macros | ❌ | Phase 2; the device trait was designed with this in mind |

## Audio effects

| Device | Status |
|---|---|
| EQ (8-band parametric) | ✅ |
| Compressor (with sidechain) | ✅ |
| Delay (synced, ping-pong) | ✅ |
| Reverb (algorithmic) | ✅ |
| Saturator | ✅ |
| Auto Filter (synced LFO) | ✅ |
| Utility | ✅ |
| Limiter, Glue-style bus compressor, Gate, Multiband | ❌ Phase 1 |
| Chorus, Flanger, Phaser, Echo, Auto Pan | ❌ Phase 1 |
| Beat Repeat, Grain Delay, Redux, Vocoder, Corpus, Spectral | ❌ Phase 2 |
| Amp / Cabinet / Pedal, Drum Buss | ❌ Phase 2 |
| Convolution reverb | ❌ Phase 2 |
| MIDI effects (Arpeggiator, Chord, Scale, Random, Note Length) | ❌ Phase 1 |

## Mixing and routing

| Feature | Status | Notes |
|---|---|---|
| Volume, pan, mute, solo | ✅ | |
| Sends / returns | 🟡 | Post-fader only |
| Master chain | ✅ | |
| Meters | ✅ | Peak with decay |
| Group tracks | ❌ | Phase 1 |
| I/O routing, resampling, track-to-track | ❌ | Phase 1 |
| Plugin delay compensation | ❌ | Phase 2 |
| Crossfader, cue / pre-listen | ❌ | Phase 3 |

## Plugins and extensibility

| Feature | Status | Notes |
|---|---|---|
| CLAP: scan, load, audio, notes | ✅ | |
| CLAP: parameters, automation, state save | ❌ | Phase 1 |
| CLAP: plugin GUIs | ❌ | Phase 1 |
| VST3 | ❌ | Phase 3; the SDK is GPLv3/proprietary, so it will be an opt-in GPL build feature |
| Audio Units (macOS) | ❌ | Phase 3 |
| Scripted devices (a Max for Live alternative) | ❌ | Phase 3; plan is WASM-sandboxed devices |

## Control and performance

| Feature | Status |
|---|---|
| MIDI mapping, key mapping | ❌ Phase 3 |
| Control surface scripts, Push-style controllers | ❌ Phase 3 |
| Tempo follower, tap tempo | ❌ Phase 3 |

## Files and workflow

| Feature | Status | Notes |
|---|---|---|
| Project save / open (`.loopcraft`) | ✅ | Versioned JSON |
| Undo / redo | ✅ | Snapshot-based, 200 steps |
| WAV export (arrangement or scenes) | ✅ | 16/24-bit dithered, 32-bit float |
| Stem export, FLAC/MP3 export | ❌ | Phase 2 |
| Browser: devices, plugins, project samples | ✅ | |
| Browser search, collections, preview | ❌ | Phase 1 |
| Device presets | ❌ | Phase 1 |
| Templates | ❌ | |
| `.als` import | ❌ | Later; clean-room only, from documented behaviour of user files |
| Audio device settings panel | ❌ | Phase 1 (uses the system default today) |

---

## Phases

**Phase 1: "Playable" (v0.2).** Audio input and recording; hardware MIDI input; velocity and automation editing; follow actions; group tracks and routing; limiter, glue compressor, modulation effects and MIDI effects; CLAP parameters, state and GUIs; browser search and presets; Link and MIDI clock as opt-in features; audio settings.

**Phase 2: "Producer" (v0.3).** Racks with macros; wavetable, FM and multisample instruments; Beats and Complex warp modes with warp markers and transient detection; arrangement editing (split, consolidate, time selection); delay compensation; groove; stem and compressed-format export.

**Phase 3: "Performer" (v0.4).** MIDI mapping and control surfaces; crossfader and cueing; tempo follower; WASM scripted devices; VST3 and AU behind opt-in features.

**Later.** MPE, spectral tools, stem separation, `.als` import, collaboration.
