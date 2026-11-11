//! Feedback: the anonymous ids a report needs, and GitHub / email with them filled in.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t, tf};

use crate::a11y::KeyboardFocusable;
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let viewport = window.viewport_size();
        let width = crate::feed::feed_width(window);
        let margin = if width >= 840. { 24. } else { 16. };
        let center_vertically = width >= 840. && f32::from(viewport.height) >= 600.;

        let info_row = |label: &'static str, value: String| {
            h_flex()
                .gap_4()
                .pr_3()
                .child(div().w(px(96.)).flex_shrink_0().text_sm().text_color(theme.muted_foreground).child(label))
                .child(div().flex_1().min_w_0().text_sm().child(value))
        };
        let contact = |id: &'static str, icon: Lucide, title: String, hint: &'static str, url: String| {
            h_flex()
                .id(id)
                .role(Role::Link)
                .aria_label(title.clone())
                .keyboard_focusable(theme.ring)
                .gap_4()
                .px_4()
                .py_3()
                .items_center()
                .rounded(px(12.))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|style| style.bg(theme.secondary))
                .child(
                    div()
                        .size(px(40.))
                        .flex_shrink_0()
                        .rounded_full()
                        .bg(theme.secondary)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(icon).size(px(22.))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_medium().child(title))
                        .child(div().pt(px(2.)).text_xs().text_color(theme.muted_foreground).child(hint)),
                )
                .child(Icon::new(Lucide::ExternalLink).size(px(18.)).text_color(theme.muted_foreground))
                .on_click(move |_, _, cx| cx.open_url(&url))
        };
        let lines = info.lines();
        let info_card = v_flex()
            .gap_2()
            .pl_4()
            .pr_1()
            .pt_1()
            .pb_4()
            .rounded(px(12.))
            .bg(theme.secondary)
            .child(
                h_flex()
                    .items_center()
                    .child(div().flex_1().text_sm().font_semibold().child(t(Str::FeedbackInfoTitle)))
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
            .child(div().pr_3().text_xs().text_color(theme.muted_foreground).child(t(Str::FeedbackInfoNote)));
        let column = v_flex()
            .w_full()
            .max_w(px(560.))
            .px(px(margin))
            .py_4()
            .gap_4()
            .child(div().text_sm().child(t(Str::FeedbackIntro)))
            .child(info_card)
            .child(contact(
                "github",
                Lucide::MessageCircle,
                t(Str::FeedbackGithubTitle).to_owned(),
                t(Str::FeedbackGithubHint),
                github_url,
            ))
            .child(contact(
                "email",
                Lucide::Send,
                tf(Str::FeedbackEmailTitle, &[&email]),
                t(Str::FeedbackEmailHint),
                mailto,
            ))
            .child(div().text_xs().text_color(theme.muted_foreground).child(t(Str::FeedbackPrefilledNote)));
        div().id("feedback-scroll").size_full().overflow_y_scroll().child(
            h_flex()
                .w_full()
                .min_h_full()
                .justify_center()
                .when(center_vertically, |row| row.items_center())
                .when(!center_vertically, |row| row.items_start())
                .child(column),
        )
    }
}
