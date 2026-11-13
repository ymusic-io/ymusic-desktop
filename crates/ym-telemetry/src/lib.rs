//! Crash and error reporting to Sentry: warnings and errors ride as breadcrumbs, `capture_error`
//! is the one event of a failed job (tagged with its trail and error code, carrying the newest
//! unsent app-log lines), and panics are reported by the SDK.

mod feedback;

pub use feedback::{FeedbackError, FeedbackReport, send_feedback};

use std::borrow::Cow;
use std::error::Error;
use std::sync::Arc;

use sentry::protocol::{Attachment, Breadcrumb, Event, Level as SentryLevel};
use ym_common::log::{Crumb, Level, LogSink};
use ym_logstore::LogStore;

const TRAIL_TAG: &str = "trail";
const CODE_TAG: &str = "error.code";
const DEVICE_ID_TAG: &str = "device_id";
const REPORT_TYPE_TAG: &str = "report_type";
/// Failures that are never defects: the UI already shows their localized category.
const IGNORED_CATEGORIES: &[&str] = &["network"];
/// As the JVM app keeps.
const MAX_BREADCRUMBS: usize = 200;
/// This workspace's crates: a panic's culprit is the topmost of their frames, never the SDK's or gpui's.
const IN_APP_CRATES: &[&str] = &["ymusic::", "ym_"];

/// What a report may carry, read at send time (the remote config can change it).
pub trait ReportPolicy: Send + Sync {
    /// How many of the newest log lines an error event takes; 0 = none.
    fn error_log_lines(&self) -> i64;
    /// The share of non-fatal errors sent, 0..=1.
    fn error_sample_rate(&self) -> f64;
}

pub struct Telemetry {
    _guard: sentry::ClientInitGuard,
}

pub struct TelemetryConfig {
    pub dsn: String,
    pub release: String,
    pub environment: String,
    pub device_id: String,
}

impl Telemetry {
    /// `None` when there is no DSN: nothing is ever sent.
    pub fn init(config: TelemetryConfig) -> Option<Self> {
        if config.dsn.trim().is_empty() {
            return None;
        }
        let mut options = sentry::ClientOptions::default();
        options.release = Some(config.release.into());
        options.environment = Some(config.environment.into());
        options.send_default_pii = false;
        options.max_breadcrumbs = MAX_BREADCRUMBS;
        options.in_app_include = IN_APP_CRATES.to_vec();
        // The contexts integration fills in the host name, which can carry the user's name.
        options.before_send = Some(Arc::new(|mut event| {
            event.server_name = None;
            Some(event)
        }));
        let guard = sentry::init((config.dsn.as_str(), options));
        sentry::configure_scope(|scope| scope.set_tag(DEVICE_ID_TAG, &config.device_id));
        Some(Self { _guard: guard })
    }
}

/// The Sentry side of the logger.
pub struct SentrySink {
    pub logs: Arc<LogStore>,
    pub policy: Arc<dyn ReportPolicy>,
}

fn trail_path(trail: &[Crumb]) -> String {
    trail.iter().map(|crumb| crumb.tag.as_str()).collect::<Vec<_>>().join("/")
}

/// The code an error carries: a category word at the start, else the first site code in its text.
fn error_code(text: &str) -> Option<String> {
    if let Some(category) = IGNORED_CATEGORIES.iter().find(|category| text.starts_with(*category)) {
        return Some((*category).to_owned());
    }
    let bytes = text.as_bytes();
    (0..bytes.len().saturating_sub(5)).find_map(|start| {
        let candidate = text.get(start..start + 6)?;
        let code = candidate.as_bytes();
        let bounded = (start == 0 || !bytes[start - 1].is_ascii_alphanumeric())
            && bytes.get(start + 6).is_none_or(|next| !next.is_ascii_alphanumeric());
        (bounded && matches!(code[0], b'E' | b'R') && code[1..].iter().all(u8::is_ascii_digit))
            .then(|| candidate.to_owned())
    })
}

impl LogSink for SentrySink {
    fn log(&self, level: Level, trail: &[Crumb], message: &str, error: Option<&(dyn Error + 'static)>) {
        if level < Level::Warn {
            return;
        }
        let text = match error {
            Some(error) => format!("{message}: {error}"),
            None => message.to_owned(),
        };
        sentry::add_breadcrumb(Breadcrumb {
            category: Some(trail_path(trail)),
            message: Some(text),
            level: if level == Level::Error { SentryLevel::Error } else { SentryLevel::Warning },
            ..Default::default()
        });
    }

    fn capture_error(&self, trail: &[Crumb], message: &str, error: &(dyn Error + 'static)) {
        let description = error.to_string();
        let code = error_code(&description).or_else(|| error_code(message));
        if code.as_deref().is_some_and(|code| IGNORED_CATEGORIES.contains(&code)) {
            return;
        }
        let rate = self.policy.error_sample_rate().clamp(0.0, 1.0);
        if rate < 1.0 && fastrand_unit() >= rate {
            return;
        }
        let lines = self.policy.error_log_lines();
        let log_text = if lines > 0 {
            self.logs
                .claim_unsent(lines)
                .map(|lines| {
                    lines
                        .iter()
                        .map(|line| format!("{} {} [{}] {}", line.epoch_ms, line.level, line.trail, line.message))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        let path = trail_path(trail);
        sentry::with_scope(
            |scope| {
                scope.set_tag(TRAIL_TAG, &path);
                scope.set_tag(REPORT_TYPE_TAG, "error");
                if let Some(code) = &code {
                    scope.set_tag(CODE_TAG, code);
                }
                for crumb in trail {
                    if let Some(props) = &crumb.props {
                        for (key, value) in props {
                            scope.set_extra(&format!("{}.{key}", crumb.tag), value.clone());
                        }
                    }
                }
                if !log_text.is_empty() {
                    scope.add_attachment(Attachment {
                        buffer: log_text.into_bytes(),
                        filename: "log.txt".into(),
                        content_type: Some("text/plain".into()),
                        ..Default::default()
                    });
                }
            },
            || {
                let mut event = sentry::event_from_error(error);
                event.message = Some(message.to_owned());
                event.level = SentryLevel::Error;
                if let Some(fingerprint) = fingerprint(code.as_deref(), &path, &event) {
                    event.fingerprint = fingerprint.into_iter().map(Cow::Owned).collect::<Vec<_>>().into();
                }
                sentry::capture_event(event);
            },
        );
    }
}

/// The JVM app's grouping: a site code is one throw site, one issue; an uncoded error groups by its
/// type within its trail, so one failure shape in two subsystems stays two issues.
fn fingerprint(code: Option<&str>, trail: &str, event: &Event<'_>) -> Option<Vec<String>> {
    if let Some(code) = code {
        return Some(vec![code.to_owned()]);
    }
    if trail.is_empty() {
        return None;
    }
    let kind = event.exception.values.last().map_or("error", |exception| exception.ty.as_str());
    Some(vec![kind.to_owned(), trail.to_owned()])
}

/// A uniform 0..1 draw for sampling; quality is irrelevant.
fn fastrand_unit() -> f64 {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.subsec_nanos());
    f64::from(nanos % 1_000_000) / 1_000_000.0
}
