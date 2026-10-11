//! LoopCraft IO: audio files, MIDI files, offline rendering and the demo song.

pub mod audio;
pub mod demo;
pub mod midi;
pub mod render;

pub use audio::{BitDepth, FileLoader, read_wav, write_wav};
pub use render::{RenderMode, RenderOptions, Rendered, render};

#[cfg(test)]
mod tests {
    use super::*;
    use loopcraft_engine::NoPlugins;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("loopcraft-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn wav_roundtrip() {
        let d = tmp("wav");
        let l: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let r: Vec<f32> = l.iter().map(|x| -x).collect();
        let p = d.join("t.wav");
        write_wav(&p, &l, &r, 44_100, BitDepth::Int24).unwrap();
        let back = read_wav(&p).unwrap();
        assert_eq!(back.sample_rate, 44_100.0);
        assert_eq!(back.channels.len(), 2);
        assert!((back.channels[0][100] - l[100]).abs() < 1e-4);
        assert!((back.channels[1][100] - r[100]).abs() < 1e-4);
    }

    #[test]
    fn demo_renders_scenes_and_arrangement() {
        let d = tmp("demo");
        let path = demo::write_demo(&d, 44_100).unwrap();
        let project = loopcraft_core::Project::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();

        let opts = RenderOptions { sample_rate: 44_100, mode: RenderMode::Scenes(vec![(0, 8.0), (2, 8.0), (3, 8.0)]), tail_seconds: 1.0 };
        let out = render(&project, &d, &opts, &NoPlugins).unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert!(out.peak() > 0.1, "demo is silent");
        assert!(out.peak() < 1.5, "demo clips badly: {}", out.peak());
        assert!(out.left.iter().all(|s| s.is_finite()));

        let arr = RenderOptions { sample_rate: 22_050, mode: RenderMode::Arrangement { length: Some(40.0) }, tail_seconds: 0.0 };
        let out = render(&project, &d, &arr, &NoPlugins).unwrap();
        assert!(out.peak() > 0.05);
    }
}
