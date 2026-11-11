//! YMusicV4's C Media Source host for the player bundle's `__mse*` natives: demux, decode, the
//! A/V clock and audio output all live in C; this crate owns one host and its process hooks.

mod local;

pub use local::{Frame, LocalAv, Probe, attached_picture, probe, video_frame};

/// EQ bands' centre frequencies, Hz.
pub const EQ_BANDS_HZ: [u32; 10] = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];

/// The in-app tone shaping both engines apply (`audio_fx.c`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    /// Per band, dB.
    pub eq_db: [f32; 10],
    pub bass_db: f32,
    /// Stereo width; 1 is unchanged.
    pub virtualizer: f32,
    /// Level trim on top of the automatic loudness pre-gain, -12..=12 dB.
    pub preamp_db: f32,
}

impl Default for Tone {
    fn default() -> Self {
        Self { eq_db: [0.0; 10], bass_db: 0.0, virtualizer: 1.0, preamp_db: 0.0 }
    }
}

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
    fn mse_host_set_eq(host: *mut c_void, bands_db: *const f32);
    fn mse_host_set_bass(host: *mut c_void, db: f32);
    fn mse_host_set_virtualizer(host: *mut c_void, width: f32);
    fn mse_host_set_preamp(host: *mut c_void, db: f32);
    fn mse_host_set_video_hidden(host: *mut c_void, hidden: c_int);
    fn mse_host_pcm_levels(host: *mut c_void, out: *mut f32, count: c_int) -> c_int;
    fn mse_host_samples_played(host: *mut c_void) -> i64;
    fn mse_host_bind_presenter(host: *mut c_void, presenter: *mut c_void);
    #[cfg(target_os = "macos")]
    fn vpresent_create_vt_layer(layer: *mut c_void, sink: *const VpSink) -> *mut c_void;
    fn vpresent_release(presenter: *mut c_void);
    fn vpresent_mode(presenter: *mut c_void) -> c_int;
}

#[repr(C)]
struct VpSink {
    on_frame: Option<unsafe extern "C" fn(user: *mut c_void, width: c_int, height: c_int, bgra: *const u8)>,
    on_mode: Option<unsafe extern "C" fn(user: *mut c_void, mode: c_int)>,
    release: Option<unsafe extern "C" fn(user: *mut c_void)>,
    user: *mut c_void,
}

/// Video output into an `AVSampleBufferDisplayLayer` (zero-copy VideoToolbox frames on macOS).
pub struct Presenter {
    raw: *mut c_void,
}

unsafe impl Send for Presenter {}
unsafe impl Sync for Presenter {}

impl Presenter {
    /// # Safety
    /// `layer` must be a live `AVSampleBufferDisplayLayer` that outlives the presenter.
    #[cfg(target_os = "macos")]
    pub unsafe fn on_layer(layer: *mut c_void) -> Option<Self> {
        let sink = VpSink { on_frame: None, on_mode: None, release: None, user: std::ptr::null_mut() };
        let raw = unsafe { vpresent_create_vt_layer(layer, &sink) };
        (!raw.is_null()).then_some(Self { raw })
    }

    fn as_raw(&self) -> *mut c_void {
        self.raw
    }

    /// 1 = GPU (the layer shows frames), 2 = CPU, 0 = detached.
    pub fn mode(&self) -> i32 {
        unsafe { vpresent_mode(self.raw) }
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        unsafe { vpresent_release(self.raw) }
    }
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

    pub fn set_tone(&self, tone: &Tone) {
        unsafe {
            mse_host_set_eq(self.raw, tone.eq_db.as_ptr());
            mse_host_set_bass(self.raw, tone.bass_db);
            mse_host_set_virtualizer(self.raw, tone.virtualizer);
            mse_host_set_preamp(self.raw, tone.preamp_db);
        }
    }

    /// No view shows the picture: rests the video decoder and keeps buffering the chosen quality.
    /// Can wait on the decoder stopping, so call it off the UI thread.
    pub fn set_video_hidden(&self, hidden: bool) {
        unsafe { mse_host_set_video_hidden(self.raw, c_int::from(hidden)) }
    }

    /// Spectrum bars in 0..=1, bass first, for what is playing now.
    pub fn levels(&self, count: usize) -> Vec<f32> {
        let mut out = vec![0.0; count];
        let filled = unsafe { mse_host_pcm_levels(self.raw, out.as_mut_ptr(), count as c_int) };
        out.truncate(filled.max(0) as usize);
        out
    }

    /// Sends this host's pictures to `presenter` (`None` unbinds). The host keeps its own reference.
    pub fn bind_presenter(&self, presenter: Option<&Presenter>) {
        unsafe { mse_host_bind_presenter(self.raw, presenter.map_or(std::ptr::null_mut(), |presenter| presenter.raw)) }
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
