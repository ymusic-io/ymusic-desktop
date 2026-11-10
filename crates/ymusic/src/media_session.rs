//! The OS media surface: Now Playing info and media keys (MPNowPlayingInfoCenter on macOS, MPRIS
//! on Linux, SMTC on Windows), mirrored from [`Playback`].

use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::*;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection,
};
use ym_i18n::{Str, t};

use crate::graph::AppGraph;
use crate::playback::{PlayState, Playback};

const SEEK_STEP_MS: i64 = 10_000;
/// Position drift that is worth re-announcing; the OS extrapolates from the last report.
const DRIFT_MS: i64 = 2_000;

pub struct MediaSession {
    controls: Option<MediaControls>,
    announced_video: Option<String>,
    announced_duration: i64,
    announced_state: Option<PlayState>,
    announced_position: i64,
    announced_at: Instant,
    _observe: Subscription,
    _events: Task<()>,
}

impl MediaSession {
    pub fn new(playback: Entity<Playback>, cx: &mut Context<Self>) -> Self {
        let (sender, mut receiver) = mpsc::unbounded::<MediaControlEvent>();
        let controls = PlatformConfig { display_name: t(Str::AppName), dbus_name: "ymusic", hwnd: None };
        let controls = match MediaControls::new(controls) {
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
                cx.global::<AppGraph>().log.warn(&format!("R90004 media controls unavailable: {error:?}"), None);
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
        Self {
            controls,
            announced_video: None,
            announced_duration: 0,
            announced_state: None,
            announced_position: 0,
            announced_at: Instant::now(),
            _observe: observe,
            _events: events,
        }
    }

    fn mirror(&mut self, playback: &Entity<Playback>, cx: &mut Context<Self>) {
        let Some(controls) = self.controls.as_mut() else { return };
        let playback = playback.read(cx);
        let Some(item) = playback.current() else {
            if self.announced_video.take().is_some() {
                let _ = controls.set_playback(MediaPlayback::Stopped);
            }
            return;
        };
        let duration_learned = playback.duration_ms > 0 && playback.duration_ms != self.announced_duration;
        if self.announced_video.as_deref() != Some(item.id.as_str()) || duration_learned {
            let cover = item.thumbs.last().map(|thumb| {
                if thumb.url.starts_with("//") { format!("https:{}", thumb.url) } else { thumb.url.clone() }
            });
            let artist = item.channel_title.clone().or(item.subtitle_line1.clone());
            let duration = (playback.duration_ms > 0).then(|| Duration::from_millis(playback.duration_ms as u64));
            let _ = controls.set_metadata(MediaMetadata {
                title: Some(&item.title),
                artist: artist.as_deref(),
                album: None,
                cover_url: cover.as_deref(),
                duration,
            });
            self.announced_video = Some(item.id.clone());
            self.announced_duration = playback.duration_ms;
            self.announced_state = None;
        }
        // While playing, the OS extrapolates from the last report; only drift from that is news.
        let elapsed = if matches!(self.announced_state, Some(PlayState::Playing)) {
            self.announced_at.elapsed().as_millis() as i64
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
