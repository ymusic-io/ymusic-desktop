//! Now Playing, laid out like the JVM desktop app. Wide (W > H and W >= 640): the video on the left
//! with overlay controls, the queue pane on the right (`clamp(0.3W, 320, 480)`) under its top bar.
//! Narrow: top bar, video full width, title, then the queue.
//! The video box is painted by `video_layer`: a native view behind the window on macOS, CPU pictures elsewhere.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::slider::{SliderEvent, SliderState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Selectable, Sizable, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t, tf};

use crate::a11y::{FocusList, KeyboardFocusable};
use crate::download_dialog::DownloadTarget;
use crate::mini_player::clock;
use crate::playback::{
    AUTO_TRANSLATE_KIND, CaptionTrack, PlayState, Playback, QUALITY_AUDIO_ONLY, QUALITY_AUTO, QueueItem,
};
use crate::queue_nav::LoopMode;
use crate::sponsor_block::SponsorBlock;
use crate::video_layer::VideoLayer;

const CONTROLS_HIDE_AFTER: Duration = Duration::from_secs(3);
const SKIP_MS: i64 = 10_000;
const SLEEP_PRESET_MINUTES: [u64; 5] = [15, 30, 45, 60, 90];
const PLAY_BUTTON: f32 = 58.;

pub enum NowPlayingEvent {
    Close,
    Download(DownloadTarget),
    OpenSound,
    /// A playback failure wants a signed-in viewer.
    SignIn,
    /// Open the video's details and comments.
    Details(String),
}

pub struct NowPlaying {
    playback: Entity<Playback>,
    sponsor_block: Entity<SponsorBlock>,
    queue_scroll: ScrollHandle,
    queue_focus: FocusList,
    video_layer: Option<Rc<VideoLayer>>,
    /// The video box as last painted, to report viewport changes to the player.
    video_box: Rc<Cell<(i32, i32)>>,
    reported_box: (i32, i32),
    seek: Entity<SliderState>,
    dragging_seek: bool,
    touched_at: Instant,
    controls_were_visible: bool,
    fullscreen: bool,
    _subscriptions: Vec<Subscription>,
    _controls_timer: Task<()>,
}

impl EventEmitter<NowPlayingEvent> for NowPlaying {}

/// Cues shown at once, newest at the bottom.
const CAPTION_ROWS: usize = 2;
/// The JVM caption style's default box opacity.
const CAPTION_BACKGROUND_OPACITY: f32 = 0.55;
/// Clears the video controls' bottom strip.
const CAPTION_ABOVE_CONTROLS: f32 = 72.;
/// Two caption rows: 24 line height plus the box's 2 + 2 padding, each.
const CAPTION_TWO_ROWS: f32 = 56.;
/// The picture's shape until the stream reports its own.
const VIDEO_RATIO: f32 = 16. / 9.;

fn icon_button(id: impl Into<ElementId>, icon: impl Into<Icon>, label: &'static str) -> Button {
    Button::new(id).ghost().icon(icon).accessibility_label(label).tooltip(label)
}

impl NowPlaying {
    pub fn new(
        playback: Entity<Playback>,
        sponsor_block: Entity<SponsorBlock>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let seek = cx.new(|_| SliderState::new().min(0.).max(1.).step(0.001));
        let subscriptions = vec![
            cx.observe(&sponsor_block, |_, _, cx| cx.notify()),
            cx.observe_in(&playback, window, |view, playback, window, cx| {
                if !view.dragging_seek {
                    let playback = playback.read(cx);
                    let fraction = if playback.duration_ms > 0 {
                        (playback.position_ms as f32 / playback.duration_ms as f32).clamp(0., 1.)
                    } else {
                        0.
                    };
                    view.seek.update(cx, |slider, cx| slider.set_value(fraction, window, cx));
                }
                cx.notify();
            }),
            cx.subscribe(&seek, |view, _, event: &SliderEvent, cx| match event {
                SliderEvent::Change(_) => {
                    view.dragging_seek = true;
                    view.touch(cx);
                }
                SliderEvent::Release(value) => {
                    view.dragging_seek = false;
                    let fraction = value.end();
                    view.playback.update(cx, |playback, cx| {
                        let target = (playback.duration_ms as f32 * fraction) as i64;
                        playback.seek(target, cx);
                    });
                }
            }),
        ];
        let executor = cx.background_executor().clone();
        let controls_timer = cx.spawn(async move |this, cx| {
            loop {
                executor.timer(Duration::from_millis(500)).await;
                let alive = this.update(cx, |view, cx| {
                    if view.controls_visible(cx) != view.controls_were_visible {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        let queue_scroll = ScrollHandle::new();
        Self {
            playback,
            sponsor_block,
            queue_scroll: queue_scroll.clone(),
            queue_focus: FocusList::new(Some(queue_scroll)),
            video_layer: None,
            video_box: Rc::new(Cell::new((0, 0))),
            reported_box: (0, 0),
            seek,
            dragging_seek: false,
            touched_at: Instant::now(),
            controls_were_visible: true,
            fullscreen: false,
            _subscriptions: subscriptions,
            _controls_timer: controls_timer,
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
        #[cfg(target_os = "macos")]
        {
            let pointer = layer.as_ptr();
            // SAFETY: the layer lives in this view for the app's lifetime, past Playback's use of it.
            self.playback.update(cx, |playback, _| unsafe { playback.attach_video_layer(pointer) });
        }
        #[cfg(not(target_os = "macos"))]
        {
            use futures::StreamExt;
            let (sender, mut pictures) = futures::channel::mpsc::unbounded::<()>();
            if let Some(presenter) = layer.presenter(move || {
                let _ = sender.unbounded_send(());
            }) {
                cx.spawn(async move |view, cx| {
                    while pictures.next().await.is_some() {
                        if view.update(cx, |_, cx| cx.notify()).is_err() {
                            break;
                        }
                    }
                })
                .detach();
                self.playback.update(cx, |playback, _| playback.attach_presenter(presenter));
            }
        }
        self.video_layer = Some(layer);
    }

    fn touch(&mut self, cx: &mut Context<Self>) {
        self.touched_at = Instant::now();
        cx.notify();
    }

    /// Shown while paused or seeking, and for a few seconds after the last pointer move.
    fn controls_visible(&self, cx: &App) -> bool {
        !self.playback.read(cx).is_playing() || self.dragging_seek || self.touched_at.elapsed() < CONTROLS_HIDE_AFTER
    }

    fn render_play_button(&self, playing: bool, buffering: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let label = t(if playing { Str::CdPause } else { Str::CdPlay });
        div()
            .id("np-play")
            .role(Role::Button)
            .aria_label(label)
            .keyboard_focusable(theme.ring)
            .size(px(PLAY_BUTTON))
            .flex_shrink_0()
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(theme.primary)
            .text_color(theme.primary_foreground)
            .opacity(if buffering { 0.6 } else { 1. })
            .child(Icon::new(if playing { Lucide::Pause } else { Lucide::Play }).large())
            .on_click(cx.listener(|view, _, _, cx| view.playback.update(cx, |playback, cx| playback.toggle(cx))))
    }

    fn render_video(&mut self, current: &QueueItem, fill: bool, cx: &mut Context<Self>) -> AnyElement {
        let playback = self.playback.read(cx);
        let (video_on, playing) = (playback.video, playback.is_playing());
        let buffering = playback.state == PlayState::Buffering;
        let (position, duration) = (playback.position_ms, playback.duration_ms);
        let layer = self.video_layer.clone();
        let video_box = self.video_box.clone();
        let show_video = video_on && layer.is_some();
        let controls = self.controls_visible(cx);
        self.controls_were_visible = controls;
        let fullscreen = self.fullscreen;

        let transport = h_flex()
            .gap_6()
            .items_center()
            .child(icon_button("np-rewind", Lucide::RotateCcw, t(Str::CdRewind10)).on_click(
                cx.listener(|view, _, _, cx| view.playback.update(cx, |playback, cx| playback.seek_by(-SKIP_MS, cx))),
            ))
            .child(
                icon_button("np-previous", Lucide::SkipBack, t(Str::CdPrevious)).on_click(
                    cx.listener(|view, _, _, cx| view.playback.update(cx, |playback, cx| playback.previous(cx))),
                ),
            )
            .child(self.render_play_button(playing, buffering, cx))
            .child(
                icon_button("np-next", Lucide::SkipForward, t(Str::CdNext))
                    .on_click(cx.listener(|view, _, _, cx| view.playback.update(cx, |playback, cx| playback.next(cx)))),
            )
            .child(icon_button("np-forward", Lucide::RotateCw, t(Str::CdForward10)).on_click(
                cx.listener(|view, _, _, cx| view.playback.update(cx, |playback, cx| playback.seek_by(SKIP_MS, cx))),
            ));
        let bottom = v_flex()
            .px(px(11.))
            .pb_2()
            .gap_1()
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(div().text_sm().child(format!("{} / {}", clock(position), clock(duration))))
                    .child(div().flex_1())
                    .when_some(self.caption_button(cx), |row, button| row.child(button))
                    .child(
                        icon_button(
                            "np-fullscreen",
                            if fullscreen { Lucide::Minimize } else { Lucide::Maximize },
                            t(if fullscreen { Str::CdExitFullscreen } else { Str::CdFullscreen }),
                        )
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.fullscreen = !view.fullscreen;
                            window.toggle_fullscreen();
                            cx.notify();
                        })),
                    ),
            )
            .child(Slider::new(&self.seek).accessibility_label(t(Str::A11yPlaybackPosition)));
        let overlay = div()
            .absolute()
            .inset_0()
            .bg(black().opacity(0.35))
            .text_color(white())
            .child(
                v_flex()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .px_4()
                    .pt(px(11.))
                    .child(div().font_semibold().truncate().child(current.title.clone()))
                    .when_some(current.artist.clone(), |column, artist| {
                        column.child(div().text_sm().opacity(0.8).truncate().child(artist))
                    }),
            )
            .child(div().absolute().inset_0().flex().items_center().justify_center().child(transport))
            .child(div().absolute().bottom_0().left_0().right_0().child(bottom));

        div()
            .id("video-area")
            .relative()
            .overflow_hidden()
            .map(|frame| if fill { frame.size_full() } else { frame.w_full().aspect_ratio(16. / 9.) })
            .when_some(layer.clone().filter(|_| show_video), |frame, layer| layer.underlay(frame))
            .when(!show_video, |frame| {
                frame.bg(black()).when_some(current.thumb(i32::MAX), |frame, url| {
                    frame.child(img(url).size_full().object_fit(ObjectFit::Contain))
                })
            })
            .on_mouse_move(cx.listener(|view, _, _, cx| view.touch(cx)))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, _, _| {
                        if let Some(layer) = &layer {
                            layer.place(bounds, show_video);
                        }
                        let width = f32::from(bounds.size.width) as i32;
                        let height = f32::from(bounds.size.height) as i32;
                        video_box.set((width, height));
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .when_some(self.caption_overlay(controls, cx), |frame, captions| frame.child(captions))
            .when(controls, |frame| frame.child(overlay))
            .when_some(self.failure_panel(controls, cx), |frame, panel| frame.child(panel))
            .into_any_element()
    }

    /// The CC control, absent when the video has no caption tracks: Off, each track, and YouTube's
    /// auto-translations in a submenu; a pick turns captions on.
    fn caption_button(&self, cx: &App) -> Option<AnyElement> {
        let playback = self.playback.read(cx);
        if playback.caption_tracks.is_empty() {
            return None;
        }
        let (visible, selected) = (playback.captions, playback.caption_selected.clone());
        let is_on = move |track: &CaptionTrack| {
            visible && selected.as_deref().is_some_and(|id| id == track.vss_id || id == track.language_code)
        };
        let (translations, tracks): (Vec<CaptionTrack>, Vec<CaptionTrack>) =
            playback.caption_tracks.iter().cloned().partition(|track| track.kind == AUTO_TRANSLATE_KIND);
        let on_track = tracks.iter().chain(&translations).find(|track| is_on(track)).map(|track| track.display_name.clone());
        let state = on_track.unwrap_or_else(|| t(Str::CaptionsOff).to_owned());
        let label = format!("{}, {state}", t(Str::CdCaptions));
        let target = self.playback.clone();
        Some(
            Button::new("np-captions")
                .ghost()
                .icon(Lucide::Captions)
                .selected(visible)
                .accessibility_label(label.clone())
                .tooltip(label)
                .dropdown_menu_with_anchor(Anchor::BottomRight, move |mut menu, window, cx| {
                    let off = target.clone();
                    menu = menu.item(
                        PopupMenuItem::new(t(Str::CaptionsOff))
                            .checked(!visible)
                            .on_click(move |_, _, cx| off.update(cx, |playback, cx| playback.set_captions(false, cx))),
                    );
                    let pick = |track: &CaptionTrack| {
                        let (playback, id) = (target.clone(), track.vss_id.clone());
                        PopupMenuItem::new(track.display_name.clone())
                            .checked(is_on(track))
                            .on_click(move |_, _, cx| playback.update(cx, |playback, cx| playback.select_caption(id.clone(), cx)))
                    };
                    for track in &tracks {
                        menu = menu.item(pick(track));
                    }
                    if !translations.is_empty() {
                        let current = translations.iter().find(|track| is_on(track)).map(|track| track.display_name.clone());
                        let title = match current {
                            Some(name) => format!("{} · {name}", t(Str::CaptionsAutoTranslate)),
                            None => t(Str::CaptionsAutoTranslate).to_owned(),
                        };
                        let items: Vec<PopupMenuItem> = translations.iter().map(&pick).collect();
                        let items = std::rc::Rc::new(std::cell::RefCell::new(Some(items)));
                        menu = menu.submenu(title, window, cx, move |mut submenu, _, _| {
                            for item in items.borrow_mut().take().into_iter().flatten() {
                                submenu = submenu.item(item);
                            }
                            submenu.max_h(px(360.)).scrollable(true)
                        });
                    }
                    menu
                })
                .into_any_element(),
        )
    }

    /// The failure on a dark panel at the picture's bottom, above the controls strip while it shows:
    /// the message, then Sign in and Retry when they apply.
    fn failure_panel(&self, controls: bool, cx: &mut Context<Self>) -> Option<Div> {
        let PlayState::Failed(failure) = &self.playback.read(cx).state else { return None };
        let failure = failure.clone();
        let accent = cx.theme().primary;
        let action = |id: &'static str, label: &'static str| {
            Button::new(id).ghost().small().label(label).text_color(accent).font_semibold()
        };
        let mut actions = h_flex().gap_1().items_center();
        if failure.needs_sign_in() {
            actions = actions.child(
                action("np-error-signin", t(Str::SigninButton))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NowPlayingEvent::SignIn))),
            );
        }
        if failure.retry {
            actions = actions.child(action("np-error-retry", t(Str::Retry)).on_click(
                cx.listener(|view, _, _, cx| view.playback.update(cx, |playback, cx| playback.retry(cx))),
            ));
        }
        Some(
            div().absolute().left_0().right_0().bottom(px(if controls { CAPTION_ABOVE_CONTROLS } else { 44. })).flex().justify_center().px_4().child(
                v_flex()
                    .items_center()
                    .max_w(px(560.))
                    .px(px(14.))
                    .py_2()
                    .rounded(px(8.))
                    .bg(black().opacity(0.8))
                    .child(
                        div()
                            .id("np-error")
                            .role(Role::Alert)
                            .aria_label(failure.text())
                            .text_color(white())
                            .text_size(px(13.))
                            .text_center()
                            .line_clamp(2)
                            .child(failure.text()),
                    )
                    .child(actions),
            ),
        )
    }

    /// The cues on the picture's bottom, left-aligned at YouTube's inset, newest at the bottom;
    /// above the controls strip while it shows.
    fn caption_overlay(&self, controls: bool, cx: &App) -> Option<Div> {
        let playback = self.playback.read(cx);
        if !playback.captions || playback.caption_lines.is_empty() {
            return None;
        }
        let (width, height) = self.video_box.get();
        let (width, height) = (width as f32, height as f32);
        let inset = if width.min(height) < 600. {
            if height > width { 0.03 } else { 0.05 }
        } else {
            0.1 + 0.1 * ((width - 600.) / (1920. - 600.)).clamp(0., 1.)
        };
        // The picture is contain-fit: captions go in the bar under it when two rows fit there clear
        // of the controls, else over its bottom.
        let (picture_width, picture_height) =
            if width / height.max(1.) <= VIDEO_RATIO { (width, width / VIDEO_RATIO) } else { (height * VIDEO_RATIO, height) };
        let bar = (height - picture_height) / 2.;
        let under_picture = bar >= CAPTION_TWO_ROWS + CAPTION_ABOVE_CONTROLS + 8.;
        let cues = playback.caption_lines.iter().rev().take(CAPTION_ROWS).rev();
        let mut column = v_flex().items_start().gap_1();
        for (_, text) in cues {
            column = column.child(
                div()
                    .px_2()
                    .py(px(2.))
                    .rounded(px(4.))
                    .bg(black().opacity(CAPTION_BACKGROUND_OPACITY))
                    .text_color(white())
                    .text_size(px(16.))
                    .line_height(px(24.))
                    .child(text.clone()),
            );
        }
        let side = (width - picture_width) / 2. + picture_width * inset;
        let placed = div().absolute().left_0().right_0().px(px(side));
        Some(if under_picture {
            placed.top(px(bar + picture_height + 8.)).child(column)
        } else {
            let bottom = (bar + 12.).max(if controls { CAPTION_ABOVE_CONTROLS } else { 12. });
            placed.bottom(px(bottom)).child(column)
        })
    }

    /// Close, then download · sleep · quality · ⋮ on the right.
    fn render_top_bar(&self, current: &QueueItem, cx: &mut Context<Self>) -> AnyElement {
        let playback = self.playback.read(cx);
        let sleep_remaining = playback.sleep_remaining();
        let sleep_after_current = playback.sleep_after_current;
        let sleep_armed = sleep_remaining.is_some() || sleep_after_current;
        let video = playback.video;
        let sponsor_whitelisted = self.sponsor_block.read(cx).current.as_ref().map(|(_, whitelisted)| *whitelisted);
        let target = current.video_id().map(|video_id| DownloadTarget {
            video_id: video_id.to_owned(),
            title: current.title.clone(),
            artist: current.artist.clone(),
            thumbnail_url: current.thumb(640).map(|url| url.to_string()),
        });

        let sleep_tooltip = match sleep_remaining {
            Some(left) => tf(Str::SleepPausingIn, &[&clock(left.as_millis() as i64)]),
            None if sleep_after_current => t(Str::SleepPausingAfterTrack).to_owned(),
            None => t(Str::CdSleepTimer).to_owned(),
        };
        let sleep_playback = self.playback.clone();
        let sleep = Button::new("np-sleep")
            .ghost()
            .icon(Lucide::Timer)
            .selected(sleep_armed)
            .accessibility_label(t(Str::CdSleepTimer))
            .tooltip(sleep_tooltip)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                for minutes in SLEEP_PRESET_MINUTES {
                    let playback = sleep_playback.clone();
                    menu =
                        menu.item(PopupMenuItem::new(tf(Str::SleepMinutes, &[&minutes])).on_click(move |_, _, cx| {
                            playback.update(cx, |playback, cx| {
                                playback.set_sleep_timer(Some(Duration::from_secs(minutes * 60)), cx)
                            })
                        }));
                }
                let end = sleep_playback.clone();
                menu = menu.item(PopupMenuItem::new(t(Str::SleepEndOfTrack)).checked(sleep_after_current).on_click(
                    move |_, _, cx| end.update(cx, |playback, cx| playback.set_sleep_after_current(true, cx)),
                ));
                if sleep_armed {
                    let off = sleep_playback.clone();
                    menu =
                        menu.separator().item(PopupMenuItem::new(t(Str::SleepTurnOff)).on_click(move |_, _, cx| {
                            off.update(cx, |playback, cx| playback.set_sleep_timer(None, cx))
                        }));
                }
                menu
            });

        let quality_playback = self.playback.clone();
        let (choices, selected, current) = {
            let playback = self.playback.read(cx);
            let label = |quality: &str, fallback: &str| match quality {
                QUALITY_AUTO => t(Str::VideoQualityAuto).to_owned(),
                QUALITY_AUDIO_ONLY => t(Str::AudioOnly).to_owned(),
                _ => fallback.to_owned(),
            };
            let mut choices = vec![(QUALITY_AUDIO_ONLY.to_owned(), t(Str::AudioOnly).to_owned())];
            if playback.quality_options.is_empty() {
                choices.push((QUALITY_AUTO.to_owned(), t(Str::VideoQualityAuto).to_owned()));
            } else {
                choices.extend(playback.quality_options.iter().map(|(quality, text)| (quality.clone(), label(quality, text))));
            }
            let selected = if video { playback.quality_preferred.clone() } else { QUALITY_AUDIO_ONLY.to_owned() };
            let current = if !video {
                t(Str::AudioOnly).to_owned()
            } else {
                let actual = playback.quality_actual.clone().unwrap_or_else(|| QUALITY_AUTO.to_owned());
                choices.iter().find(|(quality, _)| *quality == actual).map(|(_, text)| text.clone()).unwrap_or_else(|| label(&actual, &actual))
            };
            (choices, selected, current)
        };
        let quality = Button::new("np-quality")
            .outline()
            .xsmall()
            .label(current.clone())
            .accessibility_label(format!("{}, {current}", t(Str::DownloadQuality)))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                for (quality, text) in &choices {
                    let (playback, pick) = (quality_playback.clone(), quality.clone());
                    menu = menu.item(
                        PopupMenuItem::new(text.clone())
                            .checked(*quality == selected)
                            .on_click(move |_, _, cx| playback.update(cx, |playback, cx| playback.set_quality(pick.clone(), cx))),
                    );
                }
                menu
            });

        let view = cx.entity().downgrade();
        let more_target = target.clone();
        let more = icon_button("np-more", IconName::EllipsisVertical, t(Str::CdMore)).dropdown_menu_with_anchor(
            Anchor::TopRight,
            move |mut menu, _, _| {
                if let Some(target) = &more_target {
                    let (details, video_id) = (view.clone(), target.video_id.clone());
                    menu = menu.item(PopupMenuItem::new(t(Str::MenuDetailsComments)).on_click(move |_, _, cx| {
                        details.update(cx, |_, cx| cx.emit(NowPlayingEvent::Details(video_id.clone()))).ok();
                    }));
                    let link = format!("https://youtu.be/{}", target.video_id);
                    menu = menu.item(PopupMenuItem::new(t(Str::CdShare)).on_click(move |_, window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                        window.push_notification(t(Str::LinkCopied), cx);
                    }));
                    let query = format!("{} lyrics", target.title);
                    menu = menu.item(PopupMenuItem::new(t(Str::MenuSearchLyrics)).on_click(move |_, _, cx| {
                        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
                        cx.open_url(&format!("https://www.google.com/search?q={encoded}"));
                    }));
                    menu = menu.separator();
                }
                let sound = view.clone();
                menu.item(PopupMenuItem::new(t(Str::MenuSpeedPitch)).on_click(move |_, _, cx| {
                    sound.update(cx, |_, cx| cx.emit(NowPlayingEvent::OpenSound)).ok();
                }))
            },
        );

        h_flex()
            .px_2()
            .py_1()
            .gap(px(5.))
            .items_center()
            .child(
                icon_button("np-close", IconName::ChevronDown, t(Str::CdMinimize))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NowPlayingEvent::Close))),
            )
            .child(div().flex_1())
            .when_some(target, |bar, target| {
                bar.child(
                    icon_button("np-download", Lucide::Download, t(Str::CdDownload))
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(NowPlayingEvent::Download(target.clone())))),
                )
            })
            .when_some(sponsor_whitelisted, |bar, whitelisted| {
                let state = t(if whitelisted { Str::SponsorBlockExcluded } else { Str::SponsorBlockIncluded });
                bar.child(
                    Button::new("np-sponsorblock")
                        .ghost()
                        .icon(if whitelisted { Lucide::ShieldOff } else { Lucide::ShieldCheck })
                        .selected(!whitelisted)
                        .accessibility_label(t(Str::SettingsSponsorBlock))
                        .tooltip(state)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.sponsor_block.update(cx, |sponsor, cx| sponsor.set_whitelisted(!whitelisted, cx))
                        })),
                )
            })
            .child(sleep)
            .child(quality)
            .child(more)
            .into_any_element()
    }

    /// "Up next", n of total, then repeat and shuffle.
    fn render_queue_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let playback = self.playback.read(cx);
        let (loop_mode, shuffle) = (playback.nav.loop_mode, playback.nav.shuffle);
        let position = tf(Str::A11yXOfY, &[&(playback.index + 1), &playback.queue.len()]);
        let theme = cx.theme();
        h_flex()
            .px(px(20.))
            .py(px(6.))
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(div().id("np-up-next").role(Role::Heading).font_semibold().child(t(Str::UpNext)))
            .child(div().text_sm().text_color(theme.muted_foreground).child(position))
            .child(div().flex_1())
            .child(
                icon_button(
                    "np-repeat",
                    if loop_mode == LoopMode::One { Lucide::Repeat1 } else { Lucide::Repeat },
                    t(match loop_mode {
                        LoopMode::None => Str::CdRepeatOff,
                        LoopMode::All => Str::CdRepeatAll,
                        LoopMode::One => Str::CdRepeatOne,
                    }),
                )
                .selected(loop_mode != LoopMode::None)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.playback.update(cx, |playback, cx| playback.set_loop_mode(loop_mode.cycled(), cx))
                })),
            )
            .child(icon_button("np-shuffle", Lucide::Shuffle, t(Str::CdShuffle)).selected(shuffle).on_click(
                cx.listener(move |view, _, _, cx| {
                    view.playback.update(cx, |playback, cx| playback.set_shuffle(!shuffle, cx))
                }),
            ))
            .into_any_element()
    }

    fn render_queue(&self, large: bool, cx: &mut Context<Self>) -> AnyElement {
        let playback = self.playback.read(cx);
        let (queue, index) = (playback.queue.clone(), playback.index);
        let theme = cx.theme().clone();
        let thumb = if large { 60. } else { 48. };
        let mut list = v_flex();
        for (position, item) in queue.iter().enumerate() {
            let active = position == index;
            let label = [Some(item.title.clone()), item.artist.clone(), item.duration_label.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
            let detail = [item.duration_label.clone(), item.artist.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            let menu_playback = self.playback.clone();
            let menu_item = item.clone();
            let more =
                icon_button(("np-queue-more", position), IconName::EllipsisVertical, t(Str::CdMore))
                    .small()
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                        if !active {
                            let (next, item) = (menu_playback.clone(), menu_item.clone());
                            menu = menu.item(PopupMenuItem::new(t(Str::QueueNext)).on_click(move |_, _, cx| {
                                next.update(cx, |playback, cx| {
                                    playback.remove_at(position, cx);
                                    playback.queue_next(item.clone(), cx);
                                })
                            }));
                        }
                        let remove = menu_playback.clone();
                        menu.item(PopupMenuItem::new(t(Str::RemoveFromQueue)).on_click(move |_, _, cx| {
                            remove.update(cx, |playback, cx| playback.remove_at(position, cx))
                        }))
                    });
            let row =
                h_flex().id(("np-queue-item", position)).role(Role::ListItem).aria_label(label).aria_selected(active);
            list = list.child(
                self.queue_focus
                    .item(position, row, cx)
                    .px_4()
                    .py(px(if large { 10. } else { 8. }))
                    .gap(px(if large { 12. } else { 8. }))
                    .items_center()
                    .cursor_pointer()
                    .when(active, |row| row.bg(theme.secondary))
                    .hover(|style| style.bg(theme.secondary))
                    .child(
                        div()
                            .size(px(thumb))
                            .flex_shrink_0()
                            .rounded(px(6.))
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
                            .when(!detail.is_empty(), |column| {
                                column
                                    .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(detail))
                            }),
                    )
                    .child(more)
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.playback.update(cx, |playback, cx| playback.jump_to(position, cx))
                    })),
            );
        }
        let list = self.queue_focus.container(list, 0..queue.len(), cx);
        div()
            .id("np-queue")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.queue_scroll)
            .child(list)
            .into_any_element()
    }
}

impl Render for NowPlaying {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_video_layer(window, cx);
        #[cfg(not(target_os = "macos"))]
        if let Some(layer) = &self.video_layer
            && let Err(error) = layer.refresh(window)
        {
            let log = &cx.global::<crate::graph::AppGraph>().log;
            log.warn("video picture could not leave the atlas", Some(error.as_ref()));
        }
        let painted = self.video_box.get();
        if painted.0 > 0 && painted != self.reported_box {
            self.reported_box = painted;
            self.playback.update(cx, |playback, cx| playback.set_viewport(painted.0, painted.1, cx));
        }
        let Some(current) = self.playback.read(cx).current().cloned() else { return div().into_any_element() };
        if self.fullscreen && !window.is_fullscreen() {
            self.fullscreen = false;
        }
        if self.fullscreen {
            return div().size_full().child(self.render_video(&current, true, cx)).into_any_element();
        }
        let viewport = window.viewport_size();
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        let theme = cx.theme().clone();
        let top_bar = self.render_top_bar(&current, cx);
        let header = self.render_queue_header(cx);
        if width > height && width >= 640. {
            let pane = (0.3 * width).clamp(320., 480.);
            let queue = self.render_queue(pane >= 380., cx);
            let video = self.render_video(&current, true, cx);
            return h_flex()
                .size_full()
                .child(div().flex_1().min_w_0().h_full().child(video))
                .child(
                    v_flex()
                        .w(px(pane))
                        .h_full()
                        .flex_shrink_0()
                        .bg(theme.background)
                        .border_l_1()
                        .border_color(theme.border)
                        .child(top_bar)
                        .child(header)
                        .child(queue),
                )
                .into_any_element();
        }
        let video = self.render_video(&current, false, cx);
        let queue = self.render_queue(false, cx);
        v_flex()
            .size_full()
            .child(div().bg(theme.background).child(top_bar))
            .child(video)
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .bg(theme.background)
                    .child(
                        v_flex()
                            .items_center()
                            .px(px(20.))
                            .pt(px(18.))
                            .pb_1()
                            .child(div().font_semibold().truncate().child(current.title.clone()))
                            .when_some(current.artist.clone(), |column, artist| {
                                column.child(div().pt_1().text_sm().text_color(theme.muted_foreground).child(artist))
                            }),
                    )
                    .child(header)
                    .child(queue),
            )
            .into_any_element()
    }
}
