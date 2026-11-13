//! The app log and crash reporting, set up around the app graph.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ym_common::log::{DeferredSink, Level, LogSink, Logger, StderrSink, TeeSink};
use ym_logstore::{LogStore, LogStoreSink};
use ym_remote_config::RemoteConfig;
use ym_settings::SettingsStore;
use ym_telemetry::{FeedbackReport, LogShipper, ReportPolicy, SentrySink, Telemetry, TelemetryConfig};

use crate::graph::AppGraph;

/// Shared with the JVM app: nothing is sent unless this is on.
pub const CRASH_REPORTS_KEY: &str = "telemetry.crashReports";
/// Shared with the JVM app: "Send anonymous stats"; nothing is recorded or sent while off.
pub const STATS_KEY: &str = "telemetry.stats";
const DEVICE_ID_KEY: &str = "telemetry.deviceId";
/// Debug builds: the feedback report's envelopes go to this file instead of Sentry.
const FEEDBACK_DRY_RUN_ENV: &str = "YMUSIC_DEV_FEEDBACK_ENVELOPE";
const FEEDBACK_EXPORT_TIMEOUT: Duration = Duration::from_secs(15);

/// The app log, for the feedback report.
pub struct AppLogs(pub Option<Arc<LogStore>>);

impl gpui_kit::Global for AppLogs {}

fn sentry_dsn() -> &'static str {
    option_env!("YMUSIC_SENTRY_DSN").unwrap_or_default()
}

fn telemetry_config(dsn: &str, settings: &dyn SettingsStore) -> TelemetryConfig {
    TelemetryConfig {
        dsn: dsn.to_owned(),
        release: format!("ymusic-rust@{}", crate::graph::APP_VERSION_NAME),
        environment: if cfg!(debug_assertions) { "debug".into() } else { "production".into() },
        device_id: settings.string(DEVICE_ID_KEY, "").unwrap_or_default(),
    }
}

pub fn crash_reports_allowed(settings: &dyn SettingsStore) -> bool {
    settings.boolean(CRASH_REPORTS_KEY, true).unwrap_or(false)
}

fn feedback_dry_run() -> Option<PathBuf> {
    cfg!(debug_assertions).then(|| std::env::var_os(FEEDBACK_DRY_RUN_ENV)).flatten().map(PathBuf::from)
}

/// Feedback can go out: this build has a DSN (or a debug dry run is set).
pub fn feedback_available() -> bool {
    !sentry_dsn().is_empty() || feedback_dry_run().is_some()
}

/// Sends the whole app log with `subject` (and the user's `message`) as one anonymous Sentry report,
/// found in triage by the device id; without the log when its export fails or stalls.
pub async fn send_feedback(
    logs: Option<Arc<LogStore>>,
    settings: Arc<dyn SettingsStore>,
    subject: String,
    message: Option<String>,
    log: Logger,
) -> Result<(), ym_telemetry::FeedbackError> {
    let log_gzip = match logs {
        Some(logs) => {
            let export = tokio::task::spawn_blocking(move || {
                let mut gzip = Vec::new();
                logs.export_gzip(&mut gzip).map(|()| gzip)
            });
            match tokio::time::timeout(FEEDBACK_EXPORT_TIMEOUT, export).await {
                Ok(Ok(Ok(gzip))) => Some(gzip),
                Ok(Ok(Err(error))) => {
                    log.warn("log export for feedback failed", Some(&error));
                    None
                }
                Ok(Err(error)) => {
                    log.warn("log export for feedback failed", Some(&error));
                    None
                }
                Err(_) => {
                    log.warn("log export for feedback timed out", None);
                    None
                }
            }
        }
        None => None,
    };
    let dry_run = feedback_dry_run();
    let dsn = if dry_run.is_some() { "https://dryrun@localhost/0" } else { sentry_dsn() };
    let config = telemetry_config(dsn, settings.as_ref());
    let report = FeedbackReport { subject, message, log_gzip };
    tokio::task::spawn_blocking(move || ym_telemetry::send_feedback(&config, report, dry_run).map(|_| ()))
        .await
        .map_err(|_| ym_telemetry::FeedbackError::Dropped)?
}

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

/// Keeps reporting alive for the app's lifetime.
pub struct Reporting {
    pub _guard: Option<Telemetry>,
}

impl gpui_kit::Global for Reporting {}

/// The crash-reports switch applies at once: each value saved under [`CRASH_REPORTS_KEY`].
pub fn crash_reports_changed(value: &str) {
    ym_telemetry::set_reports_allowed(value != "false");
}

/// After boot: trims the app log to the configured size, and starts Sentry when this build has a
/// DSN; the user's crash-reports switch gates what leaves, live. Error events' log lines go out as
/// Sentry Logs over `http`. The returned guard keeps reporting alive.
pub fn start_reporting(setup: &LogSetup, graph: &AppGraph, http: reqwest::Client) -> Option<Telemetry> {
    let logs = setup.logs.clone()?;
    logs.set_max_lines(graph.remote.current().log_lines());
    let dsn = sentry_dsn();
    if dsn.is_empty() {
        return None;
    }
    let config = telemetry_config(dsn, graph.settings.as_ref());
    let shipper =
        LogShipper::start(logs, &config, crate::graph::APP_VERSION, http, &graph.tokio, graph.log.fork("sentry-logs"));
    let telemetry = Telemetry::init(config, crash_reports_allowed(graph.settings.as_ref()))?;
    match shipper {
        Ok(shipper) => {
            let sink = Arc::new(SentrySink { logs: shipper, policy: Arc::new(RemotePolicy(graph.remote.clone())) });
            sink.attach_logs_to_panics();
            setup.reports.set(sink);
        }
        Err(error) => {
            graph.log.warn("R90025 Sentry reporting runs without its sink: the DSN does not parse", Some(&error))
        }
    }
    follow_remote_tags(graph);
    Some(telemetry)
}

/// Keeps the events' remote tags equal to the config's `sentryTags`, from the cached config on.
fn follow_remote_tags(graph: &AppGraph) {
    let mut config = graph.remote.subscribe();
    graph.tokio.spawn(async move {
        loop {
            ym_telemetry::set_remote_tags(&config.borrow_and_update().sentry_tags);
            if config.changed().await.is_err() {
                return;
            }
        }
    });
}
