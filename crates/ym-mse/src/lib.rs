//! YMusicV4's C Media Source host for the player bundle's `__mse*` natives: demux, decode, the
//! A/V clock and audio output all live in C; this crate owns one host and its process hooks.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

unsafe extern "C" {
    fn mse_host_create() -> *mut c_void;
    fn mse_host_destroy(host: *mut c_void);
    fn mse_host_vtable() -> *const c_void;
    fn mse_host_set_js_event(callback: Option<unsafe extern "C" fn(name: *const c_char)>);
    fn mse_set_log_callback(callback: Option<unsafe extern "C" fn(level: c_int, message: *const c_char)>);
    fn mse_host_set_volume(host: *mut c_void, volume: f32);
    fn mse_host_set_speed(host: *mut c_void, speed: f32, tempo: f32);
    fn mse_host_samples_played(host: *mut c_void) -> i64;
}

/// One native MSE host. Attach it to the player's QuickJS runtime before the bundle evaluates;
/// destroy it only after that runtime is closed (JS calls straight into it).
pub struct MseHost {
    raw: *mut c_void,
}

// The C host serializes its own state across the QuickJS, decode and audio threads.
unsafe impl Send for MseHost {}
unsafe impl Sync for MseHost {}

impl MseHost {
    pub fn create() -> Option<Self> {
        let raw = unsafe { mse_host_create() };
        (!raw.is_null()).then_some(Self { raw })
    }

    /// The host pointer and `__mse*` vtable to hand to `QuickJsRuntime::attach_mse_host`.
    pub fn attach_pointers(&self) -> (usize, usize) {
        (self.raw as usize, unsafe { mse_host_vtable() } as usize)
    }

    /// App volume times the bundle's loudness gain, 0..=1.
    pub fn set_volume(&self, volume: f32) {
        unsafe { mse_host_set_volume(self.raw, volume.clamp(0.0, 1.0)) }
    }

    pub fn set_speed(&self, speed: f32, tempo: f32) {
        unsafe { mse_host_set_speed(self.raw, speed, tempo) }
    }

    pub fn samples_played(&self) -> i64 {
        unsafe { mse_host_samples_played(self.raw) }
    }
}

impl Drop for MseHost {
    fn drop(&mut self) {
        unsafe { mse_host_destroy(self.raw) }
    }
}

type EventSink = Box<dyn Fn(&str) + Send + Sync>;
type LogSink = Box<dyn Fn(i32, &str) + Send + Sync>;

fn event_sink() -> &'static Mutex<Option<EventSink>> {
    static SINK: OnceLock<Mutex<Option<EventSink>>> = OnceLock::new();
    SINK.get_or_init(Mutex::default)
}

fn log_sink() -> &'static Mutex<Option<LogSink>> {
    static SINK: OnceLock<Mutex<Option<LogSink>>> = OnceLock::new();
    SINK.get_or_init(Mutex::default)
}

unsafe extern "C" fn on_event(name: *const c_char) {
    if name.is_null() {
        return;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    if let Some(sink) = event_sink().lock().unwrap().as_ref() {
        sink(&name);
    }
}

unsafe extern "C" fn on_log(level: c_int, message: *const c_char) {
    if message.is_null() {
        return;
    }
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    if let Some(sink) = log_sink().lock().unwrap().as_ref() {
        sink(level, &message);
    }
}

/// Process-wide: where C's host→JS events (`mse_ended`) go. The last setter wins.
pub fn set_event_sink(sink: impl Fn(&str) + Send + Sync + 'static) {
    *event_sink().lock().unwrap() = Some(Box::new(sink));
    unsafe { mse_host_set_js_event(Some(on_event)) };
}

/// Process-wide: C's notable lines, level 0 debug … 3 error. The last setter wins.
pub fn set_log_sink(sink: impl Fn(i32, &str) + Send + Sync + 'static) {
    *log_sink().lock().unwrap() = Some(Box::new(sink));
    unsafe { mse_set_log_callback(Some(on_log)) };
}
