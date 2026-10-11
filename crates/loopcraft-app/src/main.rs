//! LoopCraft desktop app.
//!
//! Usage: `loopcraft-app [project.loopcraft]`. With no argument it opens
//! the bundled demo song (written to a temp folder on first run).

mod app;
mod audio;
mod ui;

use eframe::egui;
use loopcraft_core::Project;
use std::path::PathBuf;

fn main() -> eframe::Result {
    let arg = std::env::args().nth(1).map(PathBuf::from);
    let (project, path) = match &arg {
        Some(p) => match std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|s| Project::from_json(&s).map_err(|e| e.to_string())) {
            Ok(proj) => (proj, Some(p.clone())),
            Err(e) => {
                eprintln!("couldn't open {}: {e}", p.display());
                std::process::exit(1);
            }
        },
        None => {
            let dir = std::env::temp_dir().join("loopcraft-demo");
            match loopcraft_io::demo::write_demo(&dir, 48_000) {
                Ok(p) => (loopcraft_io::demo::demo_project(), Some(p)),
                Err(_) => (Project::default(), None),
            }
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1360.0, 860.0]).with_min_inner_size([900.0, 560.0]).with_title("LoopCraft"),
        ..Default::default()
    };
    eframe::run_native(
        "LoopCraft",
        options,
        Box::new(move |cc| {
            ui::setup_style(&cc.egui_ctx);
            Ok(Box::new(app::App::new(project, path)))
        }),
    )
}
