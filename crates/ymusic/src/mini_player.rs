//! The persistent mini-player bar: what's playing, transport controls, seek and volume.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::slider::{SliderEvent, SliderState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Selectable, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t, tf};

use crate::a11y::KeyboardFocusable;
use crate::playback::{PlayState, Playback, QueueItem};
use crate::queue_nav::LoopMode;
use crate::spinner::Spinner;

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

const SLEEP_PRESET_MINUTES: [u64; 5] = [15, 30, 45, 60, 90];

impl MiniPlayer {
    fn sleep_button(&self, remaining: Option<std::time::Duration>, after_current: bool) -> impl IntoElement {
        let tooltip = match remaining {
            Some(left) => tf(Str::SleepPausingIn, &[&clock(left.as_millis() as i64)]),
            None if after_current => t(Str::SleepPausingAfterTrack).to_owned(),
            None => t(Str::CdSleepTimer).to_owned(),
        };
        let playback = self.playback.clone();
        let armed = remaining.is_some() || after_current;
        Button::new("sleep-timer")
            .ghost()
            .small()
            .icon(Icon::new(Lucide::Timer))
            .selected(armed)
            .accessibility_label(t(Str::CdSleepTimer))
            .tooltip(tooltip)
            .dropdown_menu_with_anchor(Anchor::BottomRight, move |mut menu, _, _| {
                for minutes in SLEEP_PRESET_MINUTES {
                    let playback = playback.clone();
                    menu =
                        menu.item(PopupMenuItem::new(tf(Str::SleepMinutes, &[&minutes])).on_click(move |_, _, cx| {
                            playback.update(cx, |playback, cx| {
                                playback.set_sleep_timer(Some(std::time::Duration::from_secs(minutes * 60)), cx)
                            })
                        }));
                }
                let end_of_track = playback.clone();
                menu = menu.item(PopupMenuItem::new(t(Str::SleepEndOfTrack)).checked(after_current).on_click(
                    move |_, _, cx| end_of_track.update(cx, |playback, cx| playback.set_sleep_after_current(true, cx)),
                ));
                if armed {
                    let off = playback.clone();
                    menu =
                        menu.separator().item(PopupMenuItem::new(t(Str::SleepTurnOff)).on_click(move |_, _, cx| {
                            off.update(cx, |playback, cx| playback.set_sleep_timer(None, cx))
                        }));
                }
                menu
            })
    }
}

pub fn clock(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

/// The track info takes this share of the bar once that beats `BAR_INFO_MIN`.
const BAR_INFO_SHARE: f32 = 0.3;
const BAR_INFO_MIN: f32 = 300.;
const BAR_SEEK_MAX: f32 = 560.;
const BAR_VOLUME_WIDTH: f32 = 120.;
/// Below this the side controls drop out; Now Playing still has them.
const BAR_SIDE_MIN: f32 = 900.;
/// The sleep timer joins the side controls from this width.
const BAR_SLEEP_MIN: f32 = 1100.;
const BAR_PLAY: f32 = 36.;

impl MiniPlayer {
    fn now_playing_label(item: &QueueItem) -> String {
        [Some(t(Str::A11yNowPlaying).to_owned()), Some(item.title.clone()), item.artist.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The artist, or the failure in its place.
    fn subtitle(item: &QueueItem, state: &PlayState, cx: &App) -> Option<(String, Hsla)> {
        let theme = cx.theme();
        match state {
            PlayState::Failed(failure) => Some((failure.text(), theme.danger)),
            _ => item.artist.clone().map(|artist| (artist, theme.muted_foreground)),
        }
    }

    fn cover(item: &QueueItem, cx: &App) -> Div {
        div()
            .overflow_hidden()
            .bg(cx.theme().secondary)
            .flex_shrink_0()
            .when_some(item.thumb(320), |frame, url| frame.child(img(url).size_full().object_fit(ObjectFit::Cover)))
    }

    fn play_label(state: &PlayState) -> &'static str {
        t(if matches!(state, PlayState::Playing | PlayState::Buffering) { Str::CdPause } else { Str::CdPlay })
    }

    /// The bar's filled play circle.
    fn play_circle(&self, state: &PlayState, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let playing = matches!(state, PlayState::Playing | PlayState::Buffering);
        div()
            .id("play-pause")
            .role(Role::Button)
            .aria_label(Self::play_label(state))
            .keyboard_focusable(theme.ring)
            .size(px(BAR_PLAY))
            .flex_shrink_0()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(theme.foreground)
            .text_color(theme.background)
            .child(if *state == PlayState::Buffering {
                Spinner::new().small().into_any_element()
            } else {
                Icon::new(if playing { Lucide::Pause } else { Lucide::Play }).small().into_any_element()
            })
            .on_click(cx.listener(|player, _, _, cx| {
                cx.stop_propagation();
                player.playback.update(cx, |playback, cx| playback.toggle(cx));
            }))
    }

    /// Rail windows: a full-width bar under the rail and the content. What is playing on the left
    /// (opens Now Playing), transport over the seek bar in the middle, sound · speed · sleep · volume
    /// on the right; a click on the bar's background opens Now Playing too.
    fn render_bar(&mut self, item: QueueItem, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let playback = self.playback.read(cx);
        let theme = cx.theme().clone();
        let state = playback.state.clone();
        let (position, duration) = (playback.position_ms, playback.duration_ms);
        let has_next = playback.has_next();
        let (loop_mode, shuffle) = (playback.nav.loop_mode, playback.nav.shuffle);
        let sleep_remaining = playback.sleep_remaining();
        let sleep_after_current = playback.sleep_after_current;
        let volume = playback.volume;
        let tuned = (playback.effects.speed, playback.effects.pitch) != (1.0, 1.0);
        let subtitle = Self::subtitle(&item, &state, cx);
        let side = width >= BAR_SIDE_MIN;
        let sleep = width >= BAR_SLEEP_MIN;
        let info_width = (width * BAR_INFO_SHARE).max(BAR_INFO_MIN);
        let repeat_label = t(match loop_mode {
            LoopMode::None => Str::CdRepeatOff,
            LoopMode::All => Str::CdRepeatAll,
            LoopMode::One => Str::CdRepeatOne,
        });
        let info = h_flex()
            .id("now-playing-toggle")
            .role(Role::Button)
            .keyboard_focusable(theme.ring)
            .aria_label(Self::now_playing_label(&item))
            .w(px(info_width))
            .flex_shrink_0()
            .gap_2()
            .min_w_0()
            .rounded(px(4.))
            .cursor_pointer()
            .on_click(cx.listener(|_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(ToggleNowPlaying);
            }))
            .child(Self::cover(&item, cx).size(px(48.)).rounded(px(4.)))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_semibold().truncate().child(item.title.clone()))
                    .when_some(subtitle, |column, (line, color)| {
                        column.child(div().text_xs().text_color(color).truncate().child(line))
                    }),
            );
        let transport = v_flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("shuffle")
                            .ghost()
                            .small()
                            .icon(Icon::new(Lucide::Shuffle))
                            .selected(shuffle)
                            .tooltip(t(Str::CdShuffle))
                            .accessibility_label(t(Str::CdShuffle))
                            .on_click(cx.listener(move |player, _, _, cx| {
                                player.playback.update(cx, |playback, cx| playback.set_shuffle(!shuffle, cx))
                            })),
                    )
                    .child(
                        Button::new("previous")
                            .ghost()
                            .icon(Icon::new(Lucide::SkipBack))
                            .tooltip(t(Str::CdPrevious))
                            .accessibility_label(t(Str::CdPrevious))
                            .on_click(cx.listener(|player, _, _, cx| {
                                player.playback.update(cx, |playback, cx| playback.previous(cx))
                            })),
                    )
                    .child(self.play_circle(&state, cx))
                    .child(
                        Button::new("next")
                            .ghost()
                            .icon(Icon::new(Lucide::SkipForward))
                            .tooltip(t(Str::CdNext))
                            .accessibility_label(t(Str::CdNext))
                            .disabled(!has_next)
                            .on_click(cx.listener(|player, _, _, cx| {
                                player.playback.update(cx, |playback, cx| playback.next(cx))
                            })),
                    )
                    .child(
                        Button::new("repeat")
                            .ghost()
                            .small()
                            .icon(Icon::new(if loop_mode == LoopMode::One { Lucide::Repeat1 } else { Lucide::Repeat }))
                            .selected(loop_mode != LoopMode::None)
                            .accessibility_label(repeat_label)
                            .tooltip(repeat_label)
                            .on_click(cx.listener(move |player, _, _, cx| {
                                player
                                    .playback
                                    .update(cx, |playback, cx| playback.set_loop_mode(loop_mode.cycled(), cx))
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .max_w(px(BAR_SEEK_MAX))
                    .gap_2()
                    .items_center()
                    .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(clock(position)))
                    .child(
                        div().flex_1().child(Slider::new(&self.seek).accessibility_label(t(Str::A11yPlaybackPosition))),
                    )
                    .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child(clock(duration))),
            );
        let side_controls = side.then(|| {
            h_flex()
                .flex_shrink_0()
                .items_center()
                .child(
                    Button::new("equalizer")
                        .ghost()
                        .small()
                        .icon(Icon::new(Lucide::SlidersHorizontal))
                        .tooltip(t(Str::SoundEq))
                        .accessibility_label(t(Str::SoundEq))
                        .on_click(cx.listener(|player, _, window, cx| {
                            crate::sound_panel::SoundPanel::open(player.playback.clone(), window, cx)
                        })),
                )
                .child(
                    Button::new("speed-pitch")
                        .ghost()
                        .small()
                        .icon(Icon::new(Lucide::Gauge))
                        .selected(tuned)
                        .tooltip(t(Str::MenuSpeedPitch))
                        .accessibility_label(t(Str::MenuSpeedPitch))
                        .on_click(cx.listener(|player, _, window, cx| {
                            crate::sound_panel::SoundPanel::open(player.playback.clone(), window, cx)
                        })),
                )
                .when(sleep, |row| row.child(self.sleep_button(sleep_remaining, sleep_after_current)))
                .child(
                    div().px_1().child(
                        Icon::new(if volume <= 0. {
                            Lucide::VolumeX
                        } else if volume < 0.5 {
                            Lucide::Volume1
                        } else {
                            Lucide::Volume2
                        })
                        .small()
                        .text_color(theme.muted_foreground),
                    ),
                )
                .child(div().w(px(BAR_VOLUME_WIDTH)).child(
                    Slider::new(&self.volume).accessibility_label(tf(Str::SoundVolume, &[&((volume * 100.0) as i32)])),
                ))
        });
        h_flex()
            .id("player-bar")
            .w_full()
            .gap_4()
            .px_4()
            .py_2()
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ToggleNowPlaying)))
            .child(info)
            .child(transport)
            .children(side_controls)
            .into_any_element()
    }

    /// Drawer windows: the phone mini player. The 16:9 cover, then a thin progress line over the
    /// title with play/pause and next; a click anywhere else opens Now Playing.
    fn render_phone(&mut self, item: QueueItem, cx: &mut Context<Self>) -> AnyElement {
        let playback = self.playback.read(cx);
        let theme = cx.theme().clone();
        let state = playback.state.clone();
        let fraction = if playback.duration_ms > 0 {
            (playback.position_ms as f32 / playback.duration_ms as f32).clamp(0., 1.)
        } else {
            0.
        };
        let has_next = playback.has_next();
        let playing = matches!(state, PlayState::Playing | PlayState::Buffering);
        let subtitle = Self::subtitle(&item, &state, cx);
        h_flex()
            .id("now-playing-toggle")
            .role(Role::Button)
            .keyboard_focusable(theme.ring)
            .aria_label(Self::now_playing_label(&item))
            .w_full()
            .h(px(56.))
            .border_t_1()
            .border_color(theme.border)
            .cursor_pointer()
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ToggleNowPlaying)))
            .child(Self::cover(&item, cx).h_full().aspect_ratio(16. / 9.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(theme.sidebar)
                    .child(
                        div()
                            .w_full()
                            .h(px(4.))
                            .bg(theme.border)
                            .child(div().h_full().w(relative(fraction)).bg(theme.primary)),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .pl_3()
                            .pr_2()
                            .items_center()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().font_semibold().truncate().child(item.title.clone()))
                                    .when_some(subtitle, |column, (line, color)| {
                                        column.child(div().text_xs().text_color(color).truncate().child(line))
                                    }),
                            )
                            .child(if state == PlayState::Buffering {
                                div()
                                    .size(px(36.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(Spinner::new())
                                    .into_any_element()
                            } else {
                                Button::new("play-pause")
                                    .ghost()
                                    .icon(Icon::new(if playing { Lucide::Pause } else { Lucide::Play }))
                                    .accessibility_label(Self::play_label(&state))
                                    .tooltip(Self::play_label(&state))
                                    .on_click(cx.listener(|player, _, _, cx| {
                                        player.playback.update(cx, |playback, cx| playback.toggle(cx))
                                    }))
                                    .into_any_element()
                            })
                            .child(
                                Button::new("next")
                                    .ghost()
                                    .icon(Icon::new(Lucide::SkipForward))
                                    .accessibility_label(t(Str::CdNext))
                                    .tooltip(t(Str::CdNext))
                                    .disabled(!has_next)
                                    .on_click(cx.listener(|player, _, _, cx| {
                                        player.playback.update(cx, |playback, cx| playback.next(cx))
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for MiniPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(item) = self.playback.read(cx).current().cloned() else { return div().into_any_element() };
        let viewport = window.viewport_size();
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        if width >= 750. && height >= 480. { self.render_bar(item, width, cx) } else { self.render_phone(item, cx) }
    }
}
