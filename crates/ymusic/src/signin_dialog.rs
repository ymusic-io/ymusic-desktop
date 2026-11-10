//! Signing in without a browser: scan a QR with the phone app (its cookies arrive over the device
//! link), or paste cookies copied from a desktop browser.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t, tf};
use ym_signin::DeviceLinkOffer;

use crate::graph::{AppGraph, on_core};
use crate::signin::SignInError;

enum Link {
    Starting,
    Waiting { modules: Vec<bool>, width: usize, code: String },
    Unavailable,
}

pub struct SignInDialog {
    link: Link,
    paste: Entity<TextareaState>,
    submitting: bool,
    error: Option<String>,
    window: AnyWindowHandle,
    on_done: Rc<dyn Fn(&mut App)>,
}

impl SignInDialog {
    pub fn open(window: &mut Window, cx: &mut App, on_done: impl Fn(&mut App) + 'static) {
        let on_done: Rc<dyn Fn(&mut App)> = Rc::new(on_done);
        let dialog = cx.new(|cx| SignInDialog::new(window, on_done, cx));
        window.open_dialog(cx, move |modal, _, _| {
            modal.title(t(Str::SigninScreenTitle)).width(px(560.)).child(dialog.clone())
        });
    }

    fn new(window: &mut Window, on_done: Rc<dyn Fn(&mut App)>, cx: &mut Context<Self>) -> Self {
        let paste = cx.new(|cx| TextareaState::new(window, cx).placeholder(t(Str::SigninPasteEmpty)));
        let mut dialog = Self {
            link: Link::Starting,
            paste,
            submitting: false,
            error: None,
            window: window.window_handle(),
            on_done,
        };
        dialog.start_link(cx);
        dialog
    }

    fn start_link(&mut self, cx: &mut Context<Self>) {
        let remote = cx.global::<AppGraph>().remote.clone();
        let start = on_core(cx, async move { crate::signin::start_device_link(&remote).await });
        cx.spawn(async move |this, cx| {
            let result = start.await;
            this.update(cx, |dialog, cx| {
                match result {
                    Some(Ok(offer)) => dialog.wait_for_phone(offer, cx),
                    Some(Err(error)) => {
                        cx.global::<AppGraph>().log.info(&format!("device link unavailable: {error}"));
                        dialog.link = Link::Unavailable;
                    }
                    None => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn wait_for_phone(&mut self, offer: DeviceLinkOffer, cx: &mut Context<Self>) {
        match qrcode::QrCode::new(offer.qr_payload().as_bytes()) {
            Ok(qr) => {
                let width = qr.width();
                let modules = qr.to_colors().into_iter().map(|color| color == qrcode::Color::Dark).collect();
                self.link = Link::Waiting { modules, width, code: offer.confirmation_code() };
            }
            Err(error) => {
                cx.global::<AppGraph>().log.warn(&format!("the device-link QR could not be drawn: {error}"), None);
                self.link = Link::Unavailable;
                return;
            }
        }
        let graph = cx.global::<AppGraph>();
        let (remote, jar, engines, log) =
            (graph.remote.clone(), graph.cookies.clone(), graph.engines.clone(), graph.log.fork("devicelink"));
        let linked =
            on_core(cx, async move { crate::signin::await_device_link(remote, offer, jar, engines, &log).await });
        cx.spawn(async move |this, cx| {
            let result = linked.await;
            this.update(cx, |dialog, cx| match result {
                Some(Ok(_)) => dialog.finish(cx),
                Some(Err(error)) => {
                    dialog.error = Some(sign_in_error_text(&error, cx));
                    dialog.link = Link::Unavailable;
                    cx.notify();
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }

    fn submit_paste(&mut self, cx: &mut Context<Self>) {
        let text = self.paste.read(cx).value().to_string();
        let cookies = ym_signin::parse_cookie_paste(&text);
        if cookies.is_empty() {
            self.error = Some(t(Str::SigninPasteNoCookieTitle).to_owned());
            cx.notify();
            return;
        }
        self.submitting = true;
        self.error = None;
        let graph = cx.global::<AppGraph>();
        let (jar, engines, log) = (graph.cookies.clone(), graph.engines.clone(), graph.log.fork("signin"));
        let done = on_core(cx, async move { crate::signin::sign_in_with_cookies(cookies, jar, engines, &log).await });
        cx.spawn(async move |this, cx| {
            let result = done.await;
            this.update(cx, |dialog, cx| {
                dialog.submitting = false;
                match result {
                    Some(Ok(_)) => dialog.finish(cx),
                    Some(Err(error)) => dialog.error = Some(sign_in_error_text(&error, cx)),
                    None => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        let on_done = self.on_done.clone();
        cx.update_window(self.window, |_, window, cx| window.close_dialog(cx)).ok();
        on_done(cx);
    }
}

fn sign_in_error_text(error: &SignInError, cx: &App) -> String {
    match error {
        SignInError::NotSignedIn => t(Str::SigninCookiesRejected).to_owned(),
        SignInError::Engine(engine) => crate::feed::error_text(engine),
        other => {
            cx.global::<AppGraph>().log.warn("sign-in failed", Some(other));
            tf(Str::ErrorGeneric, &[&"R90019"])
        }
    }
}

impl Render for SignInDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let link: AnyElement = match &self.link {
            Link::Starting => h_flex().gap_2().child(Spinner::new()).child(t(Str::SigninLoading)).into_any_element(),
            Link::Unavailable => div().into_any_element(),
            Link::Waiting { modules, width, code } => {
                let cell = 4.;
                let mut grid = v_flex().p_2().bg(white()).rounded_md();
                for y in 0..*width {
                    let mut line = h_flex();
                    for x in 0..*width {
                        let dark = modules[y * width + x];
                        line = line.child(div().size(px(cell)).when(dark, |module| module.bg(black())));
                    }
                    grid = grid.child(line);
                }
                h_flex()
                    .gap_4()
                    .child(grid)
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_2()
                            .child(div().font_semibold().child(t(Str::SigninQrTitle)))
                            .child(div().text_sm().text_color(theme.muted_foreground).child(t(Str::SigninQrHint)))
                            .child(div().text_2xl().font_semibold().child(code.clone()))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(t(Str::SigninQrSasHint)))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(Spinner::new())
                                    .child(div().text_sm().child(t(Str::SigninQrWaiting))),
                            ),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .gap_4()
            .child(link)
            .child(div().text_sm().font_semibold().child(t(Str::SigninOrPaste)))
            .child(div().text_xs().text_color(theme.muted_foreground).child(t(Str::SigninPasteHint)))
            .child(Textarea::new(&self.paste).h(px(120.)))
            .when_some(self.error.clone(), |column, error| {
                column.child(div().text_sm().text_color(theme.danger).child(error))
            })
            .child(
                h_flex().justify_end().child(
                    Button::new("use-cookies")
                        .primary()
                        .label(t(Str::SigninPasteSubmit))
                        .loading(self.submitting)
                        .on_click(cx.listener(|dialog, _, _, cx| dialog.submit_paste(cx))),
                ),
            )
    }
}
