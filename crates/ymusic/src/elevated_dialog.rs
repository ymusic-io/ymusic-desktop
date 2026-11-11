//! The JVM app's `ElevatedBarDialog` chrome on a gpui-kit dialog: the title and the button bar on a
//! raised ground behind dividers, a full-bleed body between them that scrolls under the bars (a
//! shadow edge shows there is more), square-ish corners and no close button.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::{ActiveTheme, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t};

use crate::a11y::FocusList;

/// The picker width; anything wider keeps a 24px gutter to the window edge.
pub const DEFAULT_WIDTH: f32 = 560.;
const WIDE_GUTTER: f32 = 24.;
const SHADOW: f32 = 6.;

/// `dialog` as the chrome for a body at most `max_width` wide; the view draws the bars itself.
pub fn chrome(dialog: Dialog, max_width: f32, window: &Window) -> Dialog {
    let viewport = f32::from(window.viewport_size().width);
    let width = if max_width > DEFAULT_WIDTH { max_width.min(viewport - 2. * WIDE_GUTTER) } else { max_width };
    dialog
        .p_0()
        .gap_0()
        .border_0()
        .rounded(px(4.))
        .overflow_hidden()
        .close_button(false)
        .centered()
        .w(px(width.max(280.)))
}

/// The body's height cap: 85% of the window less the bars, never under 240.
pub fn body_max_height(window: &Window) -> Pixels {
    px((f32::from(window.viewport_size().height) * 0.85 - 112.).max(240.))
}

pub fn bar_color(cx: &App) -> Hsla {
    cx.theme().muted
}

pub fn title_bar(title: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .flex_shrink_0()
        .bg(bar_color(cx))
        .child(
            div()
                .id("dialog-title")
                .role(Role::Heading)
                .px(px(24.))
                .py(px(16.))
                .text_size(px(17.))
                .line_height(px(22.))
                .font_bold()
                .text_color(theme.foreground)
                .child(title.into()),
        )
        .child(div().h(px(1.)).w_full().bg(theme.border))
}

/// The bottom bar: `content` right-aligned after a divider.
pub fn button_bar(content: impl IntoElement, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .w_full()
        .flex_shrink_0()
        .bg(bar_color(cx))
        .child(div().h(px(1.)).w_full().bg(theme.border))
        .child(h_flex().w_full().px(px(12.)).py(px(2.)).justify_end().items_center().child(content))
}

/// A bar action as a text button: the primary one in the accent, the rest in secondary ink.
pub fn text_button(id: impl Into<ElementId>, label: &'static str, primary: bool, enabled: bool, cx: &App) -> Button {
    let theme = cx.theme();
    let color = match (primary, enabled) {
        (_, false) => theme.muted_foreground.opacity(0.6),
        (true, true) => theme.primary,
        (false, true) => theme.muted_foreground,
    };
    Button::new(id).ghost().h(px(40.)).px(px(12.)).text_color(color).font_medium().label(label)
}

/// The edges a scrolled body casts under the bars: drawn over its top and bottom.
pub fn scroll_shadows(scroll: &ScrollHandle) -> impl IntoElement {
    let (offset, max) = (scroll.offset().y, scroll.max_offset().y);
    let edge = |top: bool| {
        let shade = black().opacity(0.14);
        div()
            .absolute()
            .left_0()
            .right_0()
            .h(px(SHADOW))
            .map(|edge| if top { edge.top_0() } else { edge.bottom_0() })
            .bg(linear_gradient(
                if top { 180. } else { 0. },
                linear_color_stop(shade, 0.),
                linear_color_stop(shade.opacity(0.), 1.),
            ))
    };
    div()
        .when(offset < px(0.), |layer| layer.child(edge(true)))
        .when(offset > -max && max > px(0.), |layer| layer.child(edge(false)))
}

/// The `ConfirmDialog` of the JVM app: `message` under `title`, Cancel and the accent `confirm`.
pub fn confirm(
    title: &'static str,
    message: String,
    confirm: &'static str,
    window: &mut Window,
    cx: &mut App,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
) {
    let on_confirm = Rc::new(on_confirm);
    let focus = Rc::new(FocusList::modal(None, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let (enter, click) = (on_confirm.clone(), on_confirm.clone());
        let theme = cx.theme();
        let buttons = h_flex()
            .child(
                focus.button(
                    0,
                    text_button("confirm-cancel", t(Str::DialogCancel), false, true, cx)
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                    cx,
                ),
            )
            .child(focus.button(
                1,
                text_button("confirm-ok", confirm, true, true, cx).on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    click(window, cx);
                }),
                cx,
            ));
        chrome(dialog, DEFAULT_WIDTH, window)
            .on_ok(move |_, window, cx| {
                enter(window, cx);
                true
            })
            .child(
                v_flex()
                    .w_full()
                    .child(title_bar(title, cx))
                    .child(
                        div()
                            .w_full()
                            .px(px(24.))
                            .py(px(16.))
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(theme.foreground)
                            .child(message.clone()),
                    )
                    .child(button_bar(buttons, cx)),
            )
    });
}
