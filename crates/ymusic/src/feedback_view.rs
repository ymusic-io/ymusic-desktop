//! Feedback: the anonymous ids a report needs, and GitHub / email with them filled in.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::*;
use ym_i18n::{Str, t, tf};

use crate::graph::AppGraph;

const FEEDBACK_EMAIL: &str = "feedback@ymusic.io";
const FEEDBACK_ISSUE_URL: &str = "https://github.com/ymusic-io/YMusicV4/issues/new";
pub const TELEGRAM_URL: &str = "https://t.me/ymusic_io";
const DEVICE_ID_KEY: &str = "telemetry.deviceId";

/// A config-service link override, else the built-in one.
pub fn link(graph: &AppGraph, key: &str, fallback: &str) -> String {
    graph.remote.current().feature(key).and_then(|value| value.as_str()).unwrap_or(fallback).to_owned()
}

struct Info {
    device_id: String,
    app: String,
    engine: String,
    language: String,
}

impl Info {
    /// English keys whatever the UI language, so every report reads the same in triage.
    fn lines(&self) -> String {
        format!(
            "Device ID: {}\nApp: {}\nEngine: {}\nLanguage: {}",
            self.device_id, self.app, self.engine, self.language
        )
    }
}

/// Every byte but RFC 3986 unreserved; `+` stays encoded, which form decoders read as a space.
fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (byte as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

pub struct FeedbackView {
    copied: bool,
}

impl FeedbackView {
    pub fn new() -> Self {
        Self { copied: false }
    }

    fn info(cx: &App) -> Info {
        let graph = cx.global::<AppGraph>();
        Info {
            device_id: graph.settings.get_raw(DEVICE_ID_KEY).ok().flatten().unwrap_or_default(),
            app: format!("rust {} ({})", env!("CARGO_PKG_VERSION"), crate::graph::APP_VERSION),
            engine: format!("{} / {}", graph.engines.backend_version(), graph.player_source.embedded().1),
            language: ym_i18n::current_locale().to_owned(),
        }
    }
}

impl Default for FeedbackView {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for FeedbackView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let info = Self::info(cx);
        let graph = cx.global::<AppGraph>();
        let email = link(graph, "feedbackEmail", FEEDBACK_EMAIL);
        let issue = link(graph, "feedbackIssueUrl", FEEDBACK_ISSUE_URL);
        let prefill = format!("\n\n---\n{}", info.lines());
        let github_url = format!(
            "{issue}{}body={}",
            if issue.contains('?') { '&' } else { '?' },
            percent_encode(&format!("\n\n---\n```\n{}\n```", info.lines()))
        );
        let mailto = format!(
            "mailto:{email}?subject={}&body={}",
            percent_encode(t(Str::FeedbackEmailSubject)),
            percent_encode(&prefill)
        );
        let info_row = |label: &'static str, value: String| {
            h_flex()
                .gap_3()
                .child(div().w(px(96.)).text_sm().text_color(theme.muted_foreground).child(label))
                .child(div().text_sm().child(value))
        };
        let action = |id: &'static str, icon: Lucide, title: String, hint: &'static str, url: String| {
            h_flex()
                .id(id)
                .gap_3()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|style| style.bg(theme.secondary))
                .child(Icon::new(icon))
                .child(
                    v_flex()
                        .child(div().text_sm().font_medium().child(title))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(hint)),
                )
                .on_click(move |_, _, cx| cx.open_url(&url))
        };
        let lines = info.lines();
        v_flex()
            .size_full()
            .p_4()
            .gap_4()
            .max_w(px(720.))
            .child(div().text_xl().font_semibold().child(t(Str::SectionFeedback)))
            .child(div().text_sm().child(t(Str::FeedbackIntro)))
            .child(action(
                "github",
                Lucide::MessageCircle,
                t(Str::FeedbackGithubTitle).to_owned(),
                t(Str::FeedbackGithubHint),
                github_url,
            ))
            .child(action(
                "email",
                Lucide::Send,
                tf(Str::FeedbackEmailTitle, &[&email]),
                t(Str::FeedbackEmailHint),
                mailto,
            ))
            .child(div().text_xs().text_color(theme.muted_foreground).child(t(Str::FeedbackPrefilledNote)))
            .child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .bg(theme.secondary)
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(div().text_sm().font_semibold().child(t(Str::FeedbackInfoTitle)))
                            .child(
                                Button::new("copy")
                                    .small()
                                    .ghost()
                                    .label(t(if self.copied { Str::FeedbackCopied } else { Str::FeedbackCopy }))
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(lines.clone()));
                                        view.copied = true;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(info_row(t(Str::FeedbackDeviceId), info.device_id.clone()))
                    .child(info_row(t(Str::FeedbackApp), info.app.clone()))
                    .child(info_row(t(Str::FeedbackEngine), info.engine.clone()))
                    .child(info_row(t(Str::FeedbackLanguage), info.language.clone()))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(t(Str::FeedbackInfoNote))),
            )
    }
}
