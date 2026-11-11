//! The search filter dialog: the server's filter groups as rows of pills. Picking an option hands
//! back its params, which carry the whole resulting filter state (the server composes them).

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::*;
use ym_engine::yt::YtFilterGroup;
use ym_i18n::{Str, t};

use crate::a11y::FocusList;

type OnSelect = Rc<dyn Fn(String, &mut Window, &mut App)>;

pub struct SearchFilter {
    groups: Vec<YtFilterGroup>,
    on_select: OnSelect,
    focus: FocusList,
    scroll: ScrollHandle,
    focused_first: bool,
}

impl SearchFilter {
    pub fn open(
        groups: Vec<YtFilterGroup>,
        on_select: impl Fn(String, &mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut App,
    ) {
        let view = cx.new(|cx| {
            let scroll = ScrollHandle::new();
            Self {
                groups,
                on_select: Rc::new(on_select),
                focus: FocusList::modal(Some(scroll.clone()), cx),
                scroll,
                focused_first: false,
            }
        });
        let height = (f32::from(window.viewport_size().height) - 200.).clamp(200., 560.);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(t(Str::SearchFilterTitle))
                .width(px(460.))
                .child(div().max_h(px(height)).flex().flex_col().child(view.clone()))
        });
    }
}

impl Render for SearchFilter {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut stop = 0;
        let mut first_enabled = None;
        let mut column = v_flex().px_2().pb_2();
        for (group_index, group) in self.groups.iter().enumerate() {
            column = column.child(
                div()
                    .id(("filter-group", group_index))
                    .role(Role::Heading)
                    .pt_4()
                    .pb_2()
                    .text_xs()
                    .font_bold()
                    .text_color(theme.muted_foreground)
                    .child(group.title.to_uppercase()),
            );
            let mut pills = h_flex().flex_wrap().gap_2();
            for option in &group.options {
                let params = option.params.clone();
                let on_select = self.on_select.clone();
                let mut pill = Button::new(("filter-option", stop))
                    .small()
                    .rounded_full()
                    .label(option.label.clone())
                    .selected(option.selected)
                    .disabled(option.disabled);
                pill = if option.selected { pill.primary() } else { pill.outline() };
                let pill = pill.on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    if let Some(params) = params.clone() {
                        on_select(params, window, cx);
                    }
                });
                if first_enabled.is_none() && !option.disabled {
                    first_enabled = Some(stop);
                }
                pills = pills.child(self.focus.button(stop, pill, cx));
                stop += 1;
            }
            column = column.child(pills);
        }
        let cancel = stop;
        if !self.focused_first {
            self.focused_first = true;
            let first = self.focus.handle(first_enabled.unwrap_or(cancel), cx);
            window.defer(cx, move |window, cx| window.focus(&first, cx));
        }
        self.focus
            .container(v_flex().min_h_0(), 0..cancel + 1, cx)
            .child(
                div()
                    .id("filter-groups")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(column),
            )
            .child(
                h_flex().justify_end().pt_2().child(
                    self.focus.button(
                        cancel,
                        Button::new("filter-cancel")
                            .small()
                            .label(t(Str::DialogCancel))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                        cx,
                    ),
                ),
            )
    }
}
