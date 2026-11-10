use std::sync::{Arc, OnceLock};

use serde::Deserialize;
use ym_common::http::HttpClients;
use ym_common::log::{Level, Logger};
use ym_quickjs::{LogHandler, LogLevel, PipeError, QuickJsRuntime};

use crate::error::EngineError;
use crate::logs::ParkedLoggers;

pub struct EngineConfig {
    /// The `versions.app` the bundle gates compatibility on: the desktop versionCode sequence.
    pub app_version: i32,
    pub http: HttpClients,
    pub logger: Logger,
}

/// One booted `engine.js` and the host protocols it is served.
pub struct DynamicEngine {
    runtime: QuickJsRuntime,
    parked: Arc<ParkedLoggers>,
    backend_version: OnceLock<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitHead {
    backend_version: i32,
}

impl DynamicEngine {
    /// Boots `script` with every host protocol wired before the eval, so the bundle's top-level
    /// code can use them. Run [`Self::health_check`] afterwards to gate on the bundle.
    pub async fn create(script: String, config: EngineConfig) -> Result<Self, EngineError> {
        let console = config.logger.fork("js");
        let on_log: LogHandler = Arc::new(move |level, message| {
            let level = match level {
                LogLevel::Debug => Level::Debug,
                LogLevel::Info => Level::Info,
                LogLevel::Warn | LogLevel::Rejection => Level::Warn,
                LogLevel::Error => Level::Error,
            };
            console.log(level, message, None);
        });
        let runtime = QuickJsRuntime::new(config.app_version, None, Some(on_log)).map_err(load_failure)?;
        let parked = Arc::new(ParkedLoggers::new(config.logger));
        crate::http::install(&runtime, config.http, parked.clone());
        if let Err(error) = runtime.eval_engine(script).await {
            runtime.close().await;
            return Err(load_failure(error));
        }
        Ok(Self { runtime, parked, backend_version: OnceLock::new() })
    }

    /// Drives the bundle's `"init"`, proving open routing, handler dispatch, JSON encode and END,
    /// and records the bundle's self-declared revision. A RESET keeps the bundle's own code.
    pub async fn health_check(&self) -> Result<i32, EngineError> {
        let body = self.runtime.open_read_now("init").await.map_err(|error| match error {
            PipeError::Reset(reason) => EngineError::decode_reset(&reason),
            other => EngineError::site("R20004", format!("engine init failed: {other}")),
        })?;
        let head: InitHead = serde_json::from_str(&body).map_err(|_| {
            let preview: String = body.chars().take(200).collect();
            EngineError::site("R20004", format!("engine init returned a malformed head: {preview}"))
        })?;
        let _ = self.backend_version.set(head.backend_version);
        Ok(head.backend_version)
    }

    /// The bundle's revision; `None` until [`Self::health_check`] ran.
    pub fn backend_version(&self) -> Option<i32> {
        self.backend_version.get().copied()
    }

    pub fn parked_loggers(&self) -> &ParkedLoggers {
        &self.parked
    }

    pub fn runtime(&self) -> &QuickJsRuntime {
        &self.runtime
    }

    pub async fn close(&self) {
        self.runtime.close().await;
    }
}

fn load_failure(error: PipeError) -> EngineError {
    EngineError::site("R20003", format!("engine load failed: {error}"))
}
