//! Parameter tables of third-party plugins.
//!
//! Built-in devices have static parameter tables; plugins only reveal
//! theirs once loaded. The host registers each plugin's table here (once
//! per plugin id, for the life of the process) so the UI and the saved
//! `DeviceSpec` can address plugin parameters by index exactly like
//! built-in ones.

use crate::ParamInfo;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

type Entry = (&'static [ParamInfo], Vec<f32>);

fn reg() -> &'static Mutex<HashMap<String, Entry>> {
    static R: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// One parameter as the plugin describes it.
pub struct PluginParam {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

/// Register (or look up) a plugin's table. The first registration for an
/// id wins; the strings are leaked once so the table can be `'static`.
pub fn register(id: &str, params: Vec<PluginParam>) -> &'static [ParamInfo] {
    let Ok(mut r) = reg().lock() else { return &[] };
    if let Some((infos, _)) = r.get(id)
        && infos.len() == params.len()
    {
        return infos;
    }
    let defaults = params.iter().map(|p| p.default).collect();
    let infos: Vec<ParamInfo> = params.into_iter().map(|p| ParamInfo::new(Box::leak(p.name.into_boxed_str()), p.min, p.max, "")).collect();
    let infos: &'static [ParamInfo] = Box::leak(infos.into_boxed_slice());
    r.insert(id.to_string(), (infos, defaults));
    infos
}

/// The table and default values, if the plugin has been loaded this session.
pub fn get(id: &str) -> Option<(&'static [ParamInfo], Vec<f32>)> {
    reg().lock().ok()?.get(id).map(|(i, d)| (*i, d.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcraft_core::{DeviceSpec, PluginFormat, PluginRef};

    #[test]
    fn spec_params_follow_the_registry() {
        register("test.reg", vec![PluginParam { name: "Cutoff".into(), min: 0.0, max: 10.0, default: 4.0 }]);
        let mut spec = DeviceSpec::Plugin(PluginRef {
            format: PluginFormat::Clap,
            path: String::new(),
            id: "test.reg".into(),
            name: "x".into(),
            is_instrument: false,
            state: None,
            params: Vec::new(),
            instance: 0,
        });
        assert_eq!(crate::specs::param_infos(&spec)[0].name, "Cutoff");
        assert_eq!(crate::specs::get_param(&spec, 0), Some(4.0));
        assert!(crate::specs::set_param(&mut spec, 0, 20.0));
        assert_eq!(crate::specs::get_param(&spec, 0), Some(10.0), "clamped");
    }
}
