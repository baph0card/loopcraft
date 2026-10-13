//! VST3 test plugin for LoopCraft's host tests, adapted from the `vst3`
//! crate's gain example (MIT OR Apache-2.0, (c) Micah Johnston). Changes:
//! default gain 0.5, 32 samples of real (and reported) latency, component
//! and controller state, and an X11 editor that, once open, acts like a
//! user turning the knob to 0.75 (via a run-loop timer and performEdit).

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::all)]
#![allow(unsafe_op_in_unsafe_fn)]

use std::cell::{Cell, UnsafeCell};
use std::ffi::{CString, c_char, c_void};
use std::slice;
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use vst3::{Class, ComPtr, ComRef, ComWrapper, Steinberg::Linux::*, Steinberg::Vst::*, Steinberg::*, uid};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, WindowClass};

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let c_string = CString::new(src).unwrap_or_else(|_| CString::default());
    let bytes = c_string.as_bytes_with_nul();

    for (src, dst) in bytes.iter().zip(dst.iter_mut()) {
        *dst = *src as c_char;
    }

    if bytes.len() > dst.len() {
        if let Some(last) = dst.last_mut() {
            *last = 0;
        }
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (src, dst) in src.encode_utf16().zip(dst.iter_mut()) {
        *dst = src as TChar;
        len += 1;
    }

    if len < dst.len() {
        dst[len] = 0;
    } else if let Some(last) = dst.last_mut() {
        *last = 0;
    }
}

unsafe fn len_wstring(string: *const TChar) -> usize {
    let mut len = 0;

    while *string.offset(len) != 0 {
        len += 1;
    }

    len as usize
}

const PLUGIN_NAME: &'static str = "Test VST3 Gain";
const LATENCY: usize = 32;

struct GainProcessor {
    gain: AtomicU64,
    delay: UnsafeCell<([[f32; LATENCY]; 2], usize)>,
}

unsafe impl Sync for GainProcessor {}

impl Class for GainProcessor {
    type Interfaces = (IComponent, IAudioProcessor, IProcessContextRequirements);
}

impl GainProcessor {
    const CID: TUID = uid(0x6E332252, 0x54224A00, 0xAA69301A, 0xF318797D);

    fn new() -> GainProcessor {
        GainProcessor { gain: AtomicU64::new(0.5f64.to_bits()), delay: UnsafeCell::new(([[0.0; LATENCY]; 2], 0)) }
    }
}

impl IPluginBaseTrait for GainProcessor {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IComponentTrait for GainProcessor {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        *class_id = GainController::CID;
        kResultOk
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, mediaType: MediaType, dir: BusDirection) -> i32 {
        match mediaType as BusDirections {
            MediaTypes_::kAudio => match dir as BusDirections {
                BusDirections_::kInput => 1,
                BusDirections_::kOutput => 1,
                _ => 0,
            },
            MediaTypes_::kEvent => 0,
            _ => 0,
        }
    }

    unsafe fn getBusInfo(&self, mediaType: MediaType, dir: BusDirection, index: i32, bus: *mut BusInfo) -> tresult {
        match mediaType as MediaTypes {
            MediaTypes_::kAudio => match dir as BusDirections {
                BusDirections_::kInput => match index {
                    0 => {
                        let bus = &mut *bus;

                        bus.mediaType = MediaTypes_::kAudio as MediaType;
                        bus.direction = BusDirections_::kInput as BusDirection;
                        bus.channelCount = 2;
                        copy_wstring("Input", &mut bus.name);
                        bus.busType = BusTypes_::kMain as BusType;
                        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;

                        kResultOk
                    }
                    _ => kInvalidArgument,
                },
                BusDirections_::kOutput => match index {
                    0 => {
                        let bus = &mut *bus;

                        bus.mediaType = MediaTypes_::kAudio as MediaType;
                        bus.direction = BusDirections_::kOutput as BusDirection;
                        bus.channelCount = 2;
                        copy_wstring("Output", &mut bus.name);
                        bus.busType = BusTypes_::kMain as BusType;
                        bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32 as uint32;

                        kResultOk
                    }
                    _ => kInvalidArgument,
                },
                _ => kInvalidArgument,
            },
            MediaTypes_::kEvent => kInvalidArgument,
            _ => kInvalidArgument,
        }
    }

    unsafe fn getRoutingInfo(&self, _in_info: *mut RoutingInfo, _out_info: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(&self, _media_type: MediaType, _dir: BusDirection, _index: i32, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        if let Some(g) = read_f64(state) {
            self.gain.store(g.to_bits(), Ordering::Relaxed);
        }
        kResultOk
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        write_f64(state, f64::from_bits(self.gain.load(Ordering::Relaxed)))
    }
}

unsafe fn write_f64(s: *mut IBStream, v: f64) -> tresult {
    let Some(s) = ComRef::from_raw(s) else { return kInvalidArgument };
    let mut b = v.to_le_bytes();
    let mut n = 0;
    s.write(b.as_mut_ptr() as *mut c_void, 8, &mut n);
    if n == 8 { kResultOk } else { kResultFalse }
}

unsafe fn read_f64(s: *mut IBStream) -> Option<f64> {
    let s = ComRef::from_raw(s)?;
    let mut b = [0u8; 8];
    let mut n = 0;
    s.read(b.as_mut_ptr() as *mut c_void, 8, &mut n);
    (n == 8).then(|| f64::from_le_bytes(b))
}

impl IAudioProcessorTrait for GainProcessor {
    unsafe fn setBusArrangements(&self, inputs: *mut SpeakerArrangement, num_ins: i32, outputs: *mut SpeakerArrangement, num_outs: i32) -> tresult {
        if num_ins != 1 || num_outs != 1 {
            return kResultFalse;
        }

        if *inputs != SpeakerArr::kStereo || *outputs != SpeakerArr::kStereo {
            return kResultFalse;
        }

        kResultTrue
    }

    unsafe fn getBusArrangement(&self, dir: BusDirection, index: i32, arr: *mut SpeakerArrangement) -> tresult {
        match dir as BusDirections {
            BusDirections_::kInput => {
                if index == 0 {
                    *arr = SpeakerArr::kStereo;
                    kResultOk
                } else {
                    kInvalidArgument
                }
            }
            BusDirections_::kOutput => {
                if index == 0 {
                    *arr = SpeakerArr::kStereo;
                    kResultOk
                } else {
                    kInvalidArgument
                }
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn canProcessSampleSize(&self, symbolic_sample_size: i32) -> tresult {
        match symbolic_sample_size as SymbolicSampleSizes {
            SymbolicSampleSizes_::kSample32 => kResultOk as i32,
            SymbolicSampleSizes_::kSample64 => kNotImplemented as i32,
            _ => kInvalidArgument,
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        LATENCY as u32
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        kResultOk
    }

    unsafe fn setProcessing(&self, _state: TBool) -> tresult {
        kResultOk
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let process_data = &*data;

        if let Some(param_changes) = ComRef::from_raw(process_data.inputParameterChanges) {
            let param_count = param_changes.getParameterCount();
            for param_index in 0..param_count {
                if let Some(param_queue) = ComRef::from_raw(param_changes.getParameterData(param_index)) {
                    let param_id = param_queue.getParameterId();
                    let point_count = param_queue.getPointCount();

                    match param_id {
                        0 => {
                            let mut sample_offset = 0;
                            let mut value = 0.0;
                            let result = param_queue.getPoint(point_count - 1, &mut sample_offset, &mut value);

                            if result == kResultTrue {
                                self.gain.store(value.to_bits(), Ordering::Relaxed);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        let gain = f64::from_bits(self.gain.load(Ordering::Relaxed)) as f32;

        let num_samples = process_data.numSamples as usize;

        if process_data.numInputs != 1 || process_data.numOutputs != 1 {
            return kResultOk;
        }

        let input_buses = slice::from_raw_parts(process_data.inputs, process_data.numInputs as usize);
        let output_buses = slice::from_raw_parts(process_data.outputs, process_data.numOutputs as usize);

        if input_buses[0].numChannels != 2 || output_buses[0].numChannels != 2 {
            return kResultOk;
        }

        let input_channels = slice::from_raw_parts(input_buses[0].__field0.channelBuffers32, input_buses[0].numChannels as usize);
        let output_channels = slice::from_raw_parts(output_buses[0].__field0.channelBuffers32, output_buses[0].numChannels as usize);

        let input_l = slice::from_raw_parts(input_channels[0], num_samples);
        let input_r = slice::from_raw_parts(input_channels[1], num_samples);
        let output_l = slice::from_raw_parts_mut(output_channels[0], num_samples);
        let output_r = slice::from_raw_parts_mut(output_channels[1], num_samples);

        let (buf, pos) = &mut *self.delay.get();
        for i in 0..num_samples {
            let (l, r) = (input_l[i], input_r[i]);
            output_l[i] = gain * buf[0][*pos];
            output_r[i] = gain * buf[1][*pos];
            buf[0][*pos] = l;
            buf[1][*pos] = r;
            *pos = (*pos + 1) % LATENCY;
        }

        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IProcessContextRequirementsTrait for GainProcessor {
    unsafe fn getProcessContextRequirements(&self) -> u32 {
        0
    }
}

struct GainController {
    gain: Cell<f64>,
    handler: Mutex<Option<ComPtr<IComponentHandler>>>,
}

impl Class for GainController {
    type Interfaces = (IEditController,);
}

impl GainController {
    const CID: TUID = uid(0x1BA8A477, 0xEE0A4A2D, 0x80F50D14, 0x13D2EAA0);

    fn new() -> GainController {
        GainController { gain: Cell::new(0.5), handler: Mutex::new(None) }
    }
}

impl IPluginBaseTrait for GainController {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        kResultOk
    }
}

impl IEditControllerTrait for GainController {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        if let Some(g) = read_f64(state) {
            self.gain.set(g);
        }
        kResultOk
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> i32 {
        1
    }

    unsafe fn getParameterInfo(&self, param_index: i32, info: *mut ParameterInfo) -> tresult {
        match param_index {
            0 => {
                let info = &mut *info;

                info.id = 0;
                copy_wstring("Gain", &mut info.title);
                copy_wstring("Gain", &mut info.shortTitle);
                copy_wstring("", &mut info.units);
                info.stepCount = 0;
                info.defaultNormalizedValue = 0.5;
                info.unitId = 0;
                info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;

                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getParamStringByValue(&self, id: u32, value_normalized: f64, string: *mut String128) -> tresult {
        let slice = unsafe { &mut *string };

        match id {
            0 => {
                let display = format!("{value_normalized}");
                copy_wstring(&display, slice);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn getParamValueByString(&self, id: u32, string: *mut TChar, value_normalized: *mut f64) -> tresult {
        match id {
            0 => {
                let len = len_wstring(string as *const TChar);
                if let Ok(string) = String::from_utf16(slice::from_raw_parts(string as *const u16, len)) {
                    if let Ok(value) = f64::from_str(&string) {
                        *value_normalized = value;
                        return kResultOk;
                    }
                }
                kInvalidArgument
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn normalizedParamToPlain(&self, id: u32, value_normalized: f64) -> f64 {
        match id {
            0 => value_normalized,
            _ => 0.0,
        }
    }

    unsafe fn plainParamToNormalized(&self, id: u32, plain_value: f64) -> f64 {
        match id {
            0 => plain_value,
            _ => 0.0,
        }
    }

    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        match id {
            0 => self.gain.get(),
            _ => 0.0,
        }
    }

    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        match id {
            0 => {
                self.gain.set(value);
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        *self.handler.lock().unwrap() = ComRef::from_raw(handler).map(|h| h.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
        let handler = self.handler.lock().unwrap().clone();
        ComWrapper::new(View { handler, frame: Mutex::new(None), window: Mutex::new(None), timer: Mutex::new(None) })
            .to_com_ptr::<IPlugView>()
            .unwrap()
            .into_raw()
    }
}

/// The editor: an orange X11 child window plus a run-loop timer.
struct View {
    handler: Option<ComPtr<IComponentHandler>>,
    frame: Mutex<Option<ComPtr<IPlugFrame>>>,
    window: Mutex<Option<(x11rb::rust_connection::RustConnection, u32)>>,
    timer: Mutex<Option<ComWrapper<Tick>>>,
}

unsafe impl Send for View {}
unsafe impl Sync for View {}

impl Class for View {
    type Interfaces = (IPlugView,);
}

struct Tick {
    handler: Option<ComPtr<IComponentHandler>>,
    done: Cell<bool>,
}

unsafe impl Send for Tick {}
unsafe impl Sync for Tick {}

impl Class for Tick {
    type Interfaces = (ITimerHandler,);
}

impl ITimerHandlerTrait for Tick {
    unsafe fn onTimer(&self) {
        if !self.done.get() {
            self.done.set(true);
            if let Some(h) = &self.handler {
                h.beginEdit(0);
                h.performEdit(0, 0.75);
                h.endEdit(0);
            }
        }
    }
}

impl IPlugViewTrait for View {
    unsafe fn isPlatformTypeSupported(&self, t: FIDString) -> tresult {
        if std::ffi::CStr::from_ptr(t) == std::ffi::CStr::from_ptr(kPlatformTypeX11EmbedWindowID) { kResultTrue } else { kResultFalse }
    }
    unsafe fn attached(&self, parent: *mut c_void, _t: FIDString) -> tresult {
        let Ok((conn, screen)) = x11rb::connect(None) else { return kResultFalse };
        let Ok(id) = conn.generate_id() else { return kResultFalse };
        let px = conn.setup().roots[screen].white_pixel;
        let aux = CreateWindowAux::new().background_pixel(0x3C_9A_F0 & px);
        if conn.create_window(x11rb::COPY_DEPTH_FROM_PARENT, id, parent as usize as u32, 0, 0, 320, 180, 0, WindowClass::INPUT_OUTPUT, 0, &aux).is_err() {
            return kResultFalse;
        }
        let _ = conn.map_window(id);
        let _ = conn.flush();
        *self.window.lock().unwrap() = Some((conn, id));
        // Ask the host's run loop for a timer, as real Linux editors do.
        if let Some(rl) = self.frame.lock().unwrap().as_ref().and_then(|f| f.cast::<IRunLoop>()) {
            let tick = ComWrapper::new(Tick { handler: self.handler.clone(), done: Cell::new(false) });
            rl.registerTimer(tick.as_com_ref::<ITimerHandler>().unwrap().as_ptr(), 10);
            *self.timer.lock().unwrap() = Some(tick);
        }
        kResultOk
    }
    unsafe fn removed(&self) -> tresult {
        if let Some((c, w)) = self.window.lock().unwrap().take() {
            let _ = c.destroy_window(w);
            let _ = c.flush();
        }
        if let (Some(t), Some(rl)) = (self.timer.lock().unwrap().take(), self.frame.lock().unwrap().as_ref().and_then(|f| f.cast::<IRunLoop>())) {
            rl.unregisterTimer(t.as_com_ref::<ITimerHandler>().unwrap().as_ptr());
        }
        kResultOk
    }
    unsafe fn onWheel(&self, _d: f32) -> tresult {
        kResultFalse
    }
    unsafe fn onKeyDown(&self, _k: char16, _c: int16, _m: int16) -> tresult {
        kResultFalse
    }
    unsafe fn onKeyUp(&self, _k: char16, _c: int16, _m: int16) -> tresult {
        kResultFalse
    }
    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        *size = ViewRect { left: 0, top: 0, right: 320, bottom: 180 };
        kResultOk
    }
    unsafe fn onSize(&self, _s: *mut ViewRect) -> tresult {
        kResultOk
    }
    unsafe fn onFocus(&self, _s: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        *self.frame.lock().unwrap() = ComRef::from_raw(frame).map(|f| f.to_com_ptr());
        kResultOk
    }
    unsafe fn canResize(&self) -> tresult {
        kResultFalse
    }
    unsafe fn checkSizeConstraint(&self, _r: *mut ViewRect) -> tresult {
        kResultOk
    }
}

struct Factory {}

impl Class for Factory {
    type Interfaces = (IPluginFactory,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        let info = &mut *info;

        copy_cstring("LoopCraft tests", &mut info.vendor);
        copy_cstring("https://example.com", &mut info.url);
        copy_cstring("someone@example.com", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;

        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        2
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        match index {
            0 => {
                let info = &mut *info;
                info.cid = GainProcessor::CID;
                info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
                copy_cstring("Audio Module Class", &mut info.category);
                copy_cstring(PLUGIN_NAME, &mut info.name);

                kResultOk
            }
            1 => {
                let info = &mut *info;
                info.cid = GainController::CID;
                info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
                copy_cstring("Component Controller Class", &mut info.category);
                copy_cstring(PLUGIN_NAME, &mut info.name);

                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
        let instance = match *(cid as *const TUID) {
            GainProcessor::CID => Some(ComWrapper::new(GainProcessor::new()).to_com_ptr::<FUnknown>().unwrap()),
            GainController::CID => Some(ComWrapper::new(GainController::new()).to_com_ptr::<FUnknown>().unwrap()),
            _ => None,
        };

        if let Some(instance) = instance {
            let ptr = instance.as_ptr();
            ((*(*ptr).vtbl).queryInterface)(ptr, iid as *mut TUID, obj)
        } else {
            kInvalidArgument
        }
    }
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn InitDll() -> bool {
    true
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn ExitDll() -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn BundleEntry(_bundle_ref: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn BundleExit() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleEntry(_library_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleExit() -> bool {
    true
}

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    ComWrapper::new(Factory {}).to_com_ptr::<IPluginFactory>().unwrap().into_raw()
}
