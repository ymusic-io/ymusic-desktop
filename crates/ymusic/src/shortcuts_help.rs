//! The keyboard shortcuts help (F1): every Shell binding with what it does.

use gpui_kit::component::button::Button;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::*;
use ym_i18n::{Str, t};

use crate::a11y::FocusList;
use crate::shortcuts::HELP;

pub struct ShortcutsHelp {
    focus: FocusList,
    scroll: ScrollHandle,
    focused_first: bool,
}

impl ShortcutsHelp {
    /// Opens the help unless a dialog is already up; F1 again closes it.
    pub fn open(window: &mut Window, cx: &mut App) {
        if window.has_active_dialog(cx) {
            return;
        }
        let view = cx.new(|cx| {
            let scroll = ScrollHandle::new();
            Self { focus: FocusList::modal(Some(scroll.clone()), cx), scroll, focused_first: false }
        });
        let height = (f32::from(window.viewport_size().height) - 200.).clamp(240., 620.);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title(t(Str::ShortcutsTitle)).width(px(560.)).child(div().h(px(height)).child(view.clone()))
        });
    }
}

/// The platform's label for `stroke`; Esc spelled out, as its ⎋ glyph is little known.
fn key_label(stroke: &Keystroke) -> String {
    Kbd::format(stroke).replace('⎋', "Esc")
}

/// Arrow glyphs are spoken inconsistently (or not at all) across screen readers.
fn spoken(keys: &str) -> String {
    [
        ("←", Str::A11yKeyArrowLeft),
        ("→", Str::A11yKeyArrowRight),
        ("↑", Str::A11yKeyArrowUp),
        ("↓", Str::A11yKeyArrowDown),
    ]
    .iter()
    .fold(keys.to_owned(), |text, (glyph, name)| text.replace(glyph, &format!(" {} ", t(*name))))
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

impl Render for ShortcutsHelp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused_first {
            self.focused_first = true;
            let first = self.focus.handle(0, cx);
            window.defer(cx, move |window, cx| window.focus(&first, cx));
        }
        let theme = cx.theme().clone();
        let mut rows = v_flex().py_1();
        for (index, row) in HELP.iter().enumerate() {
            let mut keys = h_flex().w(px(200.)).flex_shrink_0().flex_wrap().gap_1().items_center();
            let mut spoken_keys = Vec::new();
            for (position, combo) in row.keys.iter().enumerate() {
                if position > 0 {
                    keys = keys.child(div().text_xs().text_color(theme.muted_foreground).child(if row.range {
                        "…"
                    } else {
                        ","
                    }));
                }
                let strokes: Vec<Keystroke> =
                    combo.split(' ').filter_map(|stroke| Keystroke::parse(stroke).ok()).collect();
                spoken_keys.push(strokes.iter().map(|stroke| spoken(&key_label(stroke))).collect::<Vec<_>>().join(" "));
                for stroke in &strokes {
                    keys = keys.child(
                        div()
                            .px_1p5()
                            .py_0p5()
                            .min_w(px(22.))
                            .text_center()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(theme.border)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(key_label(stroke)),
                    );
                }
            }
            let label = t(row.label);
            let line = h_flex()
                .id(("shortcut", index))
                .role(Role::ListItem)
                .aria_label(format!("{label}, {}", spoken_keys.join(if row.range { " … " } else { ", " })))
                .gap_4()
                .px_4()
                .py_2()
                .items_center()
                .rounded(px(6.))
                .child(keys)
                .child(div().flex_1().min_w_0().text_sm().child(label));
            rows = rows.child(self.focus.item(index, line, cx));
        }
        let close = HELP.len();
        let footer = h_flex().justify_end().pt_2().child(
            self.focus.button(
                close,
                Button::new("shortcuts-close")
                    .small()
                    .label(t(Str::CdClose))
                    .on_click(|_, window, cx| window.close_dialog(cx)),
                cx,
            ),
        );
        let list = div().id("shortcuts").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).child(rows);
        self.focus
            .container(
                v_flex().size_full().on_key_down(|event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "f1" {
                        window.close_dialog(cx);
                        cx.stop_propagation();
                    }
                }),
                0..close + 1,
                cx,
            )
            .child(list)
            .child(footer)
    }
}
