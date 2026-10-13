//! A minimal CLAP plugin library written against the raw C API, used to
//! test LoopCraft's host. Contains:
//!   - `dev.loopcraft.test-gain`: gain effect (default 0.5) with one
//!     parameter ("Gain", id 7) and 16 samples of reported, real latency
//!   - `dev.loopcraft.test-sine`: monophonic sine (instrument)

#![allow(clippy::missing_safety_doc)]

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::params::{CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE, clap_param_info, clap_plugin_params};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::{CLAP_PROCESS_CONTINUE, clap_process, clap_process_status};
use clap_sys::version::CLAP_VERSION;
use std::ffi::{CStr, c_char, c_void};
use std::ptr;

struct Features([*const c_char; 2]);
unsafe impl Sync for Features {}

static GAIN_FEATURES: Features = Features([c"audio-effect".as_ptr(), ptr::null()]);
static SINE_FEATURES: Features = Features([c"instrument".as_ptr(), ptr::null()]);

const fn desc(id: &'static CStr, name: &'static CStr, features: &'static Features) -> clap_plugin_descriptor {
    clap_plugin_descriptor {
        clap_version: CLAP_VERSION,
        id: id.as_ptr(),
        name: name.as_ptr(),
        vendor: c"LoopCraft tests".as_ptr(),
        url: c"".as_ptr(),
        manual_url: c"".as_ptr(),
        support_url: c"".as_ptr(),
        version: c"1.0.0".as_ptr(),
        description: c"test plugin".as_ptr(),
        features: features.0.as_ptr(),
    }
}

static GAIN: clap_plugin_descriptor = desc(c"dev.loopcraft.test-gain", c"Test Gain", &GAIN_FEATURES);
static SINE: clap_plugin_descriptor = desc(c"dev.loopcraft.test-sine", c"Test Sine", &SINE_FEATURES);

struct Data {
    clap: clap_plugin,
    sine: bool,
    sample_rate: f64,
    phase: f64,
    freq: f64,
    gate: f32,
    gain: f64,
    /// Delay line for the gain effect's latency.
    delay: [[f32; LATENCY]; 2],
    pos: usize,
}

const LATENCY: usize = 16;
const GAIN_ID: clap_id = 7;

unsafe extern "C" fn p_count(_: *const clap_plugin) -> u32 {
    1
}
unsafe extern "C" fn p_info(_: *const clap_plugin, i: u32, info: *mut clap_param_info) -> bool {
    if i != 0 {
        return false;
    }
    unsafe {
        let info = &mut *info;
        info.id = GAIN_ID;
        info.flags = CLAP_PARAM_IS_AUTOMATABLE;
        info.cookie = ptr::null_mut();
        info.name = [0; 256];
        for (d, s) in info.name.iter_mut().zip(b"Gain") {
            *d = *s as c_char;
        }
        info.module = [0; 1024];
        info.min_value = 0.0;
        info.max_value = 2.0;
        info.default_value = 0.5;
    }
    true
}
unsafe extern "C" fn p_value(p: *const clap_plugin, id: clap_id, out: *mut f64) -> bool {
    if id != GAIN_ID {
        return false;
    }
    unsafe { *out = data(p).gain };
    true
}
unsafe extern "C" fn p_flush(_: *const clap_plugin, _: *const clap_input_events, _: *const clap_output_events) {}

static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(p_count),
    get_info: Some(p_info),
    get_value: Some(p_value),
    value_to_text: None,
    text_to_value: None,
    flush: Some(p_flush),
};

unsafe extern "C" fn latency_get(_: *const clap_plugin) -> u32 {
    LATENCY as u32
}
static LATENCY_EXT: clap_plugin_latency = clap_plugin_latency { get: Some(latency_get) };

unsafe fn data<'a>(p: *const clap_plugin) -> &'a mut Data {
    unsafe { &mut *((*p).plugin_data as *mut Data) }
}

unsafe extern "C" fn init(_: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn destroy(p: *const clap_plugin) {
    unsafe { drop(Box::from_raw((*p).plugin_data as *mut Data)) };
}
unsafe extern "C" fn activate(p: *const clap_plugin, sr: f64, _: u32, _: u32) -> bool {
    unsafe { data(p).sample_rate = sr };
    true
}
unsafe extern "C" fn deactivate(_: *const clap_plugin) {}
unsafe extern "C" fn start(_: *const clap_plugin) -> bool {
    true
}
unsafe extern "C" fn stop(_: *const clap_plugin) {}
unsafe extern "C" fn reset(_: *const clap_plugin) {}
unsafe extern "C" fn get_extension(p: *const clap_plugin, id: *const c_char) -> *const c_void {
    unsafe {
        if data(p).sine {
            return ptr::null();
        }
        let id = CStr::from_ptr(id);
        if id == CLAP_EXT_PARAMS {
            &PARAMS as *const _ as *const c_void
        } else if id == CLAP_EXT_LATENCY {
            &LATENCY_EXT as *const _ as *const c_void
        } else {
            ptr::null()
        }
    }
}
unsafe extern "C" fn on_main_thread(_: *const clap_plugin) {}

unsafe extern "C" fn process(p: *const clap_plugin, pr: *const clap_process) -> clap_process_status {
    unsafe {
        let d = data(p);
        let pr = &*pr;
        let n = pr.frames_count as usize;
        let out: &clap_audio_buffer = &*pr.audio_outputs;
        let (ol, or) = (*out.data32, *out.data32.add(1));
        if d.sine {
            let ev = &*pr.in_events;
            let count = ev.size.map(|f| f(ev)).unwrap_or(0);
            for i in 0..count {
                let h = ev.get.map(|f| f(ev, i)).unwrap_or(ptr::null());
                if h.is_null() || (*h).space_id != CLAP_CORE_EVENT_SPACE_ID {
                    continue;
                }
                let note = &*(h as *const clap_event_note);
                match (*h).type_ {
                    CLAP_EVENT_NOTE_ON => {
                        d.freq = 440.0 * 2f64.powf((note.key as f64 - 69.0) / 12.0);
                        d.gate = note.velocity as f32;
                    }
                    CLAP_EVENT_NOTE_OFF => d.gate = 0.0,
                    _ => {}
                }
            }
            for i in 0..n {
                let s = (d.phase * std::f64::consts::TAU).sin() as f32 * d.gate * 0.5;
                d.phase = (d.phase + d.freq / d.sample_rate).fract();
                *ol.add(i) = s;
                *or.add(i) = s;
            }
        } else {
            let ev = &*pr.in_events;
            let count = ev.size.map(|f| f(ev)).unwrap_or(0);
            for i in 0..count {
                let h = ev.get.map(|f| f(ev, i)).unwrap_or(ptr::null());
                if !h.is_null() && (*h).space_id == CLAP_CORE_EVENT_SPACE_ID && (*h).type_ == CLAP_EVENT_PARAM_VALUE {
                    let pv = &*(h as *const clap_event_param_value);
                    if pv.param_id == GAIN_ID {
                        d.gain = pv.value;
                    }
                }
            }
            let inp: &clap_audio_buffer = &*pr.audio_inputs;
            let (il, ir) = (*inp.data32, *inp.data32.add(1));
            let g = d.gain as f32;
            for i in 0..n {
                let (l, r) = (*il.add(i), *ir.add(i));
                *ol.add(i) = d.delay[0][d.pos] * g;
                *or.add(i) = d.delay[1][d.pos] * g;
                d.delay[0][d.pos] = l;
                d.delay[1][d.pos] = r;
                d.pos = (d.pos + 1) % LATENCY;
            }
        }
    }
    CLAP_PROCESS_CONTINUE
}

unsafe extern "C" fn count(_: *const clap_plugin_factory) -> u32 {
    2
}

unsafe extern "C" fn descriptor(_: *const clap_plugin_factory, i: u32) -> *const clap_plugin_descriptor {
    match i {
        0 => &GAIN,
        1 => &SINE,
        _ => ptr::null(),
    }
}

unsafe extern "C" fn create(_: *const clap_plugin_factory, _: *const clap_host, id: *const c_char) -> *const clap_plugin {
    let id = unsafe { CStr::from_ptr(id) };
    let (desc, sine): (&'static clap_plugin_descriptor, bool) = if id == c"dev.loopcraft.test-gain" {
        (&GAIN, false)
    } else if id == c"dev.loopcraft.test-sine" {
        (&SINE, true)
    } else {
        return ptr::null();
    };
    let d = Box::into_raw(Box::new(Data {
        clap: clap_plugin {
            desc,
            plugin_data: ptr::null_mut(),
            init: Some(init),
            destroy: Some(destroy),
            activate: Some(activate),
            deactivate: Some(deactivate),
            start_processing: Some(start),
            stop_processing: Some(stop),
            reset: Some(reset),
            process: Some(process),
            get_extension: Some(get_extension),
            on_main_thread: Some(on_main_thread),
        },
        sine,
        sample_rate: 48_000.0,
        phase: 0.0,
        freq: 440.0,
        gate: 0.0,
        gain: 0.5,
        delay: [[0.0; LATENCY]; 2],
        pos: 0,
    }));
    unsafe {
        (*d).clap.plugin_data = d as *mut c_void;
        &(*d).clap
    }
}

static FACTORY: clap_plugin_factory =
    clap_plugin_factory { get_plugin_count: Some(count), get_plugin_descriptor: Some(descriptor), create_plugin: Some(create) };

unsafe extern "C" fn entry_init(_: *const c_char) -> bool {
    true
}
unsafe extern "C" fn entry_deinit() {}
unsafe extern "C" fn get_factory(id: *const c_char) -> *const c_void {
    if unsafe { CStr::from_ptr(id) } == CLAP_PLUGIN_FACTORY_ID { &FACTORY as *const _ as *const c_void } else { ptr::null() }
}

#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static clap_entry: clap_plugin_entry =
    clap_plugin_entry { clap_version: CLAP_VERSION, init: Some(entry_init), deinit: Some(entry_deinit), get_factory: Some(get_factory) };
