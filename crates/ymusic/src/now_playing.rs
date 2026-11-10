//! Now Playing: the current track large, and the queue ("Up next") to jump around in.

use gpui_kit::component::{ActiveTheme, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_engine::yt::YtThumb;
use ym_i18n::{Str, t};

use crate::playback::Playback;

pub struct NowPlaying {
    playback: Entity<Playback>,
    scroll: ScrollHandle,
    _observe: Subscription,
}

impl NowPlaying {
    pub fn new(playback: Entity<Playback>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&playback, |_, _, cx| cx.notify());
        Self { playback, scroll: ScrollHandle::new(), _observe: observe }
    }
}

fn largest(thumbs: &[YtThumb]) -> Option<SharedString> {
    thumbs.last().map(|thumb| {
        if thumb.url.starts_with("//") { format!("https:{}", thumb.url) } else { thumb.url.clone() }.into()
    })
}

fn small(thumbs: &[YtThumb]) -> Option<SharedString> {
    thumbs.iter().find(|thumb| thumb.w.is_some_and(|w| w >= 120)).or(thumbs.last()).map(|thumb| {
        if thumb.url.starts_with("//") { format!("https:{}", thumb.url) } else { thumb.url.clone() }.into()
    })
}

impl Render for NowPlaying {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let playback = self.playback.read(cx);
        let theme = cx.theme().clone();
        let Some(current) = playback.current().cloned() else { return div().into_any_element() };
        let queue = playback.queue.clone();
        let index = playback.index;
        let artist = current.channel_title.clone().or(current.subtitle_line1.clone());

        let mut list = v_flex().gap_1();
        for (position, item) in queue.iter().enumerate() {
            let active = position == index;
            list = list.child(
                h_flex()
                    .id(("queue-item", position))
                    .gap_3()
                    .p_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(active, |row| row.bg(theme.secondary))
                    .hover(|style| style.bg(theme.secondary))
                    .child(
                        div()
                            .w(px(80.))
                            .h(px(45.))
                            .flex_shrink_0()
                            .rounded_md()
                            .overflow_hidden()
                            .bg(theme.secondary)
                            .when_some(small(&item.thumbs), |frame, url| {
                                frame.child(img(url).size_full().object_fit(ObjectFit::Cover))
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .truncate()
                                    .when(active, |text| text.font_semibold())
                                    .child(item.title.clone()),
                            )
                            .when_some(item.channel_title.clone().or(item.subtitle_line1.clone()), |column, line| {
                                column.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(line))
                            }),
                    )
                    .when_some(item.duration.clone(), |row, duration| {
                        row.child(div().text_xs().text_color(theme.muted_foreground).child(duration))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.playback.update(cx, |playback, cx| playback.jump_to(position, cx))
                    })),
            );
        }

        h_flex()
            .size_full()
            .gap_8()
            .p_8()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_4()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(560.))
                            .aspect_ratio(16. / 9.)
                            .rounded_lg()
                            .overflow_hidden()
                            .bg(theme.secondary)
                            .when_some(largest(&current.thumbs), |frame, url| {
                                frame.child(img(url).size_full().object_fit(ObjectFit::Cover))
                            }),
                    )
                    .child(div().text_xl().font_semibold().text_center().child(current.title.clone()))
                    .when_some(artist, |column, artist| {
                        column.child(div().text_color(theme.muted_foreground).child(artist))
                    }),
            )
            .child(
                v_flex()
                    .w(px(380.))
                    .h_full()
                    .gap_2()
                    .child(div().text_lg().font_semibold().child(t(Str::UpNext)))
                    .child(
                        div()
                            .id("queue-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(list),
                    ),
            )
            .into_any_element()
    }
}
