//! Reading and writing device parameters on the saved `DeviceSpec`, using
//! the same indices the running devices expose. The UI edits the spec (so
//! the project saves correctly) and sends the same index/value to the
//! engine, so both always agree.

use crate::ParamInfo;
use loopcraft_core::DeviceSpec;

/// Parameter list for a spec. Builds nothing; cheap to call every frame.
pub fn param_infos(spec: &DeviceSpec) -> &'static [ParamInfo] {
    use crate::effects::*;
    match spec {
        DeviceSpec::PolySynth(_) => crate::synth::PARAMS,
        DeviceSpec::Sampler(_) => crate::sampler::PARAMS,
        DeviceSpec::DrumRack(_) => crate::drums::PARAMS,
        // Only the bands that exist (the trailing Output param is index 24).
        DeviceSpec::Eq(p) => &eq::PARAMS[..(p.bands.len().min(8) * 3)],
        DeviceSpec::Compressor(_) => compressor::PARAMS,
        DeviceSpec::Delay(_) => delay::PARAMS,
        DeviceSpec::Reverb(_) => reverb::PARAMS,
        DeviceSpec::Saturator(_) => saturator::PARAMS,
        DeviceSpec::Utility(_) => utility::PARAMS,
        DeviceSpec::AutoFilter(_) => autofilter::PARAMS,
        DeviceSpec::Plugin(_) => &[],
    }
}

/// Current value of parameter `i` (natural units).
pub fn get_param(spec: &DeviceSpec, i: usize) -> f32 {
    match spec {
        DeviceSpec::PolySynth(p) => {
            [p.cutoff_hz, p.resonance, p.filter_env_amount, p.osc_mix, p.gain_db, p.amp_env.attack, p.amp_env.release].get(i).copied().unwrap_or(0.0)
        }
        DeviceSpec::Sampler(p) => [p.gain_db, p.amp_env.attack, p.amp_env.release].get(i).copied().unwrap_or(0.0),
        DeviceSpec::DrumRack(p) => {
            if i == 0 {
                p.gain_db
            } else {
                0.0
            }
        }
        DeviceSpec::Eq(p) => {
            if i == 24 {
                return p.output_db;
            }
            p.bands.get(i / 3).map(|b| [b.freq_hz, b.gain_db, b.q][i % 3]).unwrap_or(0.0)
        }
        DeviceSpec::Compressor(p) => [p.threshold_db, p.ratio, p.attack_ms, p.release_ms, p.makeup_db, p.dry_wet].get(i).copied().unwrap_or(0.0),
        DeviceSpec::Delay(p) => [p.time_beats as f32, p.feedback, p.filter_hz, p.dry_wet].get(i).copied().unwrap_or(0.0),
        DeviceSpec::Reverb(p) => [p.size, p.damping, p.width, p.dry_wet].get(i).copied().unwrap_or(0.0),
        DeviceSpec::Saturator(p) => [p.drive_db, p.output_db, p.dry_wet].get(i).copied().unwrap_or(0.0),
        DeviceSpec::Utility(p) => [p.gain_db, p.width].get(i).copied().unwrap_or(0.0),
        DeviceSpec::AutoFilter(p) => [p.cutoff_hz, p.resonance, p.lfo_amount].get(i).copied().unwrap_or(0.0),
        DeviceSpec::Plugin(_) => 0.0,
    }
}

/// Write parameter `i` back into the spec.
pub fn set_param(spec: &mut DeviceSpec, i: usize, v: f32) {
    match spec {
        DeviceSpec::PolySynth(p) => match i {
            0 => p.cutoff_hz = v,
            1 => p.resonance = v,
            2 => p.filter_env_amount = v,
            3 => p.osc_mix = v,
            4 => p.gain_db = v,
            5 => p.amp_env.attack = v,
            6 => p.amp_env.release = v,
            _ => {}
        },
        DeviceSpec::Sampler(p) => match i {
            0 => p.gain_db = v,
            1 => p.amp_env.attack = v,
            2 => p.amp_env.release = v,
            _ => {}
        },
        DeviceSpec::DrumRack(p) => {
            if i == 0 {
                p.gain_db = v;
            }
        }
        DeviceSpec::Eq(p) => {
            if i == 24 {
                p.output_db = v;
            } else if let Some(b) = p.bands.get_mut(i / 3) {
                match i % 3 {
                    0 => b.freq_hz = v,
                    1 => b.gain_db = v,
                    _ => b.q = v,
                }
            }
        }
        DeviceSpec::Compressor(p) => match i {
            0 => p.threshold_db = v,
            1 => p.ratio = v,
            2 => p.attack_ms = v,
            3 => p.release_ms = v,
            4 => p.makeup_db = v,
            5 => p.dry_wet = v,
            _ => {}
        },
        DeviceSpec::Delay(p) => match i {
            0 => p.time_beats = v as f64,
            1 => p.feedback = v,
            2 => p.filter_hz = v,
            3 => p.dry_wet = v,
            _ => {}
        },
        DeviceSpec::Reverb(p) => match i {
            0 => p.size = v,
            1 => p.damping = v,
            2 => p.width = v,
            3 => p.dry_wet = v,
            _ => {}
        },
        DeviceSpec::Saturator(p) => match i {
            0 => p.drive_db = v,
            1 => p.output_db = v,
            2 => p.dry_wet = v,
            _ => {}
        },
        DeviceSpec::Utility(p) => match i {
            0 => p.gain_db = v,
            1 => p.width = v,
            _ => {}
        },
        DeviceSpec::AutoFilter(p) => match i {
            0 => p.cutoff_hz = v,
            1 => p.resonance = v,
            2 => p.lfo_amount = v,
            _ => {}
        },
        DeviceSpec::Plugin(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Device, NoSamples, build_device};

    /// The spec helpers must expose exactly the same parameter tables as
    /// the running devices, or automation and UI would target the wrong knob.
    #[test]
    fn spec_tables_match_devices() {
        let specs = vec![
            DeviceSpec::PolySynth(Default::default()),
            DeviceSpec::Sampler(Default::default()),
            DeviceSpec::DrumRack(Default::default()),
            DeviceSpec::Compressor(Default::default()),
            DeviceSpec::Delay(Default::default()),
            DeviceSpec::Reverb(Default::default()),
            DeviceSpec::Saturator(Default::default()),
            DeviceSpec::Utility(Default::default()),
            DeviceSpec::AutoFilter(Default::default()),
        ];
        for mut spec in specs {
            let dev = build_device(&spec, 48_000.0, &NoSamples).unwrap();
            let live = match &dev {
                Device::Instrument(i) => i.params(),
                Device::Effect(e) => e.params(),
            };
            assert_eq!(live.len(), param_infos(&spec).len(), "{}", spec.name());
            for (i, info) in param_infos(&spec).iter().enumerate() {
                let mid = (info.min + info.max) / 2.0;
                set_param(&mut spec, i, mid);
                assert!((get_param(&spec, i) - mid).abs() < 1e-3, "{} param {}", spec.name(), info.name);
            }
        }
    }
}
