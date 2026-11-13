//! The OS media surface: Now Playing info and media keys (MPNowPlayingInfoCenter on macOS, MPRIS
//! on Linux, SMTC on Windows), mirrored from [`Playback`].

use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::*;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection,
};
use ym_common::log::Logger;
use ym_i18n::{Str, t};

use crate::graph::AppGraph;
use crate::playback::{PlayState, Playback};

const SEEK_STEP_MS: i64 = 10_000;
/// Position drift that is worth re-announcing; the OS extrapolates from the last report.
const DRIFT_MS: i64 = 2_000;

pub struct MediaSession {
    controls: Option<MediaControls>,
    hwnd: Option<*mut std::ffi::c_void>,
    /// Whether the OS media surface lists the app; it does only while something is queued.
    listed: bool,
    log: Logger,
    announced_video: Option<String>,
    announced_duration: i64,
    announced_state: Option<PlayState>,
    announced_position: i64,
    announced_at: Instant,
    /// The playback speed the OS extrapolates the position at.
    announced_rate: f64,
    _observe: Subscription,
    _events: Task<()>,
}

impl MediaSession {
    pub fn new(playback: Entity<Playback>, window: &Window, cx: &mut Context<Self>) -> Self {
        let (sender, mut receiver) = mpsc::unbounded::<MediaControlEvent>();
        // MPRIS bus names are unique per bus; souvlaki's service thread panics when one is taken
        // (by a second instance or the JVM app), so each process gets the spec's instance suffix.
        let dbus_name = format!("ymusic.instance{}", std::process::id());
        let hwnd = window_hwnd(window);
        let controls = PlatformConfig { display_name: t(Str::AppName), dbus_name: &dbus_name, hwnd };
        let controls = match open_controls(controls) {
            Ok(mut controls) => match controls.attach(move |event| {
                let _ = sender.unbounded_send(event);
            }) {
                Ok(()) => Some(controls),
                Err(error) => {
                    cx.global::<AppGraph>().log.warn(&format!("R90004 media controls attach failed: {error:?}"), None);
                    None
                }
            },
            Err(error) => {
                cx.global::<AppGraph>().log.warn(&format!("R90004 media controls unavailable: {error}"), None);
                None
            }
        };
        let observe = cx.observe(&playback, |session, playback, cx| session.mirror(&playback, cx));
        let target = playback.downgrade();
        let events = cx.spawn(async move |_, cx| {
            while let Some(event) = receiver.next().await {
                let applied = target.update(cx, |playback, cx| match event {
                    MediaControlEvent::Play if !playback.is_playing() => playback.toggle(cx),
                    MediaControlEvent::Pause if playback.is_playing() => playback.toggle(cx),
                    MediaControlEvent::Toggle => playback.toggle(cx),
                    MediaControlEvent::Next => playback.next(cx),
                    MediaControlEvent::Previous => playback.previous(cx),
                    MediaControlEvent::Seek(direction) => {
                        let step = if direction == SeekDirection::Forward { SEEK_STEP_MS } else { -SEEK_STEP_MS };
                        playback.seek((playback.position_ms + step).max(0), cx);
                    }
                    MediaControlEvent::SeekBy(direction, amount) => {
                        let step = amount.as_millis() as i64;
                        let step = if direction == SeekDirection::Forward { step } else { -step };
                        playback.seek((playback.position_ms + step).max(0), cx);
                    }
                    MediaControlEvent::SetPosition(MediaPosition(position)) => {
                        playback.seek(position.as_millis() as i64, cx)
                    }
                    _ => {}
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        let mut session = Self {
            controls,
            hwnd,
            listed: true,
            log: cx.global::<AppGraph>().log.fork("media-session"),
            announced_video: None,
            announced_duration: 0,
            announced_state: None,
            announced_position: 0,
            announced_at: Instant::now(),
            announced_rate: 1.0,
            _observe: observe,
            _events: events,
        };
        session.list(false);
        session
    }

    /// Lists the app in the OS media surface or takes it off, so a stopped session offers no Play.
    fn list(&mut self, listed: bool) {
        if self.controls.is_none() || self.listed == listed {
            return;
        }
        self.listed = listed;
        if let Err(error) = set_listed(self.hwnd, listed) {
            self.log.warn(&format!("R90024 media session listing could not change: {error}"), None);
        }
    }

    fn mirror(&mut self, playback: &Entity<Playback>, cx: &mut Context<Self>) {
        if self.controls.is_none() {
            return;
        }
        let playback = playback.read(cx);
        let Some(item) = playback.current() else {
            if self.announced_video.take().is_some() {
                if let Some(controls) = self.controls.as_mut() {
                    let _ = controls.set_playback(MediaPlayback::Stopped);
                }
                self.list(false);
            }
            return;
        };
        self.list(true);
        let Some(controls) = self.controls.as_mut() else { return };
        let duration_learned = playback.duration_ms > 0 && playback.duration_ms != self.announced_duration;
        let key = item.key();
        if self.announced_video.as_deref() != Some(key.as_str()) || duration_learned {
            let cover = item.thumb(i32::MAX).and_then(|url| {
                if !crate::local_art::LocalArt::handles(&url) {
                    return Some(url.to_string());
                }
                let art = cx.try_global::<crate::local_art::SharedLocalArt>()?;
                art.0.cached_file(&url).and_then(|file| cover_file_url(&file))
            });
            let artist = item.artist.clone();
            let duration = (playback.duration_ms > 0).then(|| Duration::from_millis(playback.duration_ms as u64));
            let _ = controls.set_metadata(MediaMetadata {
                title: Some(&item.title),
                artist: artist.as_deref(),
                album: None,
                cover_url: cover.as_deref(),
                duration,
            });
            self.announced_video = Some(key);
            self.announced_duration = playback.duration_ms;
            self.announced_state = None;
        }
        let rate = f64::from(playback.effects.speed);
        if rate != self.announced_rate {
            let _ = controls.set_rate(rate);
            self.announced_rate = rate;
        }
        // While playing, the OS extrapolates from the last report at the rate; only drift from that is news.
        let elapsed = if matches!(self.announced_state, Some(PlayState::Playing)) {
            (self.announced_at.elapsed().as_millis() as f64 * self.announced_rate) as i64
        } else {
            0
        };
        let expected = self.announced_position + elapsed;
        let drifted = (playback.position_ms - expected).abs() > DRIFT_MS;
        if self.announced_state.as_ref() == Some(&playback.state) && !drifted {
            return;
        }
        let progress = Some(MediaPosition(Duration::from_millis(playback.position_ms.max(0) as u64)));
        let announced = match playback.state {
            PlayState::Playing | PlayState::Buffering => MediaPlayback::Playing { progress },
            PlayState::Idle | PlayState::Ended => MediaPlayback::Stopped,
            PlayState::Paused | PlayState::Failed(_) => MediaPlayback::Paused { progress },
        };
        let _ = controls.set_playback(announced);
        self.announced_state = Some(playback.state.clone());
        self.announced_position = playback.position_ms;
        self.announced_at = Instant::now();
    }
}

/// souvlaki panics on Windows without an HWND: there that is an unavailable session instead.
fn open_controls(config: PlatformConfig) -> Result<MediaControls, String> {
    if cfg!(target_os = "windows") && config.hwnd.is_none() {
        return Err("no window handle".to_owned());
    }
    MediaControls::new(config).map_err(|error| format!("{error:?}"))
}

#[cfg(target_os = "windows")]
fn window_hwnd(window: &Window) -> Option<*mut std::ffi::c_void> {
    crate::desktop::window_hwnd(window)
}

#[cfg(not(target_os = "windows"))]
fn window_hwnd(_: &Window) -> Option<*mut std::ffi::c_void> {
    None
}

/// Windows lists the window's SMTC while it is enabled; souvlaki toggles that only on attach and
/// detach (which drops its handlers), so the switch goes to the same SMTC fetched for the window.
#[cfg(target_os = "windows")]
fn set_listed(hwnd: Option<*mut std::ffi::c_void>, listed: bool) -> Result<(), String> {
    use windows::Media::SystemMediaTransportControls;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
    let hwnd = hwnd.ok_or("no window handle")?;
    let switch = || -> windows::core::Result<()> {
        let interop = windows::core::factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
        // SAFETY: the main window's handle, alive as long as this session.
        let controls: SystemMediaTransportControls = unsafe { interop.GetForWindow(HWND(hwnd)) }?;
        controls.SetIsEnabled(listed)
    };
    switch().map_err(|error| error.to_string())
}

/// Elsewhere the app stays listed, its state Stopped while nothing is queued.
#[cfg(not(target_os = "windows"))]
fn set_listed(_: Option<*mut std::ffi::c_void>, _: bool) -> Result<(), String> {
    Ok(())
}

/// Hack: souvlaki's SMTC strips `file://` and opens the rest as a path, so Windows gets the
/// native path behind the prefix rather than a file URL.
#[cfg(target_os = "windows")]
fn cover_file_url(file: &std::path::Path) -> Option<String> {
    Some(format!("file://{}", file.display()))
}

#[cfg(not(target_os = "windows"))]
fn cover_file_url(file: &std::path::Path) -> Option<String> {
    url::Url::from_file_path(file).ok().map(String::from)
}
