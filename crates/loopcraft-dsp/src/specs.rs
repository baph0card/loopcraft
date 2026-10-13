//! Reading and writing device parameters on the saved `DeviceSpec`.
//!
//! Every [`ParamInfo`] carries a JSON-pointer `path` into its device's
//! saved parameters, so the UI and the running device always agree on
//! what parameter N means, with one table per device as the single source
//! of truth. These helpers are for the UI thread (they go through serde).

use crate::{NoSamples, ParamInfo, build_device, device_params};
use loopcraft_core::DeviceSpec;
use serde_json::Value;

/// Parameter table for a saved device. Builds the device once to read its
/// table, which is cheap for everything except very large racks; callers
/// that redraw every frame should cache it.
pub fn param_infos(spec: &DeviceSpec) -> &'static [ParamInfo] {
    match spec {
        DeviceSpec::Plugin(_) => &[],
        DeviceSpec::InstrumentRack(r) | DeviceSpec::AudioRack(r) => &crate::rack::MACRO_PARAMS[..r.macros.len().min(8)],
        DeviceSpec::ConvolutionReverb(_) => crate::effects::convolution::PARAMS,
        // Building a Drum Rack or Multi Sampler would touch sample loading; use the static tables.
        DeviceSpec::DrumRack(_) => crate::drums::PARAMS,
        DeviceSpec::MultiSampler(_) => crate::multisampler::PARAMS,
        DeviceSpec::Sampler(_) => crate::sampler::PARAMS,
        other => build_device(other, 48_000.0, &NoSamples).as_ref().map(device_params).unwrap_or(&[]),
    }
}

fn inner(v: &mut Value) -> Option<&mut Value> {
    v.as_object_mut()?.values_mut().next()
}

/// Current value of parameter `i` (natural units), or `None` if the device
/// has no such parameter (e.g. an EQ band that doesn't exist).
pub fn get_param(spec: &DeviceSpec, i: usize) -> Option<f32> {
    let info = param_infos(spec).get(i)?;
    let mut v = serde_json::to_value(spec).ok()?;
    let p = inner(&mut v)?.pointer(&format!("/{}", info.path))?;
    p.as_f64().map(|x| x as f32)
}

/// Write parameter `i` back into the saved spec. Integer fields are rounded.
pub fn set_param(spec: &mut DeviceSpec, i: usize, value: f32) -> bool {
    let Some(info) = param_infos(spec).get(i).copied() else { return false };
    let Ok(mut v) = serde_json::to_value(&*spec) else { return false };
    let Some(slot) = inner(&mut v).and_then(|x| x.pointer_mut(&format!("/{}", info.path))) else { return false };
    let value = value.clamp(info.min.min(info.max), info.max.max(info.min));
    *slot = if slot.is_u64() {
        Value::from(value.round().max(0.0) as u64)
    } else if slot.is_i64() {
        Value::from(value.round() as i64)
    } else {
        Value::from(value as f64)
    };
    match serde_json::from_value::<DeviceSpec>(v) {
        Ok(new) => {
            *spec = new;
            true
        }
        Err(_) => false,
    }
}

/// True if every parameter of every built-in device resolves in its spec.
pub fn check_catalog() -> Result<(), String> {
    for spec in DeviceSpec::catalog() {
        let n = param_infos(&spec).len();
        for i in 0..n {
            let info = param_infos(&spec)[i];
            // EQ tables list 8 bands; default EQs have 4, so skip missing bands.
            if matches!(spec, DeviceSpec::Eq(_)) && info.path.starts_with("bands/") && get_param(&spec, i).is_none() {
                continue;
            }
            if get_param(&spec, i).is_none() {
                return Err(format!("{} param {} ('{}') has no value at '{}'", spec.name(), i, info.name, info.path));
            }
        }
        // Built devices must expose the same table as the spec helpers.
        if let Some(d) = build_device(&spec, 48_000.0, &NoSamples) {
            let live = device_params(&d);
            if live.len() != n {
                return Err(format!("{}: running device has {} params, spec table has {n}", spec.name(), live.len()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcraft_core::*;

    #[test]
    fn every_param_path_resolves() {
        check_catalog().unwrap();
    }

    #[test]
    fn set_and_get_roundtrip() {
        for mut spec in DeviceSpec::catalog() {
            for i in 0..param_infos(&spec).len() {
                let info = param_infos(&spec)[i];
                if get_param(&spec, i).is_none() {
                    continue;
                }
                let mid = (info.min + info.max) / 2.0;
                assert!(set_param(&mut spec, i, mid), "{} {}", spec.name(), info.name);
                let got = get_param(&spec, i).unwrap();
                assert!((got - mid).abs() <= 0.5 + 1e-3, "{} {}: {got} vs {mid}", spec.name(), info.name);
            }
        }
    }

    #[test]
    fn integer_fields_stay_integers() {
        let mut s = DeviceSpec::Scale(ScaleParams::default());
        assert!(set_param(&mut s, 0, 4.4));
        let DeviceSpec::Scale(p) = s else { panic!() };
        assert_eq!(p.root, 4);
    }
}
