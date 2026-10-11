//! Audio output: a cpal stream when a device is available, otherwise a
//! "null" output thread that runs the engine in real time without sound,
//! so the app stays fully usable on machines with no audio device.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use loopcraft_engine::Engine;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct DeviceInfo {
    pub sample_rate: f32,
    pub channels: usize,
    pub name: String,
    device: Option<cpal::Device>,
}

pub enum Output {
    #[allow(dead_code)] // held to keep the stream alive
    Device(cpal::Stream),
    Null {
        stop: Arc<AtomicBool>,
    },
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Output::Null { stop } = self {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// Find the default output device and its preferred rate.
pub fn probe() -> DeviceInfo {
    let host = cpal::default_host();
    if let Some(dev) = host.default_output_device() {
        if let Ok(cfg) = dev.default_output_config() {
            let name = dev.description().map(|d| d.to_string()).unwrap_or_else(|_| "Default output".into());
            return DeviceInfo { sample_rate: cfg.sample_rate() as f32, channels: cfg.channels() as usize, name, device: Some(dev) };
        }
    }
    DeviceInfo { sample_rate: 48_000.0, channels: 2, name: "No audio device (silent)".into(), device: None }
}

pub fn start(info: &DeviceInfo, engine: Engine) -> (Output, String) {
    // The engine sits in a slot so we can take it back if the device fails
    // to start. The callback only ever uses `try_lock`, which never blocks;
    // nothing else touches the slot once the stream is running.
    let slot = Arc::new(Mutex::new(Some(engine)));
    if let Some(dev) = &info.device {
        let config = cpal::StreamConfig { channels: info.channels as u16, sample_rate: info.sample_rate as u32, buffer_size: cpal::BufferSize::Default };
        let channels = info.channels;
        let mut scratch = vec![0.0f32; 2 * 8192];
        let cb_slot = slot.clone();
        let stream = dev.build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _| match cb_slot.try_lock() {
                Ok(mut g) => match g.as_mut() {
                    Some(engine) => {
                        for chunk in data.chunks_mut(8192 * channels) {
                            engine.process_interleaved(chunk, channels, &mut scratch);
                        }
                    }
                    None => data.fill(0.0),
                },
                Err(_) => data.fill(0.0),
            },
            |e| eprintln!("audio stream error: {e}"),
            None,
        );
        match stream {
            Ok(s) if s.play().is_ok() => return (Output::Device(s), info.name.clone()),
            Ok(_) => eprintln!("audio device would not start; running silent"),
            Err(e) => eprintln!("could not open audio device ({e}); running silent"),
        }
    }
    let engine = slot.lock().ok().and_then(|mut g| g.take()).expect("engine still in its slot");
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let sr = info.sample_rate;
    std::thread::Builder::new()
        .name("loopcraft-null-audio".into())
        .spawn(move || {
            let mut engine = engine;
            let block = 512usize;
            let (mut l, mut r) = (vec![0.0f32; block], vec![0.0f32; block]);
            let period = Duration::from_secs_f64(block as f64 / sr as f64);
            let mut next = Instant::now();
            while !flag.load(Ordering::Relaxed) {
                engine.process(&mut l, &mut r);
                next += period;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
        })
        .expect("spawn audio thread");
    let label = if info.device.is_some() { "Audio device unavailable (silent)".to_string() } else { info.name.clone() };
    (Output::Null { stop }, label)
}
