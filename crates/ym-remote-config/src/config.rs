use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use tokio::sync::watch;
use ym_common::log::{Logger, now_millis};

/// Hammering `check_update` from every screen costs at most one request per interval.
const MIN_INTERVAL: Duration = Duration::from_secs(5 * 60);

fn null_default<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(deserializer: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A published JS bundle; the document arrives unsigned, so `signature` authenticates the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundleVersion {
    pub version: i64,
    pub url: String,
    pub signature: String,
}

/// A newer app build for this platform; `signature` covers `app-<platform>:<versionCode>:<sha256>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdate {
    pub version_code: i32,
    pub version_name: String,
    #[serde(default)]
    pub notes: String,
    pub url: String,
    pub size: i64,
    pub sha256: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub changelog_url: Option<String>,
    #[serde(default)]
    pub download_page: Option<String>,
    #[serde(default = "default_true")]
    pub install_in_app: bool,
}

fn default_true() -> bool {
    true
}

/// `GET /v1/config`: every field defaults and unknown fields are skipped, so the service can add a
/// flag without breaking an older client. `features` keeps every key as sent.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemoteConfigData {
    #[serde(deserialize_with = "null_default")]
    pub features: Map<String, Value>,
    #[serde(deserialize_with = "null_default")]
    pub app_update: Option<AppUpdate>,
    #[serde(deserialize_with = "null_default")]
    pub engine_js: Option<BundleVersion>,
    #[serde(deserialize_with = "null_default")]
    pub player_js: Option<BundleVersion>,
    #[serde(deserialize_with = "null_default")]
    pub sentry_tags: BTreeMap<String, String>,
    /// When the app last heard from the service (stamped at parse); a server value wins.
    pub updated_at: i64,
}

impl RemoteConfigData {
    pub fn feature(&self, key: &str) -> Option<&Value> {
        self.features.get(key).filter(|value| !value.is_null())
    }

    pub fn player_cache_enabled(&self) -> bool {
        self.feature("playerCacheEnabled").and_then(Value::as_bool).unwrap_or(true)
    }

    pub fn log_lines(&self) -> i64 {
        self.feature("logLines").and_then(Value::as_i64).unwrap_or(5_000)
    }

    pub fn error_log_lines(&self) -> i64 {
        self.feature("errorLogLines").and_then(Value::as_i64).unwrap_or(500)
    }

    pub fn error_sample_rate(&self) -> f64 {
        self.feature("errorSampleRate").and_then(Value::as_f64).unwrap_or(1.0)
    }
}

/// What every request to the service carries.
#[derive(Debug, Clone)]
pub struct ApiIdentity {
    pub base_url: String,
    pub api_key: Option<String>,
    /// windows, macos or linux.
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub app_version_code: i32,
    pub app_version_name: String,
    pub device_id: String,
}

/// The policy around the service: the cache of the previous run, the throttle, what counts as
/// fresh. A failed fetch publishes nothing, so an outage never changes how the app behaves.
pub struct RemoteConfig {
    identity: Option<ApiIdentity>,
    client: reqwest::Client,
    cache_file: PathBuf,
    sender: watch::Sender<RemoteConfigData>,
    in_flight: tokio::sync::Mutex<()>,
    last_attempt: Mutex<i64>,
    /// Bundle revisions running now, reported as headers.
    running_bundles: Mutex<(Option<i64>, Option<i64>)>,
    log: Logger,
}

impl RemoteConfig {
    /// Loads the cache of the previous run (blocking). `identity: None` = no service in this build.
    pub fn open(identity: Option<ApiIdentity>, client: reqwest::Client, cache_file: PathBuf, log: Logger) -> Self {
        let log = log.fork("remote-config");
        let cached = match std::fs::read(&cache_file) {
            Ok(bytes) => serde_json::from_slice::<RemoteConfigData>(&bytes).unwrap_or_else(|error| {
                log.warn("R30001 config cache unreadable, dropped", Some(&error));
                let _ = std::fs::remove_file(&cache_file);
                RemoteConfigData::default()
            }),
            Err(_) => RemoteConfigData::default(),
        };
        let identity = identity.filter(|identity| !identity.base_url.trim().is_empty());
        let (sender, _) = watch::channel(cached);
        Self {
            identity,
            client,
            cache_file,
            sender,
            in_flight: tokio::sync::Mutex::new(()),
            last_attempt: Mutex::new(0),
            running_bundles: Mutex::new((None, None)),
            log,
        }
    }

    pub fn current(&self) -> RemoteConfigData {
        self.sender.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<RemoteConfigData> {
        self.sender.subscribe()
    }

    pub fn set_running_bundles(&self, engine: Option<i64>, player: Option<i64>) {
        *self.running_bundles.lock().unwrap() = (engine, player);
    }

    /// Fetches when the held config is older than `max_age` (throttled); otherwise returns.
    pub async fn check_update(&self, max_age: Duration) {
        let Some(identity) = &self.identity else { return };
        let now = now_millis();
        if now - self.current().updated_at < max_age.as_millis() as i64 {
            return;
        }
        let Ok(_guard) = self.in_flight.try_lock() else { return };
        {
            let mut last = self.last_attempt.lock().unwrap();
            if now - *last < MIN_INTERVAL.as_millis() as i64 {
                return;
            }
            *last = now;
        }
        match self.fetch(identity).await {
            Ok(mut config) => {
                if config.updated_at == 0 {
                    config.updated_at = now_millis();
                }
                if let Ok(bytes) = serde_json::to_vec(&config) {
                    let cache_file = self.cache_file.clone();
                    let saved = tokio::task::spawn_blocking(move || {
                        if let Some(parent) = cache_file.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        let staged = cache_file.with_extension("tmp");
                        std::fs::write(&staged, bytes)?;
                        std::fs::rename(&staged, &cache_file)
                    })
                    .await;
                    if let Ok(Err(error)) = saved {
                        self.log.warn("R30002 config cache not written", Some(&error));
                    }
                }
                self.sender.send_replace(config);
            }
            Err(error) => self.log.warn(&format!("R30003 config fetch failed: {error}"), None),
        }
    }

    async fn fetch(&self, identity: &ApiIdentity) -> Result<RemoteConfigData, String> {
        let url = format!("{}/v1/config", identity.base_url.trim_end_matches('/'));
        let (engine, player) = *self.running_bundles.lock().unwrap();
        let mut request = self
            .client
            .get(url)
            .timeout(Duration::from_secs(15))
            .header("x-os", &identity.os)
            .header("x-os-ver", &identity.os_version)
            .header("x-app-ver", identity.app_version_code.to_string())
            .header("x-app-ver-name", &identity.app_version_name)
            .header("x-device-id", &identity.device_id)
            .header("x-arch", &identity.arch);
        if let Some(key) = &identity.api_key {
            request = request.header("x-api-key", key);
        }
        if let Some(version) = engine {
            request = request.header("x-engine-js-ver", version.to_string());
        }
        if let Some(version) = player {
            request = request.header("x-player-js-ver", version.to_string());
        }
        let response = request.send().await.map_err(|error| error.without_url().to_string())?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status().as_u16()));
        }
        let body = response.bytes().await.map_err(|error| error.without_url().to_string())?;
        serde_json::from_slice(&body).map_err(|error| format!("malformed config: {error}"))
    }
}
