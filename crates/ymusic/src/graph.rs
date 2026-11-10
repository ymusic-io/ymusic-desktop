//! The app's long-lived services, shared by every view through a gpui global.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{App, Global};
use tokio::runtime::Handle;
use ym_common::http::HttpClients;
use ym_common::log::Logger;
use ym_engine::{CookieJar, DiskCacheStore, DynamicEngine, EngineConfig, EngineError, PersistentCookieJar};
use ym_settings::{AppDb, SettingsStore};
use ym_store::MediaStore;

const COOKIES_SECRET: &str = "cookies";

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
    pub media: Arc<MediaStore>,
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
        let app_db_path = data_dir.join("app.db");
        let app_db = tokio::task::spawn_blocking(move || AppDb::open(&app_db_path))
            .await
            .map_err(|error| EngineError::site("R90005", format!("app.db: {error}")))?
            .map_err(|error| EngineError::site("R90005", format!("app.db: {error}")))?;
        let app_db = Arc::new(app_db);
        let saved_cookies = app_db.secret(COOKIES_SECRET).ok().flatten();
        let cookie_db = app_db.clone();
        let cookies: Arc<dyn CookieJar> = Arc::new(PersistentCookieJar::new(
            saved_cookies.as_deref(),
            Box::new(move |bytes| Ok(cookie_db.put_secret(COOKIES_SECRET, bytes)?)),
        ));
        let settings: Arc<dyn SettingsStore> = app_db;
        let media_path = data_dir.join("media_store.db");
        let media = tokio::task::spawn_blocking(move || MediaStore::open(&media_path))
            .await
            .map_err(|error| EngineError::site("R90010", format!("media store: {error}")))?
            .map_err(|error| EngineError::site("R90010", format!("media store: {error}")))?;
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
            media: Arc::new(media),
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
