//! `loopcraft` command-line tool.

use anyhow::{Context, Result, anyhow, bail};
use loopcraft_core::*;
use loopcraft_io::{BitDepth, RenderMode, RenderOptions, demo, midi, render, render_stems, write_audio};
use loopcraft_plugin::{HostFactory, default_clap_paths, scan_clap};
use std::path::{Path, PathBuf};

const USAGE: &str = "\
LoopCraft — open-source clip-launching DAW (command-line tools)

USAGE:
  loopcraft demo <dir>                       Write the demo song and its audio into <dir>
  loopcraft render <project> -o <out.wav>    Export audio
        [--arrangement] [--beats N]          Render the Arrangement (default if it has clips)
        [--scenes 0:16,2:16,...]             Launch scenes in order, each for N beats
        [--sr 48000] [--tail 2.0] [--float] [--16]
        [--stems <dir>]                      Also write one file per track
        Output format follows the extension: .wav, .flac, .aif/.aiff
  loopcraft info <project>                   Show tracks, scenes and clips
  loopcraft import-midi <in.mid> <out.loopcraft>
  loopcraft export-midi <project> <track> <slot> <out.mid>
  loopcraft plugins [dir...]                 Scan for CLAP plugins
  loopcraft devices                          List built-in instruments and effects
";

fn main() {
    if let Err(e) = run(std::env::args().skip(1).collect()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn load(path: &Path) -> Result<Project> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Project::from_json(&s)?)
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn run(args: Vec<String>) -> Result<()> {
    let Some(cmd) = args.first() else {
        print!("{USAGE}");
        return Ok(());
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "demo" => {
            let dir = PathBuf::from(rest.first().ok_or_else(|| anyhow!("missing <dir>"))?);
            let p = demo::write_demo(&dir, 48_000)?;
            println!("wrote {}", p.display());
            println!("try:  loopcraft render {} -o night-drive.wav --scenes 0:16,1:16,2:32,3:16", p.display());
        }
        "render" => cmd_render(rest)?,
        "info" => cmd_info(&load(Path::new(rest.first().ok_or_else(|| anyhow!("missing <project>"))?))?)?,
        "import-midi" => {
            let (input, output) = (rest.first().ok_or_else(|| anyhow!("missing <in.mid>"))?, rest.get(1).ok_or_else(|| anyhow!("missing <out>"))?);
            let song = midi::import_midi(&std::fs::read(input)?)?;
            let mut p = Project { name: Path::new(input).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), ..Default::default() };
            if let Some(t) = song.tempo {
                p.tempo = t;
            }
            p.add_scene("1");
            for t in song.tracks {
                let is_drums = t.clip_is_drums();
                let inst = if is_drums { DeviceSpec::DrumRack(Default::default()) } else { DeviceSpec::PolySynth(Default::default()) };
                let i = p.add_track(Track::midi(t.name.clone(), inst));
                p.tracks[i].arrangement.push(ArrangementClip::new(0.0, t.clip.clone()));
                p.tracks[i].slots[0] = Some(t.clip);
            }
            std::fs::write(output, p.to_json())?;
            println!("imported {} track(s) at {:.1} bpm -> {output}", p.tracks.len(), p.tempo);
        }
        "export-midi" => {
            let p = load(Path::new(rest.first().ok_or_else(|| anyhow!("missing <project>"))?))?;
            let track: usize = rest.get(1).ok_or_else(|| anyhow!("missing <track>"))?.parse()?;
            let slot: usize = rest.get(2).ok_or_else(|| anyhow!("missing <slot>"))?.parse()?;
            let out = rest.get(3).ok_or_else(|| anyhow!("missing <out.mid>"))?;
            let clip =
                p.tracks.get(track).and_then(|t| t.slots.get(slot)).and_then(|c| c.as_ref()).ok_or_else(|| anyhow!("no clip at track {track}, slot {slot}"))?;
            let ClipContent::Midi { notes } = &clip.content else { bail!("that clip is audio, not MIDI") };
            std::fs::write(out, midi::export_midi(notes, p.tempo, &clip.name)?)?;
            println!("wrote {} notes to {out}", notes.len());
        }
        "plugins" => {
            let dirs: Vec<PathBuf> = if rest.is_empty() { default_clap_paths() } else { rest.iter().map(PathBuf::from).collect() };
            let (found, errors) = scan_clap(&dirs);
            for d in &dirs {
                println!("searched {}", d.display());
            }
            for (path, plugins) in &found {
                println!("\n{}", path.display());
                for p in plugins {
                    println!("  {:<40} {} {} [{}]", p.id, p.name, p.version, if p.is_instrument { "instrument" } else { "effect" });
                }
            }
            for e in errors {
                eprintln!("skipped {e}");
            }
            if found.is_empty() {
                println!("\nno CLAP plugins found");
            }
        }
        "devices" => {
            for kind in [DeviceKind::Instrument, DeviceKind::MidiEffect, DeviceKind::AudioEffect] {
                let label = match kind {
                    DeviceKind::Instrument => "Instruments",
                    DeviceKind::MidiEffect => "MIDI effects",
                    DeviceKind::AudioEffect => "Audio effects",
                };
                let names: Vec<String> = DeviceSpec::catalog().iter().filter(|d| d.kind() == kind).map(|d| d.name().to_string()).collect();
                println!("{label} ({}): {}", names.len(), names.join(", "));
            }
        }
        "-h" | "--help" | "help" => print!("{USAGE}"),
        other => bail!("unknown command `{other}`\n\n{USAGE}"),
    }
    Ok(())
}

trait DrumGuess {
    fn clip_is_drums(&self) -> bool;
}

impl DrumGuess for midi::ImportedTrack {
    /// Heuristic: named like a drum track, or every note in the GM drum range.
    fn clip_is_drums(&self) -> bool {
        let n = self.name.to_lowercase();
        if ["drum", "kit", "perc", "beat"].iter().any(|w| n.contains(w)) {
            return true;
        }
        matches!(&self.clip.content, ClipContent::Midi { notes } if !notes.is_empty() && notes.iter().all(|x| (35..=59).contains(&x.pitch)) && notes.iter().any(|x| x.pitch == 36 || x.pitch == 35))
    }
}

fn parse_scenes(s: &str) -> Result<Vec<(usize, Beats)>> {
    s.split(',')
        .map(|part| {
            let (a, b) = part.split_once(':').ok_or_else(|| anyhow!("scene entries look like INDEX:BEATS, got `{part}`"))?;
            Ok((a.trim().parse()?, b.trim().parse()?))
        })
        .collect()
}

fn render_mode(project: &Project, args: &[String]) -> Result<RenderMode> {
    Ok(if let Some(s) = opt(args, "--scenes") {
        RenderMode::Scenes(parse_scenes(s)?)
    } else if flag(args, "--arrangement") || project.arrangement_length() > 0.0 {
        RenderMode::Arrangement { length: opt(args, "--beats").map(str::parse).transpose()? }
    } else {
        let bar = project.time_signature.beats_per_bar();
        RenderMode::Scenes((0..project.scenes.len()).map(|i| (i, bar * 8.0)).collect())
    })
}

fn cmd_render(args: &[String]) -> Result<()> {
    let input = PathBuf::from(args.first().ok_or_else(|| anyhow!("missing <project>"))?);
    let out = PathBuf::from(opt(args, "-o").or(opt(args, "--out")).ok_or_else(|| anyhow!("missing -o <out.wav>"))?);
    let project = load(&input)?;
    let sr: u32 = opt(args, "--sr").map(str::parse).transpose()?.unwrap_or(48_000);
    let tail: f32 = opt(args, "--tail").map(str::parse).transpose()?.unwrap_or(2.0);
    let mode = render_mode(&project, args)?;
    let base = input.parent().map(Path::to_path_buf).unwrap_or_default();
    let plugins = HostFactory::new();
    let started = std::time::Instant::now();
    let r = render(&project, &base, &RenderOptions { sample_rate: sr, mode, tail_seconds: tail }, &plugins)?;
    for w in r.warnings.iter().chain(plugins.errors().iter()) {
        eprintln!("warning: {w}");
    }
    let depth = if flag(args, "--float") {
        BitDepth::Float32
    } else if flag(args, "--16") {
        BitDepth::Int16
    } else {
        BitDepth::Int24
    };
    write_audio(&out, &r.left, &r.right, sr, depth)?;
    if let Some(dir) = opt(args, "--stems") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir)?;
        let ext = out.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "wav".into());
        let opts = RenderOptions { sample_rate: sr, mode: render_mode(&project, args)?, tail_seconds: tail };
        for (i, (name, stem)) in render_stems(&project, &base, &opts, &plugins)?.into_iter().enumerate() {
            let safe: String = name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
            let p = dir.join(format!("{:02} {safe}.{ext}", i + 1));
            write_audio(&p, &stem.left, &stem.right, sr, depth)?;
            println!("stem {}", p.display());
        }
    }
    let took = started.elapsed().as_secs_f32();
    println!(
        "rendered {:.1}s of audio in {:.2}s ({:.0}x real time), peak {:.1} dBFS -> {}",
        r.seconds(),
        took,
        r.seconds() / took.max(1e-6),
        gain_to_db(r.peak()),
        out.display()
    );
    Ok(())
}

fn cmd_info(p: &Project) -> Result<()> {
    println!(
        "{}  —  {:.1} bpm, {}/{}, launch quantization {}",
        p.name,
        p.tempo,
        p.time_signature.numerator,
        p.time_signature.denominator,
        p.quantization.label()
    );
    println!("\nScenes:");
    for (i, s) in p.scenes.iter().enumerate() {
        println!("  {i}: {}{}", s.name, s.tempo.map(|t| format!("  ({t} bpm)")).unwrap_or_default());
    }
    println!("\nTracks:");
    for (i, t) in p.tracks.iter().enumerate() {
        let devices: Vec<&str> = t.instrument.iter().chain(t.effects.iter()).map(DeviceSpec::name).collect();
        println!("  {i}: {:<10} {:?}  [{}]  vol {:+.1} dB", t.name, t.kind, devices.join(" → "), t.mixer.volume_db);
        for (s, c) in t.slots.iter().enumerate() {
            if let Some(c) = c {
                let kind = match &c.content {
                    ClipContent::Midi { notes } => format!("MIDI, {} notes", notes.len()),
                    ClipContent::Audio(a) => format!("audio {} ({:?} from {} bpm)", a.file, a.warp, a.original_bpm),
                };
                println!("       slot {s}: {:<16} {:>5.1} beats  {kind}", c.name, c.length);
            }
        }
        if !t.arrangement.is_empty() {
            println!("       arrangement: {} clip(s), {} automation lane(s)", t.arrangement.len(), t.automation.len());
        }
    }
    for r in &p.returns {
        let devices: Vec<&str> = r.effects.iter().map(DeviceSpec::name).collect();
        println!("  ret {:<9} [{}]", r.name, devices.join(" → "));
    }
    let devices: Vec<&str> = p.master.effects.iter().map(DeviceSpec::name).collect();
    println!("  master        [{}]", devices.join(" → "));
    println!("\nArrangement length: {:.1} beats", p.arrangement_length());
    Ok(())
}
