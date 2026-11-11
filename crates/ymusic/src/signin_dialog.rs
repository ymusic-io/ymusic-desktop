//! Signing in without a browser, laid out like the JVM's CookieSignInScreen: a full-page layer with
//! its own bar. First the QR to scan with the phone app (its cookies arrive over the device link),
//! with the matching code; "Or paste cookies from a browser" switches to pasting a copied request.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, StyledExt, WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t, tf};
use ym_signin::DeviceLinkOffer;

use crate::a11y::KeyboardFocusable;
use crate::graph::{AppGraph, on_core};
use crate::signin::SignInError;
use crate::spinner::Spinner;

const FORM_MAX: f32 = 560.;
const QR_BOX: f32 = 220.;
const QR_PADDING: f32 = 8.;

pub enum SignInEvent {
    /// Leave the screen without signing in.
    Back,
    Done,
}

enum Link {
    Starting,
    Waiting {
        modules: Vec<bool>,
        width: usize,
        code: String,
    },
    /// The offer could not be made or expired; Retry starts a new one.
    Ended,
}

#[derive(Clone, Copy, PartialEq)]
enum Browser {
    Chrome,
    Firefox,
    Safari,
}

impl Browser {
    const ALL: [Browser; 3] = [Browser::Chrome, Browser::Firefox, Browser::Safari];

    fn label(self) -> &'static str {
        t(match self {
            Browser::Chrome => Str::SigninPasteBrowserChrome,
            Browser::Firefox => Str::SigninPasteBrowserFirefox,
            Browser::Safari => Str::SigninPasteBrowserSafari,
        })
    }

    fn hint(self) -> &'static str {
        t(match self {
            Browser::Chrome => Str::SigninPasteHint,
            Browser::Firefox => Str::SigninPasteHintFirefox,
            Browser::Safari => Str::SigninPasteHintSafari,
        })
    }
}

pub struct SignInView {
    link: Link,
    /// Shown under the code: why the QR ended, or why a paste was refused.
    error: Option<String>,
    paste_mode: bool,
    browser: Browser,
    paste: Entity<TextareaState>,
    submitting: bool,
    _link: Option<Task<()>>,
}

impl EventEmitter<SignInEvent> for SignInView {}

impl SignInView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paste = cx.new(|cx| TextareaState::new(window, cx).placeholder(t(Str::SigninPasteEmpty)));
        let mut view = Self {
            link: Link::Starting,
            error: None,
            paste_mode: false,
            browser: if cfg!(target_os = "macos") { Browser::Safari } else { Browser::Chrome },
            paste,
            submitting: false,
            _link: None,
        };
        view.start_link(cx);
        view
    }

    /// Back from pasting returns to the QR; back from the QR leaves.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        if self.paste_mode {
            self.paste_mode = false;
            self.error = None;
            cx.notify();
        } else {
            cx.emit(SignInEvent::Back);
        }
    }

    fn start_link(&mut self, cx: &mut Context<Self>) {
        self.link = Link::Starting;
        self.error = None;
        let remote = cx.global::<AppGraph>().remote.clone();
        let start = on_core(cx, async move { crate::signin::start_device_link(&remote).await });
        self._link = Some(cx.spawn(async move |this, cx| {
            let result = start.await;
            this.update(cx, |view, cx| {
                match result {
                    Some(Ok(offer)) => view.wait_for_phone(offer, cx),
                    Some(Err(error)) => {
                        cx.global::<AppGraph>().log.info(&format!("device link unavailable: {error}"));
                        view.error = Some(sign_in_error_text(&error, cx));
                        view.link = Link::Ended;
                    }
                    None => {}
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn wait_for_phone(&mut self, offer: DeviceLinkOffer, cx: &mut Context<Self>) {
        match qrcode::QrCode::new(offer.qr_payload().as_bytes()) {
            Ok(qr) => {
                let width = qr.width();
                let modules = qr.to_colors().into_iter().map(|color| color == qrcode::Color::Dark).collect();
                self.link = Link::Waiting { modules, width, code: offer.confirmation_code() };
            }
            Err(error) => {
                cx.global::<AppGraph>()
                    .log
                    .warn(&format!("R90023 the device-link QR could not be drawn: {error}"), None);
                self.error = Some(tf(Str::ErrorGeneric, &[&"R90023"]));
                self.link = Link::Ended;
                return;
            }
        }
        let graph = cx.global::<AppGraph>();
        let (remote, jar, engines, log) =
            (graph.remote.clone(), graph.cookies.clone(), graph.engines.clone(), graph.log.fork("devicelink"));
        let linked =
            on_core(cx, async move { crate::signin::await_device_link(remote, offer, jar, engines, &log).await });
        self._link = Some(cx.spawn(async move |this, cx| {
            let result = linked.await;
            this.update(cx, |view, cx| match result {
                Some(Ok(_)) => cx.emit(SignInEvent::Done),
                Some(Err(error)) => {
                    view.error = Some(sign_in_error_text(&error, cx));
                    view.link = Link::Ended;
                    cx.notify();
                }
                None => {}
            })
            .ok();
        }));
    }

    fn submit_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.paste.read(cx).value().to_string();
        let cookies = ym_signin::parse_cookie_paste(&text);
        if cookies.is_empty() {
            if text.trim_start().starts_with("curl") {
                window.open_dialog(cx, |dialog, _, _| {
                    dialog
                        .title(t(Str::SigninPasteNoCookieTitle))
                        .width(px(FORM_MAX))
                        .child(div().text_sm().child(t(Str::SigninPasteNoCookieBody)))
                });
            } else {
                self.error = Some(t(Str::SigninPasteEmpty).to_owned());
                cx.notify();
            }
            return;
        }
        self.submitting = true;
        self.error = None;
        let graph = cx.global::<AppGraph>();
        let (jar, engines, log) = (graph.cookies.clone(), graph.engines.clone(), graph.log.fork("signin"));
        let done = on_core(cx, async move { crate::signin::sign_in_with_cookies(cookies, jar, engines, &log).await });
        cx.spawn(async move |this, cx| {
            let result = done.await;
            this.update(cx, |view, cx| {
                view.submitting = false;
                match result {
                    Some(Ok(_)) => cx.emit(SignInEvent::Done),
                    Some(Err(error)) => view.error = Some(sign_in_error_text(&error, cx)),
                    None => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_qr(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let qr: AnyElement = match &self.link {
            Link::Waiting { modules, width, .. } => {
                let cell = ((QR_BOX - 2. * QR_PADDING) / *width as f32).floor();
                let mut grid = v_flex()
                    .id("device-link-qr")
                    .role(Role::Image)
                    .aria_label(t(Str::CdDeviceLinkQr))
                    .p(px(QR_PADDING))
                    .bg(white());
                for y in 0..*width {
                    let mut line = h_flex();
                    for x in 0..*width {
                        let dark = modules[y * width + x];
                        line = line.child(div().size(px(cell)).when(dark, |module| module.bg(black())));
                    }
                    grid = grid.child(line);
                }
                grid.into_any_element()
            }
            Link::Starting => Spinner::new().into_any_element(),
            Link::Ended => div().into_any_element(),
        };
        let code = match &self.link {
            Link::Waiting { code, .. } => Some(code.clone()),
            _ => None,
        };
        let status: AnyElement = match (&self.error, &self.link) {
            (Some(error), _) => {
                div().text_sm().text_color(theme.danger).text_center().child(error.clone()).into_any_element()
            }
            (None, Link::Ended) => div().into_any_element(),
            (None, _) => h_flex()
                .gap_2()
                .items_center()
                .child(Spinner::new().small())
                .child(div().text_sm().text_color(theme.muted_foreground).child(t(Str::SigninQrWaiting)))
                .into_any_element(),
        };
        let ended = matches!(self.link, Link::Ended);
        vec![
            div().w_full().text_center().font_semibold().child(t(Str::SigninQrTitle)).into_any_element(),
            div()
                .w_full()
                .text_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(t(Str::SigninQrHint))
                .into_any_element(),
            div().w_full().h(px(QR_BOX)).flex().items_center().justify_center().child(qr).into_any_element(),
            v_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(28.))
                        .font_bold()
                        .font_family("Menlo")
                        .text_color(if code.is_some() { theme.foreground } else { transparent_black() })
                        .child(code.unwrap_or_else(|| "000 000".to_owned())),
                )
                .child(div().text_sm().text_center().text_color(theme.muted_foreground).child(t(Str::SigninQrSasHint)))
                .into_any_element(),
            v_flex()
                .w_full()
                .min_h(px(20.))
                .items_center()
                .gap_1()
                .child(status)
                .when(ended, |column| {
                    column.child(
                        Button::new("signin-retry")
                            .ghost()
                            .icon(Lucide::RefreshCw)
                            .label(t(Str::Retry))
                            .on_click(cx.listener(|view, _, _, cx| view.start_link(cx))),
                    )
                })
                .into_any_element(),
            div()
                .id("signin-paste-link")
                .role(Role::Link)
                .aria_label(t(Str::SigninOrPaste))
                .keyboard_focusable(theme.ring)
                .py_2()
                .px_1()
                .rounded(px(4.))
                .text_sm()
                .text_color(theme.link)
                .cursor_pointer()
                .child(t(Str::SigninOrPaste))
                .on_click(cx.listener(|view, _, _, cx| {
                    view.paste_mode = true;
                    view.error = None;
                    cx.notify();
                }))
                .into_any_element(),
        ]
    }

    fn render_paste(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let mut browsers = h_flex().gap_2().justify_center();
        for (index, browser) in Browser::ALL.into_iter().enumerate() {
            let selected = browser == self.browser;
            let chip =
                Button::new(("signin-browser", index)).small().rounded_full().label(browser.label()).selected(selected);
            let chip = if selected { chip.primary() } else { chip.outline() };
            browsers = browsers.child(chip.on_click(cx.listener(move |view, _, _, cx| {
                view.browser = browser;
                cx.notify();
            })));
        }
        let blank = self.paste.read(cx).value().trim().is_empty();
        vec![
            div().w_full().text_center().font_semibold().child(t(Str::SigninOrPaste)).into_any_element(),
            browsers.into_any_element(),
            div().w_full().text_sm().text_color(theme.muted_foreground).child(self.browser.hint()).into_any_element(),
            v_flex()
                .w_full()
                .gap_1()
                .child(div().text_xs().text_color(theme.muted_foreground).child(t(Str::SigninPasteLabel)))
                .child(Textarea::new(&self.paste).h(px(120.)))
                .when_some(self.error.clone(), |column, error| {
                    column.child(div().text_sm().text_color(theme.danger).child(error))
                })
                .into_any_element(),
            Button::new("signin-use-cookies")
                .primary()
                .w_full()
                .label(t(Str::SigninPasteSubmit))
                .loading(self.submitting)
                .disabled(blank || self.submitting)
                .on_click(cx.listener(|view, _, window, cx| view.submit_paste(window, cx)))
                .into_any_element(),
        ]
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

impl Render for SignInView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let bar = h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .child(
                Button::new("signin-back")
                    .ghost()
                    .icon(Icon::new(IconName::ArrowLeft))
                    .accessibility_label(t(Str::CdBack))
                    .tooltip(t(Str::CdBack))
                    .on_click(cx.listener(|view, _, _, cx| view.back(cx))),
            )
            .child(div().id("signin-heading").role(Role::Heading).font_semibold().child(t(Str::SigninScreenTitle)));
        let content = if self.paste_mode { self.render_paste(cx) } else { self.render_qr(cx) };
        v_flex().size_full().bg(theme.background).child(bar).child(
            div().id("signin-scroll").flex_1().min_h_0().overflow_y_scroll().child(
                h_flex().w_full().min_h_full().justify_center().items_center().child(
                    v_flex().w_full().max_w(px(FORM_MAX)).px_4().py_4().gap_4().items_center().children(content),
                ),
            ),
        )
    }
}
