# Contributing to LoopCraft

Thanks for helping build an open-source DAW.

## Ground rules

1. **Clean room.** Read [docs/CLEAN_ROOM.md](docs/CLEAN_ROOM.md). No Ableton code, assets or reverse-engineered internals, ever.
2. **Real-time safety.** Code that runs in `Engine::process` (engine rendering, every `Instrument::render` / `Effect::process`) must not allocate, lock, free, or do I/O. Preallocate in constructors.
3. **Tests that listen.** New engine or DSP behaviour should come with a test that renders audio and checks it (onset positions, frequencies, levels). See `crates/loopcraft-engine/src/tests.rs` for the pattern.
4. **License.** Contributions are dual-licensed MIT OR Apache-2.0.

## Getting started

```sh
cargo test                       # library crates (fast)
cargo run --release -p loopcraft-app
cargo clippy --workspace --all-targets
cargo fmt --all
```

On Linux, install `libasound2-dev` (ALSA headers) first.

## Adding a built-in effect

1. Add a params struct and a `DeviceSpec` variant in `crates/loopcraft-core/src/model.rs`.
2. Implement `Effect` in `crates/loopcraft-dsp/src/effects/`, with a `pub(crate) const PARAMS` table.
3. Wire it into `build_device` (`dsp/src/lib.rs`) and `get_param` / `set_param` / `param_infos` (`dsp/src/specs.rs`). The `spec_tables_match_devices` test will tell you if they disagree.
4. Add it to the browser list in `crates/loopcraft-app/src/ui/browser.rs`.

## Where to help

The [roadmap](docs/ROADMAP.md) lists every missing feature with its phase. Phase 1 items are the most useful right now.
