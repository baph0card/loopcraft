//! LoopCraft core: musical time and the project model.
//!
//! This crate has no audio or UI dependencies, so tools (renderers,
//! converters, tests) can read and write projects without pulling in the
//! engine.

pub mod devices;
pub mod model;
pub mod time;

pub use devices::*;
pub use model::*;
pub use time::*;
