//! Musical time: beats, tempo, time signatures and launch quantization.
//!
//! The engine counts time in *beats* (quarter notes) as `f64`. Converting to
//! samples always goes through the current tempo, so changing tempo never
//! drifts clips out of sync with each other.

use serde::{Deserialize, Serialize};

/// A position or duration measured in quarter-note beats.
pub type Beats = f64;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimeSignature {
    pub numerator: u32,
    pub denominator: u32,
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self { numerator: 4, denominator: 4 }
    }
}

impl TimeSignature {
    /// Length of one bar in quarter-note beats (6/8 -> 3.0, 4/4 -> 4.0).
    pub fn beats_per_bar(&self) -> Beats {
        self.numerator as f64 * 4.0 / self.denominator.max(1) as f64
    }
}

/// Seconds per beat at `bpm`.
pub fn seconds_per_beat(bpm: f64) -> f64 {
    60.0 / bpm.max(1.0)
}

pub fn beats_to_samples(beats: Beats, bpm: f64, sample_rate: f64) -> f64 {
    beats * seconds_per_beat(bpm) * sample_rate
}

pub fn samples_to_beats(samples: f64, bpm: f64, sample_rate: f64) -> Beats {
    samples / (seconds_per_beat(bpm) * sample_rate)
}

/// Launch quantization for clips and scenes, mirroring the global
/// quantization menu found in clip-launching DAWs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Quantization {
    None,
    EightBars,
    FourBars,
    TwoBars,
    #[default]
    OneBar,
    Half,
    Quarter,
    Eighth,
    Sixteenth,
    ThirtySecond,
}

impl Quantization {
    pub const ALL: [Quantization; 10] = [
        Quantization::None,
        Quantization::EightBars,
        Quantization::FourBars,
        Quantization::TwoBars,
        Quantization::OneBar,
        Quantization::Half,
        Quantization::Quarter,
        Quantization::Eighth,
        Quantization::Sixteenth,
        Quantization::ThirtySecond,
    ];

    /// Grid size in beats, or `None` for immediate launch.
    pub fn grid(&self, sig: TimeSignature) -> Option<Beats> {
        let bar = sig.beats_per_bar();
        Some(match self {
            Quantization::None => return None,
            Quantization::EightBars => bar * 8.0,
            Quantization::FourBars => bar * 4.0,
            Quantization::TwoBars => bar * 2.0,
            Quantization::OneBar => bar,
            Quantization::Half => 2.0,
            Quantization::Quarter => 1.0,
            Quantization::Eighth => 0.5,
            Quantization::Sixteenth => 0.25,
            Quantization::ThirtySecond => 0.125,
        })
    }

    /// The first grid line at or after `now`.
    ///
    /// A tiny epsilon keeps a launch that lands exactly on a boundary from
    /// being pushed a whole grid step later by floating-point noise.
    pub fn next_boundary(&self, now: Beats, sig: TimeSignature) -> Beats {
        match self.grid(sig) {
            None => now,
            Some(g) => {
                let eps = 1e-9;
                let n = ((now - eps) / g).ceil();
                (n * g).max(0.0)
            }
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Quantization::None => "None",
            Quantization::EightBars => "8 Bars",
            Quantization::FourBars => "4 Bars",
            Quantization::TwoBars => "2 Bars",
            Quantization::OneBar => "1 Bar",
            Quantization::Half => "1/2",
            Quantization::Quarter => "1/4",
            Quantization::Eighth => "1/8",
            Quantization::Sixteenth => "1/16",
            Quantization::ThirtySecond => "1/32",
        }
    }
}

/// Convert decibels to linear gain. Anything at or below -70 dB is silence.
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -70.0 { 0.0 } else { 10f32.powf(db / 20.0) }
}

pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 1e-6 { -70.0 } else { 20.0 * gain.log10() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_lengths() {
        assert_eq!(TimeSignature::default().beats_per_bar(), 4.0);
        assert_eq!(TimeSignature { numerator: 6, denominator: 8 }.beats_per_bar(), 3.0);
        assert_eq!(TimeSignature { numerator: 7, denominator: 4 }.beats_per_bar(), 7.0);
    }

    #[test]
    fn quantize_boundaries() {
        let sig = TimeSignature::default();
        let q = Quantization::OneBar;
        assert_eq!(q.next_boundary(0.0, sig), 0.0);
        assert_eq!(q.next_boundary(0.1, sig), 4.0);
        assert_eq!(q.next_boundary(4.0, sig), 4.0);
        assert_eq!(q.next_boundary(5.5, sig), 8.0);
        assert_eq!(Quantization::None.next_boundary(5.5, sig), 5.5);
        assert_eq!(Quantization::Sixteenth.next_boundary(1.01, sig), 1.25);
    }

    #[test]
    fn sample_conversion_roundtrip() {
        let s = beats_to_samples(3.0, 120.0, 48_000.0);
        assert!((s - 72_000.0).abs() < 1e-6);
        assert!((samples_to_beats(s, 120.0, 48_000.0) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn decibels() {
        assert!((db_to_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_gain(-6.0) - 0.501).abs() < 1e-3);
        assert_eq!(db_to_gain(-80.0), 0.0);
    }
}
