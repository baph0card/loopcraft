//! Offline (faster-than-real-time) rendering, i.e. "Export Audio".

use crate::audio::FileLoader;
use anyhow::Result;
use loopcraft_core::{Beats, Project};
use loopcraft_engine::{Command, Core, Engine, PluginFactory, create};
use std::path::Path;

#[derive(Clone, Debug)]
pub enum RenderMode {
    /// Play the Arrangement from beat 0. `None` renders to the last clip.
    Arrangement { length: Option<Beats> },
    /// Launch scenes in sequence: (scene index, length in beats).
    Scenes(Vec<(usize, Beats)>),
}

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub sample_rate: u32,
    pub mode: RenderMode,
    /// Extra seconds after the end so reverb and delay tails ring out.
    pub tail_seconds: f32,
}

pub struct Rendered {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub sample_rate: u32,
    /// Missing files and other non-fatal problems.
    pub warnings: Vec<String>,
}

impl Rendered {
    pub fn peak(&self) -> f32 {
        loopcraft_dsp::peak(&self.left).max(loopcraft_dsp::peak(&self.right))
    }
    pub fn seconds(&self) -> f32 {
        self.left.len() as f32 / self.sample_rate as f32
    }
}

const BLOCK: usize = 512;

fn run_until(engine: &mut Engine, out: &mut Rendered, beat: Beats) {
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    // Render in small steps so commands sent afterwards land before `beat`.
    while engine.core().beat() < beat {
        let remaining = ((beat - engine.core().beat()) / (engine.core().tempo() / 60.0) * out.sample_rate as f64).ceil() as usize;
        let n = remaining.clamp(1, BLOCK);
        engine.process(&mut l[..n], &mut r[..n]);
        out.left.extend_from_slice(&l[..n]);
        out.right.extend_from_slice(&r[..n]);
    }
}

fn run_frames(engine: &mut Engine, out: &mut Rendered, frames: usize) {
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut left = frames;
    while left > 0 {
        let n = left.min(BLOCK);
        engine.process(&mut l[..n], &mut r[..n]);
        out.left.extend_from_slice(&l[..n]);
        out.right.extend_from_slice(&r[..n]);
        left -= n;
    }
}

/// Render a project. Relative file paths resolve against `base_dir`.
pub fn render(project: &Project, base_dir: &Path, opts: &RenderOptions, plugins: &dyn PluginFactory) -> Result<Rendered> {
    let sr = opts.sample_rate as f32;
    let loader = FileLoader::new(base_dir, sr);
    let core = Core::from_project(project, sr, &loader, plugins);
    let (mut engine, mut handle) = create(core);
    let mut out = Rendered { left: Vec::new(), right: Vec::new(), sample_rate: opts.sample_rate, warnings: loader.errors() };

    match &opts.mode {
        RenderMode::Arrangement { length } => {
            let end = length.unwrap_or_else(|| project.arrangement_length());
            handle.send(Command::Play).ok();
            run_until(&mut engine, &mut out, end);
        }
        RenderMode::Scenes(seq) => {
            let mut at = 0.0;
            for (i, (scene, beats)) in seq.iter().enumerate() {
                if i == 0 {
                    handle.send(Command::LaunchScene { scene: *scene }).ok();
                } else {
                    // Queue the scene shortly before its bar line, like a
                    // performer would; quantization makes it land exactly.
                    run_until(&mut engine, &mut out, at - 0.5);
                    handle.send(Command::LaunchScene { scene: *scene }).ok();
                }
                at += beats;
            }
            run_until(&mut engine, &mut out, at);
        }
    }
    handle.send(Command::StopAllClips).ok();
    handle.send(Command::Stop).ok();
    run_frames(&mut engine, &mut out, (opts.tail_seconds.max(0.0) * sr) as usize);
    handle.poll();
    Ok(out)
}
