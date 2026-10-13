//! Built-in audio effects.

pub mod autofilter;
pub mod character;
pub mod compressor;
pub mod convolution;
pub mod creative;
pub mod delay;
pub mod dynamics;
pub mod eq;
pub mod modulation;
pub mod reverb;
pub mod saturator;
pub mod utility;

/// Largest block any effect must handle in one call. The engine splits
/// larger host buffers, so effects can preallocate scratch to this size.
pub const MAX_BLOCK: usize = 4096;

#[inline]
pub(crate) fn mix(dry: f32, wet: f32, amount: f32) -> f32 {
    dry + (wet - dry) * amount
}
