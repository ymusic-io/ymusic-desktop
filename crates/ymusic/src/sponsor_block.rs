//! SponsorBlock: arms the current item's skip spans, keyed by its YouTube id so downloaded files
//! skip too. Off when the setting is, or for a whitelisted video. A cache younger than 6 minutes is
//! reused; a failed fetch falls back to a stale cache.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;
use ym_common::log::{Logger, now_millis};
use ym_history::sponsor::{Segment, SponsorBlockStore};
use ym_remote_config::RemoteConfig;
use ym_settings::SettingsStore;

use crate::graph::{AppGraph, on_core};
use crate::playback::{Playback, QueueItem};

pub const SPONSOR_BLOCK_KEY: &str = "playback.sponsorBlock";
const CACHE_TTL: Duration = Duration::from_secs(6 * 60);

pub struct SponsorBlock {
    playback: Entity<Playback>,
    armed_for: Option<String>,
    _observe: Subscription,
}

impl SponsorBlock {
    pub fn new(playback: Entity<Playback>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&playback, |sponsor, playback, cx| sponsor.observe(&playback, cx));
        Self { playback, armed_for: None, _observe: observe }
    }

    fn observe(&mut self, playback: &Entity<Playback>, cx: &mut Context<Self>) {
        let Some(item) = playback.read(cx).current().cloned() else { return };
        let key = item.key();
        if self.armed_for.as_deref() == Some(key.as_str()) {
            return;
        }
        self.armed_for = Some(key.clone());
        let Some(video_id) = youtube_id(&item) else { return };
        let graph = cx.global::<AppGraph>();
        let (store, remote, settings, log) = (
            graph.sponsor_block.clone(),
            graph.remote.clone(),
            graph.settings.clone(),
            graph.log.fork("sponsor-block"),
        );
        let load = on_core(cx, async move { segments_for(&video_id, store, remote, settings, &log).await });
        let target = self.playback.downgrade();
        cx.spawn(async move |_, cx| {
            let Some(segments) = load.await else { return };
            let spans = segments
                .iter()
                .map(|segment| ((segment.start_time * 1000.0) as i64, (segment.end_time * 1000.0) as i64))
                .collect();
            target.update(cx, |playback, _| playback.set_skip_segments(key, spans)).ok();
        })
        .detach();
    }
}

fn youtube_id(item: &QueueItem) -> Option<String> {
    item.video_id().map(str::to_owned).or_else(|| item.youtube_vid.clone())
}

/// The spans to skip; empty when off, whitelisted, or unknown.
async fn segments_for(
    video_id: &str,
    store: Arc<SponsorBlockStore>,
    remote: Arc<RemoteConfig>,
    settings: Arc<dyn SettingsStore>,
    log: &Logger,
) -> Vec<Segment> {
    let enabled = settings.get_raw(SPONSOR_BLOCK_KEY).ok().flatten().is_none_or(|value| value != "false");
    if !enabled {
        return Vec::new();
    }
    let id = video_id.to_owned();
    let local = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || {
            Ok::<_, ym_history::StoreError>((store.is_whitelisted(&id)?, store.cached(&id)?))
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result.map_err(|error| error.to_string()))
    };
    let cached = match local {
        Ok((true, _)) => return Vec::new(),
        Ok((false, cached)) => cached,
        Err(message) => {
            log.warn(&format!("sponsorblock cache read failed: {message}"), None);
            None
        }
    };
    if let Some(cached) = &cached
        && now_millis() - cached.updated_at < CACHE_TTL.as_millis() as i64
    {
        return cached.segments.clone();
    }
    match remote.skip_segments(video_id).await {
        Ok(fetched) => {
            let segments: Vec<Segment> = fetched
                .into_iter()
                .map(|segment| Segment {
                    start_time: segment.start_time,
                    end_time: segment.end_time,
                    category: segment.category,
                    video_duration: segment.video_duration,
                })
                .collect();
            let (id, rows, write_log) = (video_id.to_owned(), segments.clone(), log.clone());
            tokio::task::spawn_blocking(move || {
                if let Err(error) = store.cache(&id, &rows) {
                    write_log.warn("sponsorblock cache write failed", Some(&error));
                }
            });
            segments
        }
        // A build without the config service has no SponsorBlock source.
        Err(ym_remote_config::ServiceError::NoService) => cached.map(|cached| cached.segments).unwrap_or_default(),
        Err(error) => {
            log.warn("sponsorblock segments unavailable", Some(&error));
            cached.map(|cached| cached.segments).unwrap_or_default()
        }
    }
}
