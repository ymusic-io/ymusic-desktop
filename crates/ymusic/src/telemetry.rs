//! The app log and crash reporting, set up around the app graph.

use std::path::Path;
use std::sync::Arc;

use ym_common::log::{DeferredSink, Level, LogSink, Logger, StderrSink, TeeSink};
use ym_logstore::{LogStore, LogStoreSink};
use ym_remote_config::RemoteConfig;
use ym_telemetry::{ReportPolicy, SentrySink, Telemetry, TelemetryConfig};

use crate::graph::AppGraph;

/// Shared with the JVM app: nothing is sent unless this is on.
const CRASH_REPORTS_KEY: &str = "telemetry.crashReports";
const DEVICE_ID_KEY: &str = "telemetry.deviceId";

pub struct LogSetup {
    pub log: Logger,
    pub logs: Option<Arc<LogStore>>,
    reports: Arc<DeferredSink>,
}

/// The logger every layer uses: stderr, the app log (`logs.db`), and Sentry once it joins.
pub fn logger(data_dir: &Path) -> LogSetup {
    let logs = LogStore::open(&data_dir.join("logs.db")).ok();
    let reports = Arc::new(DeferredSink::default());
    let stderr_level = if cfg!(debug_assertions) { Level::Info } else { Level::Warn };
    let mut sinks: Vec<Arc<dyn LogSink>> = vec![Arc::new(StderrSink { min_level: stderr_level })];
    if let Some(logs) = &logs {
        sinks.push(Arc::new(LogStoreSink { store: logs.clone() }));
    }
    sinks.push(reports.clone());
    LogSetup { log: Logger::new(Arc::new(TeeSink { sinks })), logs, reports }
}

struct RemotePolicy(Arc<RemoteConfig>);

impl ReportPolicy for RemotePolicy {
    fn error_log_lines(&self) -> i64 {
        self.0.current().error_log_lines()
    }

    fn error_sample_rate(&self) -> f64 {
        self.0.current().error_sample_rate()
    }
}

/// After boot: trims the app log to the configured size, and starts Sentry when this build has a
/// DSN and the user allows crash reports. The returned guard keeps reporting alive.
pub fn start_reporting(setup: &LogSetup, graph: &AppGraph) -> Option<Telemetry> {
    let logs = setup.logs.clone()?;
    logs.set_max_lines(graph.remote.current().log_lines());
    let allowed = graph.settings.boolean(CRASH_REPORTS_KEY, true).unwrap_or(false);
    let dsn = option_env!("YMUSIC_SENTRY_DSN").unwrap_or_default();
    if !allowed || dsn.is_empty() {
        return None;
    }
    let telemetry = Telemetry::init(TelemetryConfig {
        dsn: dsn.to_owned(),
        release: format!("ymusic-rust@{}", env!("CARGO_PKG_VERSION")),
        environment: if cfg!(debug_assertions) { "debug".into() } else { "production".into() },
        device_id: graph.settings.string(DEVICE_ID_KEY, "").unwrap_or_default(),
    })?;
    setup.reports.set(Arc::new(SentrySink { logs, policy: Arc::new(RemotePolicy(graph.remote.clone())) }));
    Some(telemetry)
}
