//! Local files: the in-process libav player (`file_av.c`) and the scan probe.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;

use crate::{Presenter, Tone};

unsafe extern "C" {
    fn file_av_create() -> *mut c_void;
    fn file_av_destroy(player: *mut c_void);
    fn file_av_open(player: *mut c_void, path: *const c_char) -> c_int;
    fn file_av_close(player: *mut c_void);
    fn file_av_play(player: *mut c_void);
    fn file_av_pause(player: *mut c_void);
    fn file_av_seek(player: *mut c_void, seconds: f64);
    fn file_av_step(player: *mut c_void) -> c_int;
    fn file_av_set_volume(player: *mut c_void, volume: f64);
    fn file_av_set_video_enabled(player: *mut c_void, on: c_int);
    fn file_av_set_speed(player: *mut c_void, speed: f64);
    fn file_av_set_pitch(player: *mut c_void, pitch: f64);
    fn file_av_set_eq(player: *mut c_void, bands_db: *const f32);
    fn file_av_set_bass(player: *mut c_void, db: f32);
    fn file_av_set_virtualizer(player: *mut c_void, width: f32);
    fn file_av_set_preamp(player: *mut c_void, db: f32);
    fn file_av_pcm_levels(player: *mut c_void, out: *mut f32, count: c_int) -> c_int;
    fn file_av_bind_presenter(player: *mut c_void, presenter: *mut c_void);
    fn file_av_duration(player: *mut c_void) -> f64;
    fn file_av_position(player: *mut c_void) -> f64;
    fn file_av_ended(player: *mut c_void) -> c_int;
    fn file_av_last_error(player: *mut c_void) -> *const c_char;
    fn ym_probe(path: *const c_char, out: *mut RawProbe) -> c_int;
}

/// The in-process file player. Every call comes from one player thread, which also calls
/// [`LocalAv::step`] in a loop while playing; decode and audio output run on file_av's own threads.
pub struct LocalAv {
    raw: *mut c_void,
}

// Driven from its one player thread; the C side guards what its own threads share.
unsafe impl Send for LocalAv {}

impl LocalAv {
    pub fn create() -> Option<Self> {
        let raw = unsafe { file_av_create() };
        (!raw.is_null()).then_some(Self { raw })
    }

    pub fn open(&mut self, path: &Path) -> Result<(), String> {
        let path = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "path contains NUL".to_owned())?;
        if unsafe { file_av_open(self.raw, path.as_ptr()) } == 0 { Ok(()) } else { Err(self.last_error()) }
    }

    pub fn close(&mut self) {
        unsafe { file_av_close(self.raw) }
    }

    pub fn play(&mut self) {
        unsafe { file_av_play(self.raw) }
    }

    pub fn pause(&mut self) {
        unsafe { file_av_pause(self.raw) }
    }

    pub fn seek(&mut self, seconds: f64) {
        unsafe { file_av_seek(self.raw, seconds) }
    }

    /// One present tick; paces itself to the next due picture.
    pub fn step(&mut self) -> i32 {
        unsafe { file_av_step(self.raw) }
    }

    pub fn set_volume(&mut self, volume: f64) {
        unsafe { file_av_set_volume(self.raw, volume.clamp(0.0, 1.0)) }
    }

    pub fn set_video_enabled(&mut self, on: bool) {
        unsafe { file_av_set_video_enabled(self.raw, c_int::from(on)) }
    }

    pub fn set_speed_pitch(&mut self, speed: f64, pitch: f64) {
        unsafe {
            file_av_set_speed(self.raw, speed);
            file_av_set_pitch(self.raw, pitch);
        }
    }

    pub fn set_tone(&mut self, tone: &Tone) {
        unsafe {
            file_av_set_eq(self.raw, tone.eq_db.as_ptr());
            file_av_set_bass(self.raw, tone.bass_db);
            file_av_set_virtualizer(self.raw, tone.virtualizer);
            file_av_set_preamp(self.raw, tone.preamp_db);
        }
    }

    /// Spectrum bars in 0..=1, bass first, for what is playing now.
    pub fn levels(&self, count: usize) -> Vec<f32> {
        let mut out = vec![0.0; count];
        let filled = unsafe { file_av_pcm_levels(self.raw, out.as_mut_ptr(), count as c_int) };
        out.truncate(filled.max(0) as usize);
        out
    }

    pub fn bind_presenter(&mut self, presenter: Option<&Presenter>) {
        unsafe { file_av_bind_presenter(self.raw, presenter.map_or(std::ptr::null_mut(), Presenter::as_raw)) }
    }

    pub fn duration(&self) -> f64 {
        unsafe { file_av_duration(self.raw) }
    }

    pub fn position(&self) -> f64 {
        unsafe { file_av_position(self.raw) }
    }

    pub fn ended(&self) -> bool {
        unsafe { file_av_ended(self.raw) != 0 }
    }

    pub fn last_error(&self) -> String {
        let raw = unsafe { file_av_last_error(self.raw) };
        if raw.is_null() { String::new() } else { unsafe { CStr::from_ptr(raw) }.to_string_lossy().into_owned() }
    }
}

impl Drop for LocalAv {
    fn drop(&mut self) {
        unsafe { file_av_destroy(self.raw) }
    }
}

#[repr(C)]
struct RawProbe {
    title: [c_char; 512],
    artist: [c_char; 512],
    album: [c_char; 512],
    genre: [c_char; 256],
    date: [c_char; 64],
    track: [c_char; 64],
    audio_codec: [c_char; 64],
    video_codec: [c_char; 64],
    subtitle_codec: [c_char; 64],
    format: [c_char; 128],
    duration_ms: i64,
    bitrate: i64,
    width: c_int,
    height: c_int,
    thumbnail_stream: c_int,
    has_audio: c_int,
    composer: [c_char; 256],
    stream_composer: [c_char; 256],
}

/// A media file's tags and stream facts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Probe {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    pub track_number: Option<i32>,
    pub audio_codec: Option<String>,
    pub video_codec: Option<String>,
    pub subtitle_codec: Option<String>,
    pub format: Option<String>,
    pub duration_ms: Option<i64>,
    pub bitrate: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub thumbnail_stream: Option<i32>,
    pub has_audio: bool,
    /// The container's `composer` tag, then the first audio stream's.
    pub composers: Vec<String>,
}

fn text(field: &[c_char]) -> Option<String> {
    let bytes: Vec<u8> = field.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    let text = String::from_utf8_lossy(&bytes).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Blocking: opens and reads the file's headers. `Err` carries libav's error code.
pub fn probe(path: &Path) -> Result<Probe, i32> {
    let path = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| -22)?;
    // SAFETY: RawProbe is plain data the C side fills completely.
    let mut raw: RawProbe = unsafe { std::mem::zeroed() };
    let rc = unsafe { ym_probe(path.as_ptr(), &mut raw) };
    if rc < 0 {
        return Err(rc);
    }
    let leading_number = |value: Option<String>| -> Option<i32> {
        value.and_then(|value| value.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok())
    };
    Ok(Probe {
        title: text(&raw.title),
        artist: text(&raw.artist),
        album: text(&raw.album),
        genre: text(&raw.genre),
        year: leading_number(text(&raw.date)),
        track_number: leading_number(text(&raw.track)),
        audio_codec: text(&raw.audio_codec),
        video_codec: text(&raw.video_codec),
        subtitle_codec: text(&raw.subtitle_codec),
        format: text(&raw.format),
        duration_ms: (raw.duration_ms >= 0).then_some(raw.duration_ms),
        bitrate: (raw.bitrate >= 0).then_some(raw.bitrate),
        width: (raw.width > 0).then_some(raw.width),
        height: (raw.height > 0).then_some(raw.height),
        thumbnail_stream: (raw.thumbnail_stream >= 0).then_some(raw.thumbnail_stream),
        has_audio: raw.has_audio != 0,
        composers: [text(&raw.composer), text(&raw.stream_composer)].into_iter().flatten().collect(),
    })
}
