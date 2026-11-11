//! Search input: a pushed layer with back and the search field on top, live suggestions below.

use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::*;
use ym_i18n::{Str, t};

use crate::a11y::FocusList;
use crate::graph::{AppGraph, on_core};

const SUGGEST_DEBOUNCE: Duration = Duration::from_millis(250);

pub enum SearchEvent {
    Submit(String),
    Back,
}

pub struct SearchView {
    input: Entity<InputState>,
    suggestions: Vec<String>,
    generation: u64,
    focus: FocusList,
    _subscription: Subscription,
}

impl EventEmitter<SearchEvent> for SearchView {}

impl SearchView {
    pub fn new(query: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder(t(Str::SearchYoutube));
            state.set_value(query.to_owned(), window, cx);
            state
        });
        input.update(cx, |state, cx| state.focus(window, cx));
        let subscription = cx.subscribe_in(&input, window, |view, input, event: &InputEvent, _, cx| match event {
            InputEvent::Change => view.query_changed(input.read(cx).value().to_string(), cx),
            InputEvent::PressEnter { .. } => view.submit(input.read(cx).value().to_string(), cx),
            _ => {}
        });
        let mut view = Self {
            input,
            suggestions: Vec::new(),
            generation: 0,
            focus: FocusList::new(None),
            _subscription: subscription,
        };
        view.query_changed(query.to_owned(), cx);
        view
    }

    fn submit(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if !query.is_empty() {
            cx.emit(SearchEvent::Submit(query));
        }
    }

    fn query_changed(&mut self, query: String, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        if query.trim().is_empty() {
            self.suggestions.clear();
            cx.notify();
            return;
        }
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine(), graph.log.fork("search"));
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(SUGGEST_DEBOUNCE).await;
            if this.read_with(cx, |view, _| view.generation != generation).unwrap_or(true) {
                return;
            }
            let fetch = cx.update(|cx| on_core(cx, async move { engine.search_suggestions(&query, &log).await }));
            let result = fetch.await;
            this.update(cx, |view, cx| {
                if view.generation == generation
                    && let Some(Ok(suggestions)) = result
                {
                    view.suggestions = suggestions;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Render for SearchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let bar = h_flex()
            .gap_2()
            .px_2()
            .pt(px(6.))
            .pb(px(7.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("search-back")
                    .ghost()
                    .icon(Icon::new(IconName::ArrowLeft))
                    .accessibility_label(t(Str::CdBack))
                    .tooltip(t(Str::CdBack))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SearchEvent::Back))),
            )
            .child(div().flex_1().child(Input::new(&self.input).cleanable(true)));
        let mut rows = v_flex();
        for (index, suggestion) in self.suggestions.iter().take(12).enumerate() {
            let (submitted, inserted) = (suggestion.clone(), suggestion.clone());
            rows = rows.child(
                self.focus
                    .item(
                        index,
                        h_flex().id(("suggestion", index)).role(Role::ListItem).aria_label(suggestion.clone()),
                        cx,
                    )
                    .gap_4()
                    .pl_4()
                    .pr_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary))
                    .child(Icon::new(IconName::Search).text_color(theme.muted_foreground))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(suggestion.clone()))
                    .child(
                        Button::new(("insert", index))
                            .ghost()
                            .small()
                            .icon(Icon::new(Lucide::ArrowUpLeft))
                            .accessibility_label(t(Str::CdInsertQuery))
                            .tooltip(t(Str::CdInsertQuery))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.input.update(cx, |input, cx| input.set_value(inserted.clone(), window, cx));
                            })),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| view.submit(submitted.clone(), cx))),
            );
        }
        let shown = self.suggestions.len().min(12);
        let rows = self.focus.container(rows, 0..shown, cx);
        v_flex().size_full().child(bar).child(div().id("suggestions").flex_1().overflow_y_scroll().child(rows))
    }
}
