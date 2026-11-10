//! Now Playing: the current track large, and the queue ("Up next") to jump around in.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t};

use crate::playback::Playback;
use crate::video_layer::VideoLayer;

pub struct NowPlaying {
    playback: Entity<Playback>,
    scroll: ScrollHandle,
    video_layer: Option<Rc<VideoLayer>>,
    /// The video box as last painted, to report viewport changes to the player.
    video_box: Rc<Cell<(i32, i32)>>,
    reported_box: (i32, i32),
    _observe: Subscription,
}

impl NowPlaying {
    pub fn new(playback: Entity<Playback>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&playback, |_, _, cx| cx.notify());
        Self {
            playback,
            scroll: ScrollHandle::new(),
            video_layer: None,
            video_box: Rc::new(Cell::new((0, 0))),
            reported_box: (0, 0),
            _observe: observe,
        }
    }

    pub fn hide_video(&self) {
        if let Some(layer) = &self.video_layer {
            layer.hide();
        }
    }

    fn ensure_video_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.video_layer.is_some() {
            return;
        }
        let Some(layer) = VideoLayer::attach(window) else { return };
        let layer = Rc::new(layer);
        let pointer = layer.as_ptr();
        // SAFETY: the layer lives in this view for the app's lifetime, past Playback's use of it.
        self.playback.update(cx, |playback, _| unsafe { playback.attach_video_layer(pointer) });
        self.video_layer = Some(layer);
    }
}

impl Render for NowPlaying {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_video_layer(window, cx);
        let painted = self.video_box.get();
        if painted.0 > 0 && painted != self.reported_box {
            self.reported_box = painted;
            self.playback.update(cx, |playback, cx| playback.set_viewport(painted.0, painted.1, cx));
        }
        // The video layer draws above every gpui layer, so it gives way to dialogs and sheets.
        let covered = window.has_active_dialog(cx) || window.has_active_sheet(cx);
        let playback = self.playback.read(cx);
        let video_on = playback.video;
        let theme = cx.theme().clone();
        let Some(current) = playback.current().cloned() else { return div().into_any_element() };
        let queue = playback.queue.clone();
        let index = playback.index;
        let artist = current.artist.clone();

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
                            .when_some(item.thumb(120), |frame, url| {
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
                            .when_some(item.artist.clone(), |column, line| {
                                column.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(line))
                            }),
                    )
                    .when_some(item.duration_label.clone(), |row, duration| {
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
                    .child({
                        let layer = self.video_layer.clone();
                        let video_box = self.video_box.clone();
                        let show_video = video_on && !covered && layer.is_some();
                        div()
                            .relative()
                            .w_full()
                            .max_w(px(720.))
                            .aspect_ratio(16. / 9.)
                            .rounded_lg()
                            .overflow_hidden()
                            .bg(if show_video { black() } else { theme.secondary })
                            .when(!show_video, |frame| {
                                frame.when_some(current.thumb(i32::MAX), |frame, url| {
                                    frame.child(img(url).size_full().object_fit(ObjectFit::Cover))
                                })
                            })
                            .child(
                                canvas(
                                    |_, _, _| {},
                                    move |bounds, _, window, cx| {
                                        let covered = window.has_active_dialog(cx) || window.has_active_sheet(cx);
                                        if let Some(layer) = &layer {
                                            layer.place(bounds, show_video && !covered);
                                        }
                                        let width = f32::from(bounds.size.width) as i32;
                                        let height = f32::from(bounds.size.height) as i32;
                                        video_box.set((width, height));
                                    },
                                )
                                .absolute()
                                .size_full(),
                            )
                    })
                    .child(Switch::new("audio-only").small().checked(!video_on).label(t(Str::AudioOnly)).on_click(
                        cx.listener(|view, checked: &bool, _, cx| {
                            let video = !*checked;
                            view.playback.update(cx, |playback, cx| playback.set_video(video, cx));
                        }),
                    ))
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
