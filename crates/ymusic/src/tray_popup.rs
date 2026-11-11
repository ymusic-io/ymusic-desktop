//! The tray icon's mini player (JVM `MiniPlayer.kt`): a small card under the icon with what is
//! playing, open the app, stop (end the listening session), a seek bar and the transport. It
//! closes when it loses focus, like a system notification.

use futures::channel::mpsc::UnboundedSender;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::slider::{SliderEvent, SliderState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t};

use crate::desktop::TrayCommand;
use crate::mini_player::clock;
use crate::playback::{PlayState, Playback};
use crate::spinner::Spinner;

pub const WIDTH: f32 = 340.;
pub const HEIGHT: f32 = 132.;
const MARGIN: f32 = 8.;
const COVER: f32 = 44.;

/// The popup closed itself (focus went elsewhere, or Esc).
pub struct Closed;

pub struct TrayPopup {
    playback: Entity<Playback>,
    commands: UnboundedSender<TrayCommand>,
    seek: Entity<SliderState>,
    dragging_seek: bool,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Closed> for TrayPopup {}

impl TrayPopup {
    /// Beside the tray icon at `anchor` (logical screen bounds): under it when the icon sits in
    /// the top half of its display (the macOS menu bar), above it otherwise (a bottom taskbar),
    /// centered on it and kept inside the display. The origin is relative to the returned display.
    pub fn placement(anchor: Bounds<Pixels>, cx: &App) -> Option<(DisplayId, Bounds<Pixels>)> {
        let center = anchor.center();
        let display = cx
            .displays()
            .into_iter()
            .find(|display| display.bounds().contains(&center))
            .or_else(|| cx.primary_display())?;
        let area = display.bounds();
        let (width, height, margin) = (px(WIDTH), px(HEIGHT), px(MARGIN));
        let x = (center.x - width / 2.).max(area.left() + margin).min(area.right() - width - margin);
        let y = if center.y < area.center().y { anchor.bottom() + margin } else { anchor.top() - height - margin };
        let origin = point(x - area.origin.x, y - area.origin.y);
        Some((display.id(), Bounds::new(origin, size(width, height))))
    }

    pub fn new(
        playback: Entity<Playback>,
        commands: UnboundedSender<TrayCommand>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let seek = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.001));
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let subscriptions = vec![
            cx.observe_in(&playback, window, |popup, playback, window, cx| {
                if !popup.dragging_seek {
                    let playback = playback.read(cx);
                    let fraction = if playback.duration_ms > 0 {
                        (playback.position_ms as f32 / playback.duration_ms as f32).clamp(0., 1.)
                    } else {
                        0.
                    };
                    popup.seek.update(cx, |slider, cx| slider.set_value(fraction, window, cx));
                }
                cx.notify();
            }),
            cx.subscribe(&seek, |popup, _, event: &SliderEvent, cx| match event {
                SliderEvent::Change(_) => popup.dragging_seek = true,
                SliderEvent::Release(value) => {
                    popup.dragging_seek = false;
                    let fraction = value.end();
                    popup.playback.update(cx, |playback, cx| {
                        let target = (playback.duration_ms as f32 * fraction) as i64;
                        playback.seek(target, cx);
                    });
                }
            }),
            cx.observe_window_activation(window, |popup, window, cx| {
                if !window.is_window_active() {
                    popup.close(window, cx);
                }
            }),
        ];
        Self { playback, commands, seek, dragging_seek: false, focus, _subscriptions: subscriptions }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(Closed);
        window.remove_window();
    }

    fn command(&self, command: TrayCommand) {
        // The shell's command loop ends only with the shell itself, and this window with it.
        let _ = self.commands.unbounded_send(command);
    }
}

impl Render for TrayPopup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let playback = self.playback.read(cx);
        let item = playback.current().cloned();
        let state = playback.state.clone();
        let playing = matches!(state, PlayState::Playing | PlayState::Buffering);
        let (position, duration) = (playback.position_ms, playback.duration_ms);
        let has_next = playback.has_next();
        let title =
            item.as_ref().map_or_else(|| t(Str::MiniPlayerNothingPlaying).to_owned(), |item| item.title.clone());
        let artist = item.as_ref().and_then(|item| item.artist.clone());
        let cover = div()
            .size(px(COVER))
            .flex_shrink_0()
            .rounded(px(5.))
            .overflow_hidden()
            .bg(theme.secondary)
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(Lucide::Music).text_color(theme.muted_foreground))
            .when_some(item.as_ref().and_then(|item| item.thumb(160)), |frame, url| {
                frame.child(img(url).absolute().inset_0().size_full().object_fit(ObjectFit::Cover))
            });
        let play_label = t(if playing { Str::CdPause } else { Str::CdPlay });
        v_flex()
            .id("tray-popup")
            .track_focus(&self.focus)
            .size_full()
            .px_3()
            .py(px(10.))
            .gap_1()
            .rounded(px(8.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .on_key_down(cx.listener(|popup, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    popup.close(window, cx);
                }
            }))
            .child(
                h_flex()
                    .gap(px(10.))
                    .items_center()
                    .child(cover.relative())
                    .child(v_flex().flex_1().min_w_0().child(div().text_sm().truncate().child(title)).when_some(
                        artist,
                        |column, artist| {
                            column.child(div().text_xs().truncate().text_color(theme.muted_foreground).child(artist))
                        },
                    ))
                    .child(
                        Button::new("tray-open-app")
                            .ghost()
                            .small()
                            .icon(Icon::new(Lucide::ExternalLink))
                            .accessibility_label(t(Str::CdOpenApp))
                            .tooltip(t(Str::CdOpenApp))
                            .on_click(cx.listener(|popup, _, _, _| popup.command(TrayCommand::Open))),
                    )
                    .child(
                        Button::new("tray-dismiss")
                            .ghost()
                            .small()
                            .icon(Icon::new(Lucide::X))
                            .accessibility_label(t(Str::CdStopDismiss))
                            .tooltip(t(Str::CdStopDismiss))
                            .on_click(cx.listener(|popup, _, _, _| popup.command(TrayCommand::Stop))),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(clock(position)))
                    .child(
                        div().flex_1().child(
                            Slider::new(&self.seek)
                                .accessibility_label(t(Str::A11yPlaybackPosition))
                                .disabled(duration <= 0),
                        ),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(if duration > 0 {
                        clock(duration)
                    } else {
                        "--:--".to_owned()
                    })),
            )
            .child(
                h_flex()
                    .justify_center()
                    .items_center()
                    .gap(px(14.))
                    .child(
                        Button::new("tray-previous")
                            .ghost()
                            .small()
                            .icon(Icon::new(Lucide::SkipBack))
                            .accessibility_label(t(Str::CdPrevious))
                            .tooltip(t(Str::CdPrevious))
                            .disabled(item.is_none())
                            .on_click(cx.listener(|popup, _, _, cx| {
                                popup.playback.update(cx, |playback, cx| playback.previous(cx))
                            })),
                    )
                    .child(
                        Button::new("tray-play-pause")
                            .ghost()
                            .when(state == PlayState::Buffering, |button| button.child(Spinner::new().small()))
                            .when(state != PlayState::Buffering, |button| {
                                button.icon(Icon::new(if playing { Lucide::Pause } else { Lucide::Play }))
                            })
                            .accessibility_label(play_label)
                            .tooltip(play_label)
                            .disabled(item.is_none())
                            .on_click(cx.listener(|popup, _, _, cx| {
                                popup.playback.update(cx, |playback, cx| playback.toggle(cx))
                            })),
                    )
                    .child(
                        Button::new("tray-next")
                            .ghost()
                            .small()
                            .icon(Icon::new(Lucide::SkipForward))
                            .accessibility_label(t(Str::CdNext))
                            .tooltip(t(Str::CdNext))
                            .disabled(!has_next)
                            .on_click(cx.listener(|popup, _, _, cx| {
                                popup.playback.update(cx, |playback, cx| playback.next(cx))
                            })),
                    ),
            )
    }
}
