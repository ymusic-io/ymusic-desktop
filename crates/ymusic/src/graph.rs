//! The app's long-lived services, shared by every view through a gpui global.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{App, Global};
use tokio::runtime::Handle;
use ym_common::http::HttpClients;
use ym_common::log::Logger;
use ym_engine::{CookieJar, DiskCacheStore, DynamicEngine, EngineConfig, EngineError, MemoryCookieJar};
use ym_settings::{MemorySettings, SettingsStore};

/// The bundles every build embeds; the remote-update seam replaces them at runtime later.
const ENGINE_JS: &str = include_str!(concat!(env!("YMUSIC_V4_DIR"), "/engine-js/build/dist/engine.js"));
pub const PLAYER_JS: &str = include_str!(concat!(env!("YMUSIC_V4_DIR"), "/engine-js/build/dist/player.js"));
/// The desktop versionCode sequence the bundles gate compatibility on.
pub const APP_VERSION: i32 = 96;

pub struct AppGraph {
    pub engine: Arc<DynamicEngine>,
    pub tokio: Handle,
    pub log: Logger,
    pub http: HttpClients,
    /// One identity for scraping and playback.
    pub cookies: Arc<dyn CookieJar>,
    pub settings: Arc<dyn SettingsStore>,
    pub cache: Arc<DiskCacheStore>,
}

impl Global for AppGraph {}

impl AppGraph {
    pub async fn boot(tokio: Handle, log: Logger, http: HttpClients, data_dir: PathBuf) -> Result<Self, EngineError> {
        let cache_dir = data_dir.join("engine-cache");
        let open_failed = |error: String| EngineError::site("R90001", format!("engine cache: {error}"));
        let cache = tokio::task::spawn_blocking({
            let log = log.clone();
            move || DiskCacheStore::open(&cache_dir.join("index.db"), cache_dir.join("blobs"), log)
        })
        .await
        .map_err(|error| open_failed(error.to_string()))?
        .map_err(|error| open_failed(error.to_string()))?;
        let cookies: Arc<dyn CookieJar> = Arc::new(MemoryCookieJar::default());
        let settings: Arc<dyn SettingsStore> = Arc::new(MemorySettings::default());
        let graph = Self {
            engine: Arc::new(
                DynamicEngine::create(
                    ENGINE_JS.to_owned(),
                    Self::config(&http, &log, &cookies, &settings, &cache, "engine"),
                )
                .await?,
            ),
            tokio,
            log,
            http,
            cookies,
            settings,
            cache,
        };
        graph.engine.health_check().await?;
        Ok(graph)
    }

    /// An engine config sharing this graph's HTTP, cookies, settings and cache.
    pub fn engine_config(&self, tag: &str) -> EngineConfig {
        Self::config(&self.http, &self.log, &self.cookies, &self.settings, &self.cache, tag)
    }

    fn config(
        http: &HttpClients,
        log: &Logger,
        cookies: &Arc<dyn CookieJar>,
        settings: &Arc<dyn SettingsStore>,
        cache: &Arc<DiskCacheStore>,
        tag: &str,
    ) -> EngineConfig {
        EngineConfig {
            app_version: APP_VERSION,
            http: http.clone(),
            logger: log.fork(tag),
            cookies: Some(cookies.clone()),
            settings: Some(settings.clone()),
            remote_features: Some(Arc::new(|_| None)),
            cache: Some(cache.clone()),
            mse: None,
            main: None,
        }
    }
}

/// Runs a core future on the tokio runtime and resolves on any executor (gpui's included).
/// `None` when the runtime is shutting down.
pub fn on_core<F>(cx: &App, future: F) -> impl Future<Output = Option<F::Output>> + use<F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handle = cx.global::<AppGraph>().tokio.spawn(future);
    async move { handle.await.ok() }
}
