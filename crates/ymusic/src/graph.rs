//! The app's long-lived services, shared by every view through a gpui global.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::{App, Global};
use tokio::runtime::Handle;
use ym_common::http::HttpClients;
use ym_common::log::Logger;
use ym_engine::{
    CookieJar, DiskCacheStore, DynamicEngine, EngineConfig, EngineError, EngineManager, MemoryCookieJar,
    PersistentCookieJar,
};
use ym_remote_config::{ApiIdentity, BundleSource, RemoteConfig, VerifiedBundle};
use ym_settings::{AppDb, SettingsStore};
use ym_signin::CookieVault;
use ym_store::MediaStore;
use ym_download::DownloadService;

use crate::downloads::DownloadInputs;
use crate::youtube_session::{CandidateSource, YoutubeSessions};

const ENGINE_JS: &str = include_str!(concat!(env!("YMUSIC_V4_DIR"), "/engine-js/build/dist/engine.js"));
const PLAYER_JS: &str = include_str!(concat!(env!("YMUSIC_V4_DIR"), "/engine-js/build/dist/player.js"));
const ENGINE_JS_VERSION: &str = env!("YM_ENGINE_JS_VERSION");
const PLAYER_JS_VERSION: &str = env!("YM_PLAYER_JS_VERSION");
/// The desktop versionCode sequence the bundles gate compatibility on.
pub const APP_VERSION: i32 = 96;
const APP_VERSION_NAME: &str = env!("CARGO_PKG_VERSION");
/// The sealed jar's row: the JVM app's name for it.
const COOKIE_JAR_SECRET: &str = "api-cookies.json";
/// Where builds before the vault kept the jar in the clear; removed at boot.
const PLAIN_COOKIES_SECRET: &str = "cookies";
/// This app's own Keychain item for the cookie key.
const KEYCHAIN_SERVICE: &str = "io.ymusic.desktop.rust";
const KEYCHAIN_ACCOUNT: &str = "cookie-key";
/// Debug builds keep the cookie key here instead (the JVM app's Linux fallback name).
const FALLBACK_KEY_SECRET: &str = "cookie-key.bin";
/// Shared with the JVM app: the anonymous id the config service counts devices by.
const DEVICE_ID_KEY: &str = "telemetry.deviceId";
/// A bundle in use refreshes a config older than this.
const BUNDLE_CONFIG_MAX_AGE: Duration = Duration::from_secs(10 * 60);

pub struct AppGraph {
    pub engines: Arc<EngineManager>,
    pub tokio: Handle,
    pub log: Logger,
    /// One identity for scraping and playback.
    pub cookies: Arc<dyn CookieJar>,
    pub settings: Arc<dyn SettingsStore>,
    pub media: Arc<MediaStore>,
    pub remote: Arc<RemoteConfig>,
    pub player_source: Arc<BundleSource>,
    /// A newer published player bundle, verified, for the next playback session; cached once it boots.
    pub pending_player: Arc<Mutex<Option<VerifiedBundle>>>,
    /// The jar is password-sealed and this launch runs on in-memory cookies until it is unlocked.
    pub cookies_locked: bool,
    /// The YouTube player session playback and downloads share.
    pub youtube: Arc<YoutubeSessions>,
    pub downloads: DownloadService,
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
        let (cookies, cookies_locked) = open_cookie_jar(app_db.clone(), &log).await;
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

        let pending_player: Arc<Mutex<Option<VerifiedBundle>>> = Arc::new(Mutex::new(None));
        let media = Arc::new(media);
        let mut player_config = template_for("player", &http, &log, &cookies, &settings, &cache, &remote);
        player_config.main = Some({
            let engines = engines.clone();
            Arc::new(move || Some(engines.current().runtime().clone()))
        });
        let candidates: CandidateSource = {
            let (pending, source) = (pending_player.clone(), player_source.clone());
            Arc::new(move || player_candidates(&pending, &source))
        };
        let youtube =
            Arc::new(YoutubeSessions::new(player_config, candidates, player_source.clone(), log.fork("youtube")));
        let download_inputs = DownloadInputs {
            tokio: tokio.clone(),
            data_dir: data_dir.clone(),
            http: http.clone(),
            cookies: cookies.clone(),
            settings: settings.clone(),
            media: media.clone(),
            sessions: youtube.clone(),
            log: log.clone(),
        };
        let downloads = tokio::task::spawn_blocking(move || crate::downloads::open(download_inputs))
            .await
            .map_err(|error| boot_error("R90015", "downloads")(error.to_string()))?
            .map_err(boot_error("R90015", "downloads"))?;

        let graph = Self {
            engines,
            tokio,
            log,
            cookies,
            settings,
            media,
            remote,
            player_source,
            pending_player,
            cookies_locked,
            youtube,
            downloads,
        };
        graph.follow_published_bundles(engine_source);
        Ok(graph)
    }

    /// The engine new work goes to; hold it for one job.
    pub fn engine(&self) -> Arc<DynamicEngine> {
        self.engines.current()
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

}

/// The player bundles to try, best first: a staged newer one, the cached one, the embedded one.
fn player_candidates(
    pending: &Mutex<Option<VerifiedBundle>>,
    source: &BundleSource,
) -> Vec<(String, Option<VerifiedBundle>)> {
    let mut candidates = Vec::new();
    if let Some(bundle) = pending.lock().unwrap().take() {
        candidates.push((bundle.source.clone(), Some(bundle)));
    }
    if let Some((script, _)) = source.cached() {
        candidates.push((script, None));
    }
    candidates.push((PLAYER_JS.to_owned(), None));
    candidates
}

/// An engine config sharing the app's HTTP, cookies, settings, cache and remote features.
fn template_for(
    tag: &str,
    http: &HttpClients,
    log: &Logger,
    cookies: &Arc<dyn CookieJar>,
    settings: &Arc<dyn SettingsStore>,
    cache: &Arc<DiskCacheStore>,
    remote: &Arc<RemoteConfig>,
) -> EngineConfig {
    let remote = remote.clone();
    EngineConfig {
        app_version: APP_VERSION,
        http: http.clone(),
        logger: log.fork(tag),
        cookies: Some(cookies.clone()),
        settings: Some(settings.clone()),
        remote_features: Some(Arc::new(move |key| remote.current().feature(key).cloned())),
        cache: Some(cache.clone()),
        mse: None,
        main: None,
    }
}

/// The OS user's key in release builds. Debug builds are re-signed by every build, which macOS
/// treats as another app: a Keychain read would stop each launch on an access prompt, so they keep
/// the key in app.db, which only keeps other OS users out.
fn cookie_key(app_db: &AppDb) -> Result<[u8; 32], ym_signin::VaultError> {
    if !cfg!(debug_assertions) {
        return ym_signin::os_cookie_key(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);
    }
    if let Some(key) =
        app_db.secret(FALLBACK_KEY_SECRET).ok().flatten().and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    {
        return Ok(key);
    }
    let key = ym_signin::random_key();
    app_db.put_secret(FALLBACK_KEY_SECRET, &key).map_err(|error| ym_signin::VaultError::KeyStore(error.to_string()))?;
    Ok(key)
}

/// The persistent cookie jar sealed in app.db under the OS user's key. Without that key, or with a
/// password-sealed jar, the launch runs as a guest: cookies in memory, nothing written in the clear.
async fn open_cookie_jar(app_db: Arc<AppDb>, log: &Logger) -> (Arc<dyn CookieJar>, bool) {
    let log = log.fork("cookies");
    let opened = tokio::task::spawn_blocking(move || {
        let _ = app_db.delete_secret(PLAIN_COOKIES_SECRET);
        let key = cookie_key(&app_db)?;
        let sealed = app_db.secret(COOKIE_JAR_SECRET).ok().flatten();
        Ok::<_, ym_signin::VaultError>((app_db, CookieVault::new(key), sealed))
    })
    .await;
    let (app_db, vault, sealed) = match opened {
        Ok(Ok(parts)) => parts,
        Ok(Err(error)) => {
            log.warn("R90013 OS cookie key unavailable, this launch runs as a guest", Some(&error));
            return (Arc::new(MemoryCookieJar::default()), false);
        }
        Err(error) => {
            log.warn("R90013 OS cookie key unavailable, this launch runs as a guest", Some(&error));
            return (Arc::new(MemoryCookieJar::default()), false);
        }
    };
    if sealed.as_deref().is_some_and(CookieVault::is_password_sealed) {
        return (Arc::new(MemoryCookieJar::default()), true);
    }
    let document = match sealed.as_deref().map(|sealed| vault.open(sealed)) {
        Some(Ok(document)) => Some(document),
        Some(Err(error)) => {
            log.warn("R90014 sealed cookie jar unreadable, starting signed out", Some(&error));
            None
        }
        None => None,
    };
    let vault = Arc::new(vault);
    let jar = PersistentCookieJar::new(
        document.as_deref(),
        Box::new(move |bytes| Ok(app_db.put_secret(COOKIE_JAR_SECRET, &vault.seal(bytes))?)),
    );
    (Arc::new(jar), false)
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
