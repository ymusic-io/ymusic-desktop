//! Play again: the queues played lately, newest first, each resumable where it was left.

use std::sync::Arc;

use gpui_kit::component::{ActiveTheme, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_history::{PlayAgainSession, PlayHistory};
use ym_i18n::{Str, t, tf};

use crate::graph::{AppGraph, on_core};
use crate::mini_player::clock;
use crate::play_history::decode_items;
use crate::playback::QueueItem;

/// Resume this saved queue.
pub struct ResumeQueue {
    pub queue_id: String,
    pub items: Vec<QueueItem>,
    pub index: usize,
    pub start_ms: i64,
}

pub struct PlayAgainView {
    history: Arc<PlayHistory>,
    sessions: Vec<PlayAgainSession>,
    error: Option<String>,
    scroll: ScrollHandle,
}

impl EventEmitter<ResumeQueue> for PlayAgainView {}

impl PlayAgainView {
    pub fn new(history: Arc<PlayHistory>, cx: &mut Context<Self>) -> Self {
        let mut view = Self { history, sessions: Vec::new(), error: None, scroll: ScrollHandle::new() };
        view.refresh(cx);
        view
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let history = self.history.clone();
        let load = on_core(cx, async move { tokio::task::spawn_blocking(move || history.sessions()).await });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |view, cx| {
                match result {
                    Some(Ok(Ok(sessions))) => {
                        view.sessions = sessions;
                        view.error = None;
                    }
                    Some(Ok(Err(error))) => view.failed(&error.to_string(), cx),
                    Some(Err(error)) => view.failed(&error.to_string(), cx),
                    None => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn failed(&mut self, message: &str, cx: &mut Context<Self>) {
        cx.global::<AppGraph>().log.warn(&format!("R90017 play again could not be read: {message}"), None);
        self.error = Some(tf(Str::ErrorGeneric, &[&"R90017"]));
    }

    /// Debug: resumes the newest session once the list has loaded.
    pub fn resume_newest(&mut self, cx: &mut Context<Self>) {
        let history = self.history.clone();
        let load = on_core(cx, async move { tokio::task::spawn_blocking(move || history.sessions()).await });
        cx.spawn(async move |this, cx| {
            if let Some(Ok(Ok(sessions))) = load.await
                && let Some(newest) = sessions.into_iter().next()
            {
                this.update(cx, |view, cx| view.resume(newest, cx)).ok();
            }
        })
        .detach();
    }

    fn resume(&mut self, session: PlayAgainSession, cx: &mut Context<Self>) {
        let history = self.history.clone();
        let queue_id = session.queue_id.clone();
        let load =
            on_core(cx, async move { tokio::task::spawn_blocking(move || history.load_session(&queue_id)).await });
        cx.spawn(async move |this, cx| {
            let loaded = load.await;
            this.update(cx, |view, cx| match loaded {
                Some(Ok(Ok(Some((saved, json))))) => match decode_items(&json) {
                    Ok(items) if !items.is_empty() => {
                        let start_ms = if saved.resumes_mid_item() { saved.position_ms } else { 0 };
                        let index = saved.queue_index.min(items.len() - 1);
                        cx.emit(ResumeQueue { queue_id: saved.queue_id, items, index, start_ms });
                    }
                    Ok(_) => view.refresh(cx),
                    Err(message) => view.failed(&message, cx),
                },
                Some(Ok(Ok(None))) => view.refresh(cx),
                Some(Ok(Err(error))) => view.failed(&error.to_string(), cx),
                Some(Err(error)) => view.failed(&error.to_string(), cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }
}

impl Render for PlayAgainView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut list = v_flex().gap_1();
        for (position, session) in self.sessions.iter().enumerate() {
            let resume_label =
                session.resumes_mid_item().then(|| tf(Str::PlayAgainResumeAt, &[&clock(session.position_ms)]));
            let track = tf(Str::PlayAgainTrackOf, &[&(session.queue_index + 1), &session.queue_size]);
            let detail =
                [session.subtitle.clone(), Some(track), resume_label].into_iter().flatten().collect::<Vec<_>>();
            let target = session.clone();
            list = list.child(
                h_flex()
                    .id(("play-again", position))
                    .gap_3()
                    .p_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary))
                    .child(
                        div()
                            .w(px(96.))
                            .h(px(54.))
                            .flex_shrink_0()
                            .rounded_md()
                            .overflow_hidden()
                            .bg(theme.secondary)
                            .when_some(session.thumbnail_url.clone(), |frame, url| {
                                frame.child(img(SharedString::from(url)).size_full().object_fit(ObjectFit::Cover))
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().truncate().child(session.title.clone()))
                            .child(
                                div().text_xs().text_color(theme.muted_foreground).truncate().child(detail.join(" · ")),
                            ),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| view.resume(target.clone(), cx))),
            );
        }
        v_flex()
            .size_full()
            .p_4()
            .gap_3()
            .child(div().text_xl().font_semibold().child(t(Str::SectionPlayAgain)))
            .when_some(self.error.clone(), |column, error| {
                column.child(div().text_sm().text_color(theme.danger).child(error))
            })
            .when(self.sessions.is_empty() && self.error.is_none(), |column| {
                column.child(div().text_sm().text_color(theme.muted_foreground).child(t(Str::PlayAgainEmpty)))
            })
            .child(
                div()
                    .id("play-again-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(list),
            )
    }
}
