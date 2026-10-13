//! LoopCraft engine: transport, Session-view clip launching, Arrangement
//! playback, automation and the mixer, all real-time safe.
//!
//! ```no_run
//! use loopcraft_core::Project;
//! use loopcraft_dsp::NoSamples;
//! use loopcraft_engine::{Command, Core, NoPlugins, create};
//!
//! let project = Project::default();
//! let core = Core::from_project(&project, 48_000.0, &NoSamples, &NoPlugins);
//! let (mut engine, mut handle) = create(core);
//! handle.send(Command::LaunchScene { scene: 0 }).ok();
//! let (mut l, mut r) = (vec![0.0; 512], vec![0.0; 512]);
//! engine.process(&mut l, &mut r); // call from the audio callback
//! ```

pub mod clip;
pub mod command;
pub mod engine;
pub mod state;

pub use clip::RtClip;
pub use command::{AudioRecord, Command, DeviceRef, EngineEvent, RecStart, RecTarget, RecordBuffer, TrackRef};
pub use engine::{CHUNK, Core, Engine, EngineHandle, MAX_HOST_FRAMES, MAX_INPUTS, NoPlugins, PluginFactory, Polled, create, record_ring};
pub use state::EngineState;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_phase1;
