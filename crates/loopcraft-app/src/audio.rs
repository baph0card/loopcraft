//! Audio I/O: a cpal output stream (plus an optional input stream feeding
//! it through a ring buffer), or a silent real-time thread when there is
//! no audio device, so the app stays fully usable on any machine.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use loopcraft_engine::Engine;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AudioSettings {
    /// Output device name; `None` = system default.
    pub output: Option<String>,
    pub input: Option<String>,
    pub input_enabled: bool,
    pub sample_rate: Option<u32>,
    /// Frames per buffer; `None` = device default.
    pub buffer: Option<u32>,
}

pub struct DeviceList {
    pub outputs: Vec<String>,
    pub inputs: Vec<String>,
}

fn name_of(d: &cpal::Device) -> String {
    d.description().map(|d| d.to_string()).unwrap_or_else(|_| "Unknown device".into())
}

pub fn list_devices() -> DeviceList {
    let host = cpal::default_host();
    let outputs = host.output_devices().map(|it| it.map(|d| name_of(&d)).collect()).unwrap_or_default();
    let inputs = host.input_devices().map(|it| it.map(|d| name_of(&d)).collect()).unwrap_or_default();
    DeviceList { outputs, inputs }
}

/// What the running stream ended up using.
#[derive(Clone)]
pub struct DeviceInfo {
    pub sample_rate: f32,
    pub out_channels: usize,
    pub in_channels: usize,
    pub name: String,
    pub input_name: Option<String>,
    pub buffer: Option<u32>,
}

/// Pick devices and a config without starting anything.
pub fn probe(settings: &AudioSettings) -> (DeviceInfo, Option<cpal::Device>, Option<cpal::Device>) {
    let host = cpal::default_host();
    let out = match &settings.output {
        Some(n) => host.output_devices().ok().and_then(|mut it| it.find(|d| &name_of(d) == n)).or_else(|| host.default_output_device()),
        None => host.default_output_device(),
    };
    let inp = if settings.input_enabled {
        match &settings.input {
            Some(n) => host.input_devices().ok().and_then(|mut it| it.find(|d| &name_of(d) == n)).or_else(|| host.default_input_device()),
            None => host.default_input_device(),
        }
    } else {
        None
    };
    let (sr, och) = out.as_ref().and_then(|d| d.default_output_config().ok()).map(|c| (c.sample_rate(), c.channels() as usize)).unwrap_or((48_000, 2));
    let sr = settings.sample_rate.unwrap_or(sr);
    let ich = inp.as_ref().and_then(|d| d.default_input_config().ok()).map(|c| c.channels() as usize).unwrap_or(0);
    let info = DeviceInfo {
        sample_rate: sr as f32,
        out_channels: och,
        in_channels: ich,
        name: out.as_ref().map(name_of).unwrap_or_else(|| "No audio device (silent)".into()),
        input_name: inp.as_ref().map(name_of),
        buffer: settings.buffer,
    };
    (info, out, inp)
}

/// No devices at all (tests, or when the user picks "None").
pub fn probe_none() -> (DeviceInfo, Option<cpal::Device>, Option<cpal::Device>) {
    (DeviceInfo { sample_rate: 48_000.0, out_channels: 2, in_channels: 0, name: "No audio device (silent)".into(), input_name: None, buffer: None }, None, None)
}

pub enum Output {
    #[allow(dead_code)] // held to keep the streams alive
    Device(cpal::Stream, Option<cpal::Stream>),
    Null { stop: Arc<AtomicBool> },
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Output::Null { stop } = self {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

const MAX_FRAMES: usize = 8192;

pub fn start(info: &DeviceInfo, out_dev: Option<cpal::Device>, in_dev: Option<cpal::Device>, engine: Engine) -> (Output, String) {
    // The engine sits in a slot so we can take it back if the device fails
    // to start. The callback only ever uses `try_lock`, which never blocks;
    // nothing else touches the slot once the stream is running.
    let slot = Arc::new(Mutex::new(Some(engine)));
    if let Some(dev) = &out_dev {
        let buffer_size = match info.buffer {
            Some(b) => cpal::BufferSize::Fixed(b),
            None => cpal::BufferSize::Default,
        };
        let config = cpal::StreamConfig { channels: info.out_channels as u16, sample_rate: info.sample_rate as u32, buffer_size };
        let och = info.out_channels.max(1);
        let mut ich = info.in_channels;

        // Input: device callback -> ring -> output callback.
        let (mut in_tx, mut in_rx) = rtrb::RingBuffer::<f32>::new((info.sample_rate as usize).max(4096) * ich.max(1));
        let mut in_stream = None;
        if let (Some(idev), true) = (&in_dev, ich > 0) {
            let icfg = cpal::StreamConfig { channels: ich as u16, sample_rate: info.sample_rate as u32, buffer_size: cpal::BufferSize::Default };
            match idev.build_input_stream::<f32, _, _>(
                icfg,
                move |data: &[f32], _| {
                    let n = data.len().min(in_tx.slots());
                    if let Ok(mut c) = in_tx.write_chunk(n) {
                        let (a, b) = c.as_mut_slices();
                        let la = a.len();
                        a.copy_from_slice(&data[..la]);
                        b.copy_from_slice(&data[la..n]);
                        c.commit_all();
                    }
                },
                |e| eprintln!("audio input error: {e}"),
                None,
            ) {
                Ok(s) => {
                    if s.play().is_ok() {
                        in_stream = Some(s);
                    }
                }
                Err(e) => eprintln!("could not open audio input ({e})"),
            }
        }
        if in_stream.is_none() {
            ich = 0;
        }

        let mut scratch = vec![0.0f32; 2 * MAX_FRAMES];
        let mut in_buf = vec![0.0f32; MAX_FRAMES * ich.max(1)];
        let cb_slot = slot.clone();
        let stream = dev.build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _| match cb_slot.try_lock() {
                Ok(mut g) => match g.as_mut() {
                    Some(engine) => {
                        for chunk in data.chunks_mut(MAX_FRAMES * och) {
                            let frames = chunk.len() / och;
                            let want = frames * ich;
                            let got = if ich > 0 {
                                let k = want.min(in_rx.slots());
                                if let Ok(c) = in_rx.read_chunk(k) {
                                    let (a, b) = c.as_slices();
                                    in_buf[..a.len()].copy_from_slice(a);
                                    in_buf[a.len()..a.len() + b.len()].copy_from_slice(b);
                                    c.commit_all();
                                }
                                k
                            } else {
                                0
                            };
                            in_buf[got..want].fill(0.0);
                            engine.process_io(&in_buf[..want], ich, chunk, och, &mut scratch);
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
            Ok(s) if s.play().is_ok() => {
                let label = match &info.input_name {
                    Some(i) if ich > 0 => format!("{} · in: {i}", info.name),
                    _ => info.name.clone(),
                };
                return (Output::Device(s, in_stream), label);
            }
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
    let label = if out_dev.is_some() { "Audio device unavailable (silent)".to_string() } else { info.name.clone() };
    (Output::Null { stop }, label)
}
