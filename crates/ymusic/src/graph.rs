//! The app's long-lived services, shared by every view through a gpui global.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::{App, Global};
use tokio::runtime::Handle;
use ym_common::http::HttpClients;
use ym_common::log::Logger;
use ym_engine::{
    CookieJar, DiskCacheStore, DynamicEngine, EngineConfig, EngineError, EngineManager, MainResolver,
    PersistentCookieJar,
};
use ym_remote_config::{ApiIdentity, BundleSource, RemoteConfig, VerifiedBundle};
use ym_settings::{AppDb, SettingsStore};
use ym_store::MediaStore;

const ENGINE_JS: &str = include_str!(concat!(env!("YMUSIC_V4_DIR"), "/engine-js/build/dist/engine.js"));
const PLAYER_JS: &str = include_str!(concat!(env!("YMUSIC_V4_DIR"), "/engine-js/build/dist/player.js"));
const ENGINE_JS_VERSION: &str = env!("YM_ENGINE_JS_VERSION");
const PLAYER_JS_VERSION: &str = env!("YM_PLAYER_JS_VERSION");
/// The desktop versionCode sequence the bundles gate compatibility on.
pub const APP_VERSION: i32 = 96;
const APP_VERSION_NAME: &str = env!("CARGO_PKG_VERSION");
const COOKIES_SECRET: &str = "cookies";
/// Shared with the JVM app: the anonymous id the config service counts devices by.
const DEVICE_ID_KEY: &str = "telemetry.deviceId";
/// A bundle in use refreshes a config older than this.
const BUNDLE_CONFIG_MAX_AGE: Duration = Duration::from_secs(10 * 60);

pub struct AppGraph {
    pub engines: Arc<EngineManager>,
    pub tokio: Handle,
    pub log: Logger,
    pub http: HttpClients,
    /// One identity for scraping and playback.
    pub cookies: Arc<dyn CookieJar>,
    pub settings: Arc<dyn SettingsStore>,
    pub cache: Arc<DiskCacheStore>,
    pub media: Arc<MediaStore>,
    pub remote: Arc<RemoteConfig>,
    pub player_source: Arc<BundleSource>,
    /// A newer published player bundle, verified, for the next playback session; cached once it boots.
    pub pending_player: Arc<Mutex<Option<VerifiedBundle>>>,
}

impl Global for AppGraph {}

fn boot_error(code: &'static str, what: &'static str) -> impl Fn(String) -> EngineError {
    move |error| EngineError::site(code, format!("{what}: {error}"))
}

impl AppGraph {
    pub async fn boot(tokio: Handle, log: Logger, http: HttpClients, data_dir: PathBuf) -> Result<Self, EngineError> {
        let cache_dir = data_dir.join("engine-cache");
        let cache_log = log.clone();
        let cache = tokio::task::spawn_blocking(move || {
            DiskCacheStore::open(&cache_dir.join("index.db"), cache_dir.join("blobs"), cache_log)
        })
        .await
        .map_err(|error| boot_error("R90001", "engine cache")(error.to_string()))?
        .map_err(|error| boot_error("R90001", "engine cache")(error.to_string()))?;
        let app_db_path = data_dir.join("app.db");
        let app_db = Arc::new(
            tokio::task::spawn_blocking(move || AppDb::open(&app_db_path))
                .await
                .map_err(|error| boot_error("R90005", "app.db")(error.to_string()))?
                .map_err(|error| boot_error("R90005", "app.db")(error.to_string()))?,
        );
        let saved_cookies = app_db.secret(COOKIES_SECRET).ok().flatten();
        let cookie_db = app_db.clone();
        let cookies: Arc<dyn CookieJar> = Arc::new(PersistentCookieJar::new(
            saved_cookies.as_deref(),
            Box::new(move |bytes| Ok(cookie_db.put_secret(COOKIES_SECRET, bytes)?)),
        ));
        let device_id = match app_db.string(DEVICE_ID_KEY, "").ok().filter(|id| !id.is_empty()) {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                let _ = app_db.put_raw(DEVICE_ID_KEY, id.clone());
                id
            }
        };
        let settings: Arc<dyn SettingsStore> = app_db;
        let media_path = data_dir.join("media_store.db");
        let media = tokio::task::spawn_blocking(move || MediaStore::open(&media_path))
            .await
            .map_err(|error| boot_error("R90010", "media store")(error.to_string()))?
            .map_err(|error| boot_error("R90010", "media store")(error.to_string()))?;

        let identity = option_env!("YMUSIC_REMOTE_CONFIG_URL").map(|base_url| ApiIdentity {
            base_url: base_url.to_owned(),
            api_key: option_env!("YMUSIC_API_KEY").map(str::to_owned),
            os: std::env::consts::OS.to_owned(),
            os_version: String::new(),
            arch: if cfg!(target_arch = "aarch64") { "arm64".into() } else { "x64".into() },
            app_version_code: APP_VERSION,
            app_version_name: APP_VERSION_NAME.to_owned(),
            device_id,
        });
        let remote_file = data_dir.join("remote-config.json");
        let remote_client = http.following.clone();
        let remote_log = log.clone();
        let remote = Arc::new(
            tokio::task::spawn_blocking(move || RemoteConfig::open(identity, remote_client, remote_file, remote_log))
                .await
                .map_err(|error| boot_error("R90011", "remote config")(error.to_string()))?,
        );
        let public_key = option_env!("YMUSIC_BUNDLE_PUBLIC_KEY").unwrap_or_default();
        let source = |name: &str, embedded: &'static str, version: &str| {
            BundleSource::new(
                name,
                public_key,
                (embedded, version.parse().unwrap_or(0)),
                data_dir.join("bundles").join(name),
                http.following.clone(),
                log.clone(),
            )
        };
        let engine_source = Arc::new(source("engine", ENGINE_JS, ENGINE_JS_VERSION));
        let player_source = Arc::new(source("player", PLAYER_JS, PLAYER_JS_VERSION));

        let features_remote = remote.clone();
        let template = EngineConfig {
            app_version: APP_VERSION,
            http: http.clone(),
            logger: log.fork("engine"),
            cookies: Some(cookies.clone()),
            settings: Some(settings.clone()),
            remote_features: Some(Arc::new(move |key| features_remote.current().feature(key).cloned())),
            cache: Some(cache.clone()),
            mse: None,
            main: None,
        };
        let mut candidates = Vec::new();
        let cached_source = engine_source.clone();
        if let Ok(Some((script, version))) = tokio::task::spawn_blocking(move || cached_source.cached()).await {
            candidates.push((script, format!("remote v{version}")));
        }
        candidates.push((ENGINE_JS.to_owned(), format!("embedded v{ENGINE_JS_VERSION}")));
        let engines = Arc::new(EngineManager::start(candidates, template).await?);
        remote.set_running_bundles(Some(i64::from(engines.backend_version())), None);

        let graph = Self {
            engines,
            tokio,
            log,
            http,
            cookies,
            settings,
            cache,
            media: Arc::new(media),
            remote,
            player_source,
            pending_player: Arc::new(Mutex::new(None)),
        };
        graph.follow_published_bundles(engine_source);
        Ok(graph)
    }

    /// The engine new work goes to; hold it for one job.
    pub fn engine(&self) -> Arc<DynamicEngine> {
        self.engines.current()
    }

    /// Resolves the current main engine at each call (the player's `host/main/*` proxy).
    pub fn main_resolver(&self) -> MainResolver {
        let engines = self.engines.clone();
        Arc::new(move || Some(engines.current().runtime().clone()))
    }

    /// An engine config sharing this graph's HTTP, cookies, settings, cache and remote features.
    pub fn engine_config(&self, tag: &str) -> EngineConfig {
        let remote = self.remote.clone();
        EngineConfig {
            app_version: APP_VERSION,
            http: self.http.clone(),
            logger: self.log.fork(tag),
            cookies: Some(self.cookies.clone()),
            settings: Some(self.settings.clone()),
            remote_features: Some(Arc::new(move |key| remote.current().feature(key).cloned())),
            cache: Some(self.cache.clone()),
            mse: None,
            main: None,
        }
    }

    /// Refreshes the config, then swaps in a newer published engine bundle (cached once it
    /// booted) and stages a newer player bundle for the next playback session.
    fn follow_published_bundles(&self, engine_source: Arc<BundleSource>) {
        let (remote, engines, player_source, pending) =
            (self.remote.clone(), self.engines.clone(), self.player_source.clone(), self.pending_player.clone());
        let log = self.log.fork("bundles");
        self.tokio.spawn(async move {
            let mut published = remote.subscribe();
            remote.check_update(BUNDLE_CONFIG_MAX_AGE).await;
            loop {
                let config = published.borrow_and_update().clone();
                if let Some(remote_engine) =
                    config.engine_js.filter(|bundle| bundle.version > i64::from(engines.backend_version()))
                {
                    match engine_source.download(&remote_engine).await {
                        Ok(bundle) => match engines.swap(bundle.source.clone()).await {
                            Ok(version) => {
                                let source = engine_source.clone();
                                let _ = tokio::task::spawn_blocking(move || source.store(&bundle)).await;
                                remote.set_running_bundles(Some(i64::from(version)), None);
                            }
                            Err(error) => log
                                .warn(&format!("R90012 engine v{} did not boot", remote_engine.version), Some(&error)),
                        },
                        Err(error) => {
                            log.warn(&format!("R90012 engine v{} not taken", remote_engine.version), Some(&error))
                        }
                    }
                }
                let embedded_player = player_source.embedded().1;
                if let Some(remote_player) = config.player_js.filter(|bundle| bundle.version > embedded_player) {
                    match player_source.download(&remote_player).await {
                        Ok(bundle) => *pending.lock().unwrap() = Some(bundle),
                        Err(error) => {
                            log.warn(&format!("R90012 player v{} not taken", remote_player.version), Some(&error))
                        }
                    }
                }
                if published.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    /// The player bundles to try, best first: a staged newer one, the cached one, the embedded one.
    pub fn player_candidates(&self) -> Vec<(String, Option<VerifiedBundle>)> {
        let mut candidates = Vec::new();
        if let Some(bundle) = self.pending_player.lock().unwrap().take() {
            candidates.push((bundle.source.clone(), Some(bundle)));
        }
        if let Some((script, _)) = self.player_source.cached() {
            candidates.push((script, None));
        }
        candidates.push((PLAYER_JS.to_owned(), None));
        candidates
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
