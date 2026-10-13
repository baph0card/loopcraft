//! Keeping the project in step with running third-party plugins: their
//! main-thread housekeeping, parameter changes made in their own editors,
//! and their internal state before saving.

use super::App;
use loopcraft_dsp::plugin_params;

impl App {
    /// Once per frame: run plugin timers / callbacks / editor windows, and
    /// copy parameter values the plugins changed themselves into the project
    /// so the device view, automation and saving see them.
    pub(crate) fn poll_plugins(&mut self) {
        if self.plugins.idle() {
            // A plugin's latency or ports changed: rebuild once the pointer
            // is up (the rebuild keeps the running instances).
            self.rebuild_pending = true;
        }
        let changes = self.plugins.take_param_changes();
        if changes.is_empty() {
            return;
        }
        self.apply_plugin_param_changes(&changes);
    }

    pub(crate) fn apply_plugin_param_changes(&mut self, changes: &[(u64, usize, f32)]) {
        let mut changed = false;
        self.project.for_each_plugin_mut(&mut |p| {
            for &(inst, i, v) in changes {
                if p.instance != inst {
                    continue;
                }
                let defaults = plugin_params::get(&p.id).map(|(_, d)| d).unwrap_or_default();
                let n = defaults.len().max(i + 1);
                if p.params.len() < n {
                    let have = p.params.len();
                    p.params.extend((have..n).map(|k| defaults.get(k).copied().unwrap_or(0.0)));
                }
                if p.params[i] != v {
                    p.params[i] = v;
                    changed = true;
                }
            }
        });
        if changed {
            self.dirty = true;
        }
    }

    /// Write every running plugin's internal state into the project.
    pub(crate) fn sync_plugin_states(&mut self) {
        self.plugins.save_states(&mut self.project);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loopcraft_core::{DeviceSpec, PluginFormat, PluginRef, Project, Track};

    fn plugin(id: &str) -> DeviceSpec {
        DeviceSpec::Plugin(PluginRef {
            format: PluginFormat::Clap,
            path: "/nonexistent.clap".into(),
            id: id.into(),
            name: "Test".into(),
            is_instrument: false,
            state: None,
            params: Vec::new(),
            instance: 0,
        })
    }

    #[test]
    fn plugin_instances_are_unique_and_editor_edits_reach_the_project() {
        plugin_params::register(
            "app.test.p",
            vec![
                plugin_params::PluginParam { name: "A".into(), min: 0.0, max: 1.0, default: 0.25 },
                plugin_params::PluginParam { name: "B".into(), min: 0.0, max: 1.0, default: 0.5 },
            ],
        );
        let mut p = Project::default();
        p.add_track(Track::audio("Audio"));
        p.tracks[0].effects.push(plugin("app.test.p"));
        let mut app = App::headless(p);
        let id = match &app.project.tracks[0].effects[0] {
            DeviceSpec::Plugin(r) => r.instance,
            _ => unreachable!(),
        };
        assert_ne!(id, 0, "instance ids are assigned in the app's own project");
        app.duplicate_track(0);
        let ids: Vec<u64> = app
            .project
            .tracks
            .iter()
            .map(|t| match &t.effects[0] {
                DeviceSpec::Plugin(r) => r.instance,
                _ => 0,
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1], "a duplicated plugin is a separate instance");
        app.apply_plugin_param_changes(&[(id, 1, 0.9)]);
        match &app.project.tracks[0].effects[0] {
            DeviceSpec::Plugin(r) => assert_eq!(r.params, vec![0.25, 0.9]),
            _ => unreachable!(),
        }
        match &app.project.tracks[1].effects[0] {
            DeviceSpec::Plugin(r) => assert!(r.params.is_empty(), "the copy is untouched"),
            _ => unreachable!(),
        }
    }
}
