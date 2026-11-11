//! The viewer's YouTube playlists: add a video to one (or a new one), and create a playlist from
//! videos, which is also how the queue is saved. Signed out, each says it needs sign-in.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_engine::EngineError;
use ym_engine::yt::YtPlaylistTarget;
use ym_i18n::{Str, t, tf};

use crate::feed::error_text;
use crate::graph::{AppGraph, on_core};
use crate::spinner::Spinner;

const PRIVACY: [(&str, Str); 3] = [
    ("PRIVATE", Str::YtPlaylistPrivacyPrivate),
    ("UNLISTED", Str::YtPlaylistPrivacyUnlisted),
    ("PUBLIC", Str::YtPlaylistPrivacyPublic),
];

enum Targets {
    Loading,
    SignedOut,
    Failed(EngineError),
    Ready(Vec<YtPlaylistTarget>),
}

/// The viewer's playlists plus New playlist; one already holding the video is checked and inert.
/// Picking one adds the video (a spinner on its row), then closes with a notice.
pub struct AddToPlaylist {
    video_id: String,
    targets: Targets,
    adding: Option<String>,
}

impl AddToPlaylist {
    pub fn open(video_id: String, window: &mut Window, cx: &mut App) {
        let view = cx.new(|cx| {
            let mut view = Self { video_id, targets: Targets::Loading, adding: None };
            view.load(cx);
            view
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title(t(Str::CdAddToPlaylist)).width(px(420.)).child(view.clone())
        });
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let graph = cx.global::<AppGraph>();
        let (engine, log, video_id) = (graph.engine(), graph.log.fork("yt-playlist"), self.video_id.clone());
        let request = on_core(cx, async move {
            if !engine.account_info(false, &log).await?.signed_in {
                return Ok(None);
            }
            engine.playlist_targets(&video_id, &log).await.map(Some)
        });
        cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |view, cx| {
                view.targets = match result {
                    Some(Ok(Some(targets))) => Targets::Ready(targets),
                    Some(Ok(None)) => Targets::SignedOut,
                    Some(Err(error)) => Targets::Failed(error),
                    None => return,
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn add(&mut self, target: YtPlaylistTarget, window: &mut Window, cx: &mut Context<Self>) {
        if self.adding.is_some() || target.contains_video {
            return;
        }
        self.adding = Some(target.playlist_id.clone());
        let graph = cx.global::<AppGraph>();
        let (engine, log, video_id, playlist_id) =
            (graph.engine(), graph.log.fork("yt-playlist"), self.video_id.clone(), target.playlist_id.clone());
        let request = on_core(cx, async move { engine.add_to_playlist(&playlist_id, &video_id, &log).await });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |view, window, cx| {
                view.adding = None;
                match result {
                    Some(Ok(())) => {
                        window.close_dialog(cx);
                        window.push_notification(tf(Str::YtPlaylistAdded, &[&target.title]), cx);
                    }
                    Some(Err(error)) => window.push_notification(error_text(&error), cx),
                    None => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn row(id: impl Into<ElementId>, icon: Icon, label: String, faint: bool, busy: bool, cx: &App) -> Stateful<Div> {
        let theme = cx.theme();
        h_flex()
            .id(id)
            .role(Role::Button)
            .aria_label(label.clone())
            .gap_4()
            .px_4()
            .py_2()
            .items_center()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|style| style.bg(theme.secondary))
            .text_color(if faint { theme.muted_foreground } else { theme.foreground })
            .child(icon.size(px(20.)))
            .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
            .when(busy, |row| row.child(Spinner::new().small()))
    }
}

impl Render for AddToPlaylist {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let message = |text: String| div().px_4().py_4().text_sm().text_color(theme.muted_foreground).child(text);
        match &self.targets {
            Targets::Loading => h_flex().justify_center().py_6().child(Spinner::new()).into_any_element(),
            Targets::SignedOut => message(t(Str::YtPlaylistSignIn).to_owned()).into_any_element(),
            Targets::Failed(error) => message(error_text(error)).into_any_element(),
            Targets::Ready(targets) => {
                let video_id = self.video_id.clone();
                let mut list = v_flex().max_h(px(420.)).id("playlist-targets").overflow_y_scroll().child(
                    Self::row("new-playlist", Icon::new(IconName::Plus).text_color(theme.primary), t(Str::AddToPlaylistNew).to_owned(), false, false, cx)
                        .on_click(cx.listener(move |view, _, window, cx| {
                            if view.adding.is_none() {
                                window.close_dialog(cx);
                                CreatePlaylist::open(vec![video_id.clone()], false, window, cx);
                            }
                        })),
                );
                for (index, target) in targets.clone().into_iter().enumerate() {
                    let icon = if target.contains_video { Icon::new(IconName::Check) } else { Icon::new(Lucide::ListPlus) };
                    let busy = self.adding.as_ref() == Some(&target.playlist_id);
                    let row = Self::row(("playlist-target", index), icon, target.title.clone(), target.contains_video, busy, cx)
                        .on_click(cx.listener(move |view, _, window, cx| view.add(target.clone(), window, cx)));
                    list = list.child(row);
                }
                list.into_any_element()
            }
        }
    }
}

/// Name and privacy for a new playlist holding `video_ids`; closes with a notice once created and
/// stays open on failure.
pub struct CreatePlaylist {
    video_ids: Vec<String>,
    from_queue: bool,
    name: Entity<InputState>,
    privacy: &'static str,
    signed_in: Option<bool>,
    busy: bool,
    _subscription: Subscription,
}

impl CreatePlaylist {
    pub fn open(video_ids: Vec<String>, from_queue: bool, window: &mut Window, cx: &mut App) {
        let view = cx.new(|cx| {
            let name = cx.new(|cx| InputState::new(window, cx).placeholder(t(Str::PlaylistNameHint)));
            name.update(cx, |input, cx| input.focus(window, cx));
            let subscription = cx.subscribe_in(&name, window, |view: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    view.submit(window, cx);
                }
                cx.notify();
            });
            let mut view =
                Self { video_ids, from_queue, name, privacy: PRIVACY[0].0, signed_in: None, busy: false, _subscription: subscription };
            view.check_account(cx);
            view
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title(t(Str::PlaylistCreateTitle)).width(px(440.)).child(view.clone())
        });
    }

    fn check_account(&mut self, cx: &mut Context<Self>) {
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine(), graph.log.fork("yt-playlist"));
        let request = on_core(cx, async move { engine.account_info(false, &log).await });
        cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |view, cx| {
                view.signed_in = Some(match result {
                    Some(Ok(account)) => account.signed_in,
                    Some(Err(error)) => {
                        cx.global::<AppGraph>().log.warn("account check failed", Some(&error));
                        false
                    }
                    None => return,
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.name.read(cx).value().trim().to_owned();
        if self.busy || title.is_empty() || self.signed_in != Some(true) {
            return;
        }
        self.busy = true;
        let graph = cx.global::<AppGraph>();
        let (engine, log, privacy, video_ids) =
            (graph.engine(), graph.log.fork("yt-playlist"), self.privacy, self.video_ids.clone());
        let name = title.clone();
        let request = on_core(cx, async move { engine.create_playlist(&name, privacy, &video_ids, &log).await });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |view, window, cx| {
                view.busy = false;
                match result {
                    Some(Ok(_)) => {
                        window.close_dialog(cx);
                        window.push_notification(tf(Str::YtPlaylistCreated, &[&title]), cx);
                    }
                    Some(Err(error)) => window.push_notification(error_text(&error), cx),
                    None => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for CreatePlaylist {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if self.signed_in == Some(false) {
            return v_flex().px_4().py_4().text_sm().text_color(theme.muted_foreground).child(t(Str::YtPlaylistSignIn));
        }
        let can_submit = !self.busy && self.signed_in == Some(true) && !self.name.read(cx).value().trim().is_empty();
        let mut chips = h_flex().gap_2();
        for (value, label) in PRIVACY {
            let selected = self.privacy == value;
            let chip = Button::new(value).small().rounded_full().label(t(label));
            chips = chips.child(if selected { chip.primary() } else { chip.outline() }.selected(selected).on_click(
                cx.listener(move |view, _, _, cx| {
                    if !view.busy {
                        view.privacy = value;
                        cx.notify();
                    }
                }),
            ));
        }
        v_flex()
            .gap_3()
            .px_4()
            .pb_2()
            .when(self.from_queue, |column| {
                column.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(tf(Str::YtPlaylistFromQueue, &[&self.video_ids.len()])),
                )
            })
            .child(Input::new(&self.name))
            .child(chips)
            .child(
                h_flex().justify_end().gap_2().child(if self.busy {
                    div().p_2().child(Spinner::new().small()).into_any_element()
                } else {
                    Button::new("create-playlist")
                        .primary()
                        .label(t(Str::PlaylistCreateConfirm))
                        .disabled(!can_submit)
                        .on_click(cx.listener(|view, _, window, cx| view.submit(window, cx)))
                        .into_any_element()
                }),
            )
    }
}
