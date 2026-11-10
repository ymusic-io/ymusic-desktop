//! YouTube playback: one player session (`player.js` on its own engine over a native MSE host),
//! the play queue, and the state the UI renders.

use std::sync::Arc;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::*;
use serde_json::Value;
use ym_engine::player::LoadRequest;
use ym_engine::yt::YtFeedItem;
use ym_engine::{DynamicEngine, EngineError};
use ym_mse::MseHost;

use crate::feed::error_text;
use crate::graph::{AppGraph, PLAYER_JS, on_core};

const VOLUME_KEY: &str = "playback.volume";

#[derive(Debug, Clone, PartialEq)]
pub enum PlayState {
    Idle,
    Buffering,
    Playing,
    Paused,
    Ended,
    Failed(String),
}

/// The engine closes before its host: JS calls straight into the C host.
struct Session {
    engine: Arc<DynamicEngine>,
    host: Arc<MseHost>,
}

pub struct Playback {
    session: Option<Arc<Session>>,
    booting: bool,
    /// What to load once the session is up.
    pending: Option<String>,
    pub queue: Vec<YtFeedItem>,
    pub index: usize,
    queue_continuation: Option<String>,
    loading_queue: bool,
    pub state: PlayState,
    pub position_ms: i64,
    pub duration_ms: i64,
    loading_video: Option<String>,
    gain: f32,
    pub volume: f32,
    _status: Option<Task<()>>,
}

impl Playback {
    pub fn new(cx: &App) -> Self {
        // Debug runs can be silenced for automated checks.
        let muted = cfg!(debug_assertions) && std::env::var_os("YMUSIC_DEV_MUTE").is_some();
        let saved_volume =
            cx.global::<AppGraph>().settings.string(VOLUME_KEY, "1").ok().and_then(|raw| raw.parse::<f32>().ok());
        Self {
            session: None,
            booting: false,
            pending: None,
            queue: Vec::new(),
            index: 0,
            queue_continuation: None,
            loading_queue: false,
            state: PlayState::Idle,
            position_ms: 0,
            duration_ms: 0,
            loading_video: None,
            gain: 1.0,
            volume: if muted { 0.0 } else { saved_volume.unwrap_or(1.0).clamp(0.0, 1.0) },
            _status: None,
        }
    }

    pub fn current(&self) -> Option<&YtFeedItem> {
        self.queue.get(self.index)
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.state, PlayState::Playing | PlayState::Buffering)
    }

    /// Plays `item`. A `section` queue plays in order; without one the engine's radio for the
    /// video becomes the queue.
    pub fn play(&mut self, item: YtFeedItem, section: Option<Vec<YtFeedItem>>, cx: &mut Context<Self>) {
        match section {
            Some(items) => {
                self.index = items.iter().position(|candidate| candidate.id == item.id).unwrap_or(0);
                self.queue = items;
                self.queue_continuation = None;
            }
            None => {
                self.queue = vec![item.clone()];
                self.index = 0;
                self.queue_continuation = None;
                self.fetch_radio(item.id.clone(), cx);
            }
        }
        self.load_current(cx);
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        let playing = self.is_playing();
        self.command(cx, move |engine, log| async move {
            if playing { engine.player_pause(&log).await } else { engine.player_play(&log).await }
        });
    }

    pub fn seek(&mut self, position_ms: i64, cx: &mut Context<Self>) {
        self.position_ms = position_ms;
        self.command(cx, move |engine, log| async move { engine.player_seek(position_ms, &log).await });
        cx.notify();
    }

    pub fn next(&mut self, cx: &mut Context<Self>) {
        if self.index + 1 < self.queue.len() {
            self.index += 1;
            self.load_current(cx);
        }
        self.extend_queue_if_near_end(cx);
    }

    pub fn jump_to(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.queue.len() && index != self.index {
            self.index = index;
            self.load_current(cx);
            self.extend_queue_if_near_end(cx);
        }
    }

    pub fn previous(&mut self, cx: &mut Context<Self>) {
        if self.position_ms > 3_000 || self.index == 0 {
            self.seek(0, cx);
        } else {
            self.index -= 1;
            self.load_current(cx);
        }
    }

    pub fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume = volume.clamp(0.0, 1.0);
        self.apply_volume();
        let settings = cx.global::<AppGraph>().settings.clone();
        let raw = format!("{:.2}", self.volume);
        let saved =
            on_core(cx, async move { tokio::task::spawn_blocking(move || settings.put_raw(VOLUME_KEY, raw)).await });
        cx.background_executor().spawn(saved).detach();
        cx.notify();
    }

    fn apply_volume(&self) {
        if let Some(session) = &self.session {
            session.host.set_volume(self.volume * self.gain);
        }
    }

    fn load_current(&mut self, cx: &mut Context<Self>) {
        let Some(video_id) = self.current().map(|item| item.id.clone()) else { return };
        self.state = PlayState::Buffering;
        self.position_ms = 0;
        self.duration_ms = self.current().and_then(|item| item.duration_ms).unwrap_or(0);
        self.loading_video = Some(video_id.clone());
        cx.notify();
        if self.session.is_none() {
            self.pending = Some(video_id);
            self.boot(cx);
            return;
        }
        self.command(cx, move |engine, log| async move {
            engine.player_load(&LoadRequest::audio(&video_id), &log).await.map(|_| ())
        });
    }

    fn command<F, Fut>(&mut self, cx: &mut Context<Self>, run: F)
    where
        F: FnOnce(Arc<DynamicEngine>, ym_common::log::Logger) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), EngineError>> + Send + 'static,
    {
        let Some(session) = self.session.clone() else { return };
        let log = cx.global::<AppGraph>().log.fork("playback");
        let done = on_core(cx, run(session.engine.clone(), log));
        cx.spawn(async move |this, cx| {
            if let Some(Err(error)) = done.await {
                this.update(cx, |playback, cx| {
                    playback.state = PlayState::Failed(error_text(&error));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn boot(&mut self, cx: &mut Context<Self>) {
        if self.booting {
            return;
        }
        self.booting = true;
        let graph = cx.global::<AppGraph>();
        let mut config = graph.engine_config("player");
        config.main = Some(graph.engine.runtime().downgrade());
        let log = graph.log.fork("playback");
        let boot = on_core(cx, async move {
            let host =
                Arc::new(MseHost::create().ok_or_else(|| EngineError::site("R90002", "mse_host_create failed"))?);
            config.mse = Some(host.attach_pointers());
            let mse_log = log.fork("mse");
            ym_mse::set_log_sink(move |level, message| {
                let level = if level >= 2 { ym_common::log::Level::Warn } else { ym_common::log::Level::Info };
                mse_log.log(level, message, None);
            });
            let engine = Arc::new(DynamicEngine::create(PLAYER_JS.to_owned(), config).await?);
            let weak = engine.runtime().downgrade();
            ym_mse::set_event_sink(move |name| {
                if let Some(runtime) = weak.upgrade() {
                    runtime.fire_event(name);
                }
            });
            engine.health_check().await?;
            engine.player_init(None, false, &log).await?;
            Ok::<_, EngineError>(Session { engine, host })
        });
        cx.spawn(async move |this, cx| {
            let result = boot.await;
            this.update(cx, |playback, cx| {
                playback.booting = false;
                match result {
                    Some(Ok(session)) => {
                        playback.session = Some(Arc::new(session));
                        playback.apply_volume();
                        playback.listen(cx);
                        if let Some(video_id) = playback.pending.take() {
                            playback.loading_video = Some(video_id.clone());
                            playback.command(cx, move |engine, log| async move {
                                engine.player_load(&LoadRequest::audio(&video_id), &log).await.map(|_| ())
                            });
                        }
                    }
                    Some(Err(error)) => playback.state = PlayState::Failed(error_text(&error)),
                    None => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn listen(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else { return };
        let (sender, mut receiver) = mpsc::unbounded::<Value>();
        let engine = session.engine.clone();
        let stream = on_core(cx, async move {
            engine
                .listen_player("listen/player/status", move |value| {
                    let _ = sender.unbounded_send(value);
                })
                .await
        });
        cx.background_executor().spawn(stream).detach();
        self._status = Some(cx.spawn(async move |this, cx| {
            while let Some(value) = receiver.next().await {
                if this.update(cx, |playback, cx| playback.apply_status(&value, cx)).is_err() {
                    return;
                }
            }
        }));
    }

    fn apply_status(&mut self, status: &Value, cx: &mut Context<Self>) {
        let current = self.current().map(|item| item.id.clone());
        if let Some(progress) = status.get("progress") {
            let video_id = progress.get("videoId").and_then(Value::as_str).map(str::to_owned);
            if video_id.is_some() && video_id == current {
                self.loading_video = None;
                self.position_ms = field_i64(progress, &["progress", "positionMs"]);
                let duration = field_i64(progress, &["duration", "durationMs"]);
                if duration > 0 {
                    self.duration_ms = duration;
                }
            }
        }
        if let Some(state) = status.get("state") {
            let play_when_ready = state.get("playWhenReady").and_then(Value::as_bool).unwrap_or(false);
            self.state = match state.get("newState").and_then(Value::as_str) {
                Some("buffering") => PlayState::Buffering,
                Some("ready") if play_when_ready => PlayState::Playing,
                Some("ready") => PlayState::Paused,
                Some("ended") => PlayState::Ended,
                Some("idle") if self.loading_video.is_some() => PlayState::Buffering,
                Some("idle") => PlayState::Idle,
                Some("error") => PlayState::Failed(
                    state
                        .get("userMessage")
                        .and_then(Value::as_str)
                        .filter(|message| !message.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            let code = state.get("code").and_then(Value::as_str).unwrap_or("R90003");
                            ym_i18n::tf(ym_i18n::Str::ErrorGeneric, &[&code])
                        }),
                ),
                _ => self.state.clone(),
            };
            if self.state == PlayState::Ended {
                self.next(cx);
            }
        }
        if let Some(gain) = status.get("gain").and_then(|gain| gain.get("value")).and_then(Value::as_f64) {
            self.gain = (gain as f32).clamp(0.0, 1.0);
            self.apply_volume();
        }
        cx.notify();
    }

    fn fetch_radio(&mut self, video_id: String, cx: &mut Context<Self>) {
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine.clone(), graph.log.fork("queue"));
        self.loading_queue = true;
        let fetch = on_core(cx, async move { engine.start_radio_queue(&video_id, &log).await });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |playback, cx| {
                playback.loading_queue = false;
                if let Some(Ok(page)) = result {
                    let current = playback.current().map(|item| item.id.clone());
                    let items =
                        page.sections.into_iter().flat_map(|section| section.items).filter(|item| item.kind == "video");
                    playback.queue.extend(items.filter(|item| Some(&item.id) != current.as_ref()));
                    playback.queue_continuation = page.continuation;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn extend_queue_if_near_end(&mut self, cx: &mut Context<Self>) {
        if self.loading_queue || self.index + 3 < self.queue.len() {
            return;
        }
        let Some(continuation) = self.queue_continuation.take() else { return };
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine.clone(), graph.log.fork("queue"));
        self.loading_queue = true;
        let fetch = on_core(cx, async move { engine.queue_continuation(&continuation, &log).await });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |playback, cx| {
                playback.loading_queue = false;
                if let Some(Ok(page)) = result {
                    playback.queue.extend(
                        page.sections.into_iter().flat_map(|section| section.items).filter(|item| item.kind == "video"),
                    );
                    playback.queue_continuation = page.continuation;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

fn field_i64(object: &Value, names: &[&str]) -> i64 {
    names.iter().find_map(|name| object.get(*name).and_then(Value::as_i64)).unwrap_or(0)
}
