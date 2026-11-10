//! The persistent mini-player bar: what's playing, transport controls, seek and volume.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::slider::{SliderEvent, SliderState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t};

use crate::playback::{PlayState, Playback};

/// The user asked to open (or close) Now Playing.
pub struct ToggleNowPlaying;

pub struct MiniPlayer {
    playback: Entity<Playback>,
    seek: Entity<SliderState>,
    volume: Entity<SliderState>,
    dragging_seek: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ToggleNowPlaying> for MiniPlayer {}

impl MiniPlayer {
    pub fn new(playback: Entity<Playback>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let seek = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.001));
        let initial_volume = playback.read(cx).volume;
        let volume = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.01).default_value(initial_volume));
        let subscriptions = vec![
            cx.observe_in(&playback, window, |player, playback, window, cx| {
                if !player.dragging_seek {
                    let playback = playback.read(cx);
                    let fraction = if playback.duration_ms > 0 {
                        (playback.position_ms as f32 / playback.duration_ms as f32).clamp(0., 1.)
                    } else {
                        0.
                    };
                    player.seek.update(cx, |slider, cx| slider.set_value(fraction, window, cx));
                }
                cx.notify();
            }),
            cx.subscribe(&seek, |player, _, event: &SliderEvent, cx| match event {
                SliderEvent::Change(_) => player.dragging_seek = true,
                SliderEvent::Release(value) => {
                    player.dragging_seek = false;
                    let fraction = value.end();
                    player.playback.update(cx, |playback, cx| {
                        let target = (playback.duration_ms as f32 * fraction) as i64;
                        playback.seek(target, cx);
                    });
                }
            }),
            cx.subscribe(&volume, |player, _, event: &SliderEvent, cx| {
                let (SliderEvent::Change(value) | SliderEvent::Release(value)) = event;
                let level = value.end();
                player.playback.update(cx, |playback, cx| playback.set_volume(level, cx));
            }),
        ];
        Self { playback, seek, volume, dragging_seek: false, _subscriptions: subscriptions }
    }
}

pub fn clock(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

impl Render for MiniPlayer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let playback = self.playback.read(cx);
        let Some(item) = playback.current().cloned() else { return div().into_any_element() };
        let theme = cx.theme();
        let state = playback.state.clone();
        let (position, duration) = (playback.position_ms, playback.duration_ms);
        let has_next = playback.index + 1 < playback.queue.len();
        let thumb = item.thumb(120);
        let artist = item.artist.clone();
        let failure = match &state {
            PlayState::Failed(message) => Some(message.clone()),
            _ => None,
        };
        let play_pause: AnyElement = match state {
            PlayState::Buffering => {
                div().size(px(36.)).flex().items_center().justify_center().child(Spinner::new()).into_any_element()
            }
            PlayState::Playing => Button::new("pause")
                .icon(Icon::new(Lucide::Pause))
                .rounded_full()
                .tooltip(t(Str::CdPause))
                .on_click(
                    cx.listener(|player, _, _, cx| player.playback.update(cx, |playback, cx| playback.toggle(cx))),
                )
                .into_any_element(),
            _ => Button::new("play")
                .icon(Icon::new(Lucide::Play))
                .rounded_full()
                .tooltip(t(Str::CdPlay))
                .on_click(
                    cx.listener(|player, _, _, cx| player.playback.update(cx, |playback, cx| playback.toggle(cx))),
                )
                .into_any_element(),
        };
        h_flex()
            .gap_4()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .id("now-playing-toggle")
                    .w(px(300.))
                    .gap_3()
                    .min_w_0()
                    .cursor_pointer()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ToggleNowPlaying)))
                    .child(
                        div()
                            .size(px(48.))
                            .flex_shrink_0()
                            .rounded_md()
                            .overflow_hidden()
                            .bg(theme.secondary)
                            .when_some(thumb, |frame, url| {
                                frame.child(img(url).size_full().object_fit(ObjectFit::Cover))
                            }),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_sm().font_medium().truncate().child(item.title.clone()))
                            .when_some(failure.clone().or(artist), |column, line| {
                                let color = if failure.is_some() { theme.danger } else { theme.muted_foreground };
                                column.child(div().text_xs().text_color(color).truncate().child(line))
                            }),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .gap_1()
                    .items_center()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("previous")
                                    .ghost()
                                    .icon(Icon::new(Lucide::SkipBack))
                                    .tooltip(t(Str::CdPrevious))
                                    .on_click(cx.listener(|player, _, _, cx| {
                                        player.playback.update(cx, |playback, cx| playback.previous(cx))
                                    })),
                            )
                            .child(play_pause)
                            .child(
                                Button::new("next")
                                    .ghost()
                                    .icon(Icon::new(Lucide::SkipForward))
                                    .tooltip(t(Str::CdNext))
                                    .disabled(!has_next)
                                    .on_click(cx.listener(|player, _, _, cx| {
                                        player.playback.update(cx, |playback, cx| playback.next(cx))
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .max_w(px(560.))
                            .gap_2()
                            .items_center()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(clock(position)))
                            .child(div().flex_1().child(Slider::new(&self.seek)))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(clock(duration))),
                    ),
            )
            .child(
                h_flex()
                    .w(px(160.))
                    .gap_2()
                    .items_center()
                    .child(Icon::new(Lucide::Volume2).small().text_color(theme.muted_foreground))
                    .child(div().flex_1().child(Slider::new(&self.volume))),
            )
            .into_any_element()
    }
}
