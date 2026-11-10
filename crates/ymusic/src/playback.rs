//! Playback: the play queue and the state the UI renders, over two engines: YouTube (`player.js`
//! on its own engine over a native MSE host) and local files (`file_av`). The video presenter
//! is bound to whichever engine plays the current item.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::*;
use serde_json::Value;
use ym_engine::player::LoadRequest;
use ym_engine::yt::{YtFeedItem, YtThumb};
use ym_engine::{DynamicEngine, EngineError};
use ym_i18n::{Str, tf};
use ym_mse::{MseHost, Presenter};

use crate::feed::error_text;
use crate::graph::{AppGraph, on_core};
use crate::local_engine::{LocalCommand, LocalEngine};

const VOLUME_KEY: &str = "playback.volume";
const LOCAL_POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq)]
pub enum PlayState {
    Idle,
    Buffering,
    Playing,
    Paused,
    Ended,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    YouTube(String),
    Local(PathBuf),
}

/// One entry of the play queue, whatever plays it.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueItem {
    pub source: Source,
    pub title: String,
    pub artist: Option<String>,
    pub duration_ms: Option<i64>,
    pub duration_label: Option<String>,
    pub thumbs: Vec<YtThumb>,
}

impl QueueItem {
    pub fn from_feed(item: &YtFeedItem) -> Self {
        Self {
            source: Source::YouTube(item.id.clone()),
            title: item.title.clone(),
            artist: item.channel_title.clone().or_else(|| item.subtitle_line1.clone()),
            duration_ms: item.duration_ms,
            duration_label: item.duration.clone(),
            thumbs: item.thumbs.clone(),
        }
    }

    pub fn from_local(item: &ym_store::MediaItem) -> Self {
        Self {
            source: Source::Local(PathBuf::from(&item.uri)),
            title: item.title.clone(),
            artist: item.artist.clone(),
            duration_ms: item.duration_ms,
            duration_label: item.duration_ms.map(crate::mini_player::clock),
            thumbs: Vec::new(),
        }
    }

    /// The thumbnail at least `width` px wide, else the largest.
    pub fn thumb(&self, width: i32) -> Option<SharedString> {
        self.thumbs.iter().find(|thumb| thumb.w.is_some_and(|w| w >= width)).or(self.thumbs.last()).map(|thumb| {
            if thumb.url.starts_with("//") { format!("https:{}", thumb.url) } else { thumb.url.clone() }.into()
        })
    }

    pub fn key(&self) -> String {
        match &self.source {
            Source::YouTube(video_id) => video_id.clone(),
            Source::Local(path) => path.to_string_lossy().into_owned(),
        }
    }

    fn video_id(&self) -> Option<&str> {
        match &self.source {
            Source::YouTube(video_id) => Some(video_id),
            Source::Local(_) => None,
        }
    }
}

/// The YouTube engine closes before its host: JS calls straight into the C host.
struct Session {
    engine: Arc<DynamicEngine>,
    host: Arc<MseHost>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Active {
    YouTube,
    Local,
}

pub struct Playback {
    session: Option<Arc<Session>>,
    booting: bool,
    /// What to load once the YouTube session is up.
    pending: Option<String>,
    local: Option<LocalEngine>,
    local_generation: u64,
    active: Option<Active>,
    pub queue: Vec<QueueItem>,
    pub index: usize,
    queue_continuation: Option<String>,
    loading_queue: bool,
    pub state: PlayState,
    pub position_ms: i64,
    pub duration_ms: i64,
    loading_video: Option<String>,
    gain: f32,
    pub volume: f32,
    /// Video on: loads request pictures and the presenter shows them; off: audio only.
    pub video: bool,
    /// Presents into the Now Playing video layer, which outlives it.
    presenter: Option<Arc<Presenter>>,
    _status: Option<Task<()>>,
    _local_poll: Option<Task<()>>,
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
            local: None,
            local_generation: 0,
            active: None,
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
            video: true,
            presenter: None,
            _status: None,
            _local_poll: None,
        }
    }

    pub fn current(&self) -> Option<&QueueItem> {
        self.queue.get(self.index)
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.state, PlayState::Playing | PlayState::Buffering)
    }

    /// Plays a feed video. A `section` queue plays in order; without one the engine's radio for
    /// the video becomes the queue.
    pub fn play_feed(&mut self, item: YtFeedItem, section: Option<Vec<YtFeedItem>>, cx: &mut Context<Self>) {
        self.queue_continuation = None;
        match section {
            Some(items) => {
                self.index = items.iter().position(|candidate| candidate.id == item.id).unwrap_or(0);
                self.queue = items.iter().map(QueueItem::from_feed).collect();
            }
            None => {
                self.queue = vec![QueueItem::from_feed(&item)];
                self.index = 0;
                self.fetch_radio(item.id.clone(), cx);
            }
        }
        self.load_current(cx);
    }

    /// Plays `items` in order from `index`.
    pub fn play_queue(&mut self, items: Vec<QueueItem>, index: usize, cx: &mut Context<Self>) {
        self.queue_continuation = None;
        self.index = index.min(items.len().saturating_sub(1));
        self.queue = items;
        self.load_current(cx);
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        let playing = self.is_playing();
        match self.active {
            Some(Active::Local) => {
                self.send_local(if playing { LocalCommand::Pause } else { LocalCommand::Play });
                self.state = if playing { PlayState::Paused } else { PlayState::Playing };
                cx.notify();
            }
            _ => self.command(cx, move |engine, log| async move {
                if playing { engine.player_pause(&log).await } else { engine.player_play(&log).await }
            }),
        }
    }

    pub fn seek(&mut self, position_ms: i64, cx: &mut Context<Self>) {
        self.position_ms = position_ms;
        match self.active {
            Some(Active::Local) => self.send_local(LocalCommand::Seek(position_ms)),
            _ => self.command(cx, move |engine, log| async move { engine.player_seek(position_ms, &log).await }),
        }
        cx.notify();
    }

    pub fn next(&mut self, cx: &mut Context<Self>) {
        if self.index + 1 < self.queue.len() {
            self.index += 1;
            self.load_current(cx);
        }
        self.extend_queue_if_near_end(cx);
    }

    pub fn previous(&mut self, cx: &mut Context<Self>) {
        if self.position_ms > 3_000 || self.index == 0 {
            self.seek(0, cx);
        } else {
            self.index -= 1;
            self.load_current(cx);
        }
    }

    pub fn jump_to(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.queue.len() && index != self.index {
            self.index = index;
            self.load_current(cx);
            self.extend_queue_if_near_end(cx);
        }
    }

    /// Hands over the video layer once a window has one.
    ///
    /// # Safety
    /// `layer` must be a live `AVSampleBufferDisplayLayer` that outlives this `Playback`.
    pub unsafe fn attach_video_layer(&mut self, layer: *mut std::ffi::c_void) {
        if self.presenter.is_some() {
            return;
        }
        self.presenter = unsafe { Presenter::on_layer(layer) }.map(Arc::new);
        self.bind_presenter();
    }

    /// Pictures go to the engine playing the current item; the other one is unbound.
    fn bind_presenter(&self) {
        let Some(presenter) = &self.presenter else { return };
        let local = self.active == Some(Active::Local);
        if let Some(session) = &self.session {
            session.host.bind_presenter(if local { None } else { Some(presenter) });
        }
        if local {
            self.send_local(LocalCommand::Presenter(Some(presenter.clone())));
        } else {
            self.send_local(LocalCommand::Presenter(None));
        }
    }

    pub fn set_video(&mut self, video: bool, cx: &mut Context<Self>) {
        if self.video == video {
            return;
        }
        self.video = video;
        self.send_local(LocalCommand::Video(video));
        if self.active == Some(Active::YouTube) {
            let (quality, audio_only) = if video { ("auto", false) } else { ("zero", true) };
            self.command(
                cx,
                move |engine, log| async move { engine.player_set_quality(quality, audio_only, &log).await },
            );
        }
        cx.notify();
    }

    /// The video box in CSS pixels; YouTube's ABR picks the quality for it.
    pub fn set_viewport(&mut self, width: i32, height: i32, cx: &mut Context<Self>) {
        self.command(cx, move |engine, log| async move { engine.player_set_viewport(width, height, &log).await });
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
        self.send_local(LocalCommand::Volume(self.volume));
    }

    fn send_local(&self, command: LocalCommand) {
        if let Some(local) = &self.local {
            local.send(command);
        }
    }

    fn load_request(&self, video_id: &str) -> LoadRequest {
        let mut request = LoadRequest::audio(video_id);
        if self.video {
            request.quality = Some("auto".into());
            request.audio_only = false;
        }
        request
    }

    fn load_current(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.current().cloned() else { return };
        self.state = PlayState::Buffering;
        self.position_ms = 0;
        self.duration_ms = item.duration_ms.unwrap_or(0);
        cx.notify();
        match item.source {
            Source::YouTube(video_id) => {
                if self.active == Some(Active::Local) {
                    self.send_local(LocalCommand::Stop);
                }
                self.active = Some(Active::YouTube);
                self.bind_presenter();
                self.loading_video = Some(video_id.clone());
                if self.session.is_none() {
                    self.pending = Some(video_id);
                    self.boot(cx);
                    return;
                }
                let request = self.load_request(&video_id);
                self.command(
                    cx,
                    move |engine, log| async move { engine.player_load(&request, &log).await.map(|_| ()) },
                );
            }
            Source::Local(path) => {
                if self.active == Some(Active::YouTube) {
                    self.command(cx, |engine, log| async move { engine.player_stop(&log).await });
                }
                if self.local.is_none() {
                    self.local = LocalEngine::start();
                    self.apply_volume();
                }
                if self.local.is_none() {
                    self.state = PlayState::Failed(tf(Str::ErrorGeneric, &[&"R90007"]));
                    return;
                }
                self.active = Some(Active::Local);
                self.loading_video = None;
                self.local_generation += 1;
                self.bind_presenter();
                self.send_local(LocalCommand::Load { path, video: self.video });
                self.poll_local(cx);
            }
        }
    }

    /// Follows the local player's status while it is the active engine.
    fn poll_local(&mut self, cx: &mut Context<Self>) {
        let generation = self.local_generation;
        let executor = cx.background_executor().clone();
        self._local_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                executor.timer(LOCAL_POLL).await;
                let keep = this.update(cx, |playback, cx| playback.apply_local_status(generation, cx));
                if !matches!(keep, Ok(true)) {
                    return;
                }
            }
        }));
    }

    /// `false` stops the poll.
    fn apply_local_status(&mut self, generation: u64, cx: &mut Context<Self>) -> bool {
        if self.active != Some(Active::Local) || generation != self.local_generation {
            return false;
        }
        let Some(local) = &self.local else { return false };
        let status = local.status.lock().unwrap().clone();
        self.position_ms = status.position_ms;
        if status.duration_ms > 0 {
            self.duration_ms = status.duration_ms;
        }
        if let Some(message) = status.error {
            let log = cx.global::<AppGraph>().log.fork("playback");
            log.warn(&format!("R90007 local file failed: {message}"), None);
            self.state = PlayState::Failed(tf(Str::ErrorGeneric, &[&"R90007"]));
            cx.notify();
            return false;
        }
        if status.ended {
            self.state = PlayState::Ended;
            self.next(cx);
            cx.notify();
            return false;
        }
        self.state = if status.playing { PlayState::Playing } else { PlayState::Paused };
        cx.notify();
        true
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
        config.main = Some(graph.main_resolver());
        let candidates = graph.player_candidates();
        let player_source = graph.player_source.clone();
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
            // A published bundle that will not boot must not take playback down: the next one boots.
            let mut last_error = None;
            for (script, staged) in candidates {
                let engine = match DynamicEngine::create(script, config.clone()).await {
                    Ok(engine) => Arc::new(engine),
                    Err(error) => {
                        last_error = Some(error);
                        continue;
                    }
                };
                if let Err(error) = engine.health_check().await {
                    engine.close().await;
                    last_error = Some(error);
                    continue;
                }
                if let Some(bundle) = staged {
                    let source = player_source.clone();
                    let _ = tokio::task::spawn_blocking(move || source.store(&bundle)).await;
                }
                let weak = engine.runtime().downgrade();
                ym_mse::set_event_sink(move |name| {
                    if let Some(runtime) = weak.upgrade() {
                        runtime.fire_event(name);
                    }
                });
                engine.player_init(None, false, &log).await?;
                return Ok::<_, EngineError>(Session { engine, host });
            }
            Err(last_error.unwrap_or_else(|| EngineError::site("R90002", "no player bundle booted")))
        });
        cx.spawn(async move |this, cx| {
            let result = boot.await;
            this.update(cx, |playback, cx| {
                playback.booting = false;
                match result {
                    Some(Ok(session)) => {
                        playback.session = Some(Arc::new(session));
                        playback.apply_volume();
                        playback.bind_presenter();
                        playback.listen(cx);
                        if let Some(video_id) = playback.pending.take()
                            && playback.active == Some(Active::YouTube)
                        {
                            playback.loading_video = Some(video_id.clone());
                            let request = playback.load_request(&video_id);
                            playback.command(cx, move |engine, log| async move {
                                engine.player_load(&request, &log).await.map(|_| ())
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
        if self.active != Some(Active::YouTube) {
            return;
        }
        let current = self.current().and_then(QueueItem::video_id).map(str::to_owned);
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
                            tf(Str::ErrorGeneric, &[&code])
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
        let (engine, log) = (graph.engine(), graph.log.fork("queue"));
        self.loading_queue = true;
        let fetch = on_core(cx, async move { engine.start_radio_queue(&video_id, &log).await });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |playback, cx| {
                playback.loading_queue = false;
                if let Some(Ok(page)) = result {
                    let current = playback.current().map(QueueItem::key);
                    let items = page
                        .sections
                        .iter()
                        .flat_map(|section| section.items.iter())
                        .filter(|item| item.kind == "video" && Some(&item.id) != current.as_ref())
                        .map(QueueItem::from_feed);
                    playback.queue.extend(items);
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
        let (engine, log) = (graph.engine(), graph.log.fork("queue"));
        self.loading_queue = true;
        let fetch = on_core(cx, async move { engine.queue_continuation(&continuation, &log).await });
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |playback, cx| {
                playback.loading_queue = false;
                if let Some(Ok(page)) = result {
                    let items = page
                        .sections
                        .iter()
                        .flat_map(|section| section.items.iter())
                        .filter(|item| item.kind == "video")
                        .map(QueueItem::from_feed);
                    playback.queue.extend(items);
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
