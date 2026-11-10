//! The app's long-lived services, shared by every view through a gpui global.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{App, Global};
use tokio::runtime::Handle;
use ym_common::http::HttpClients;
use ym_common::log::Logger;
use ym_engine::{DiskCacheStore, DynamicEngine, EngineConfig, EngineError, MemoryCookieJar};
use ym_settings::MemorySettings;

/// The bundle every build embeds; the remote-update seam replaces it at runtime later.
const ENGINE_JS: &str = include_str!(concat!(env!("YMUSIC_V4_DIR"), "/engine-js/build/dist/engine.js"));
/// The desktop versionCode sequence the bundle gates compatibility on.
const APP_VERSION: i32 = 96;

pub struct AppGraph {
    pub engine: Arc<DynamicEngine>,
    pub tokio: Handle,
    pub log: Logger,
}

impl Global for AppGraph {}

impl AppGraph {
    pub async fn boot(tokio: Handle, log: Logger, http: HttpClients, data_dir: PathBuf) -> Result<Self, EngineError> {
        let cache_dir = data_dir.join("engine-cache");
        let cache = tokio::task::spawn_blocking({
            let log = log.clone();
            move || DiskCacheStore::open(&cache_dir.join("index.db"), cache_dir.join("blobs"), log)
        })
        .await
        .map_err(|error| EngineError::site("R90001", format!("engine cache: {error}")))?
        .map_err(|error| EngineError::site("R90001", format!("engine cache: {error}")))?;
        let engine = DynamicEngine::create(
            ENGINE_JS.to_owned(),
            EngineConfig {
                app_version: APP_VERSION,
                http,
                logger: log.fork("engine"),
                cookies: Some(Arc::new(MemoryCookieJar::default())),
                settings: Some(Arc::new(MemorySettings::default())),
                remote_features: Some(Arc::new(|_| None)),
                cache: Some(cache),
                mse: None,
                main: None,
            },
        )
        .await?;
        engine.health_check().await?;
        Ok(Self { engine: Arc::new(engine), tokio, log })
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
