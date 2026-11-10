//! A feed surface: one engine source (a Discover tab, search results, a playlist / channel / music
//! page), paged by continuation, rendered as titled sections of display items.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_common::log::Logger;
use ym_engine::yt::{YtFeedItem, YtFeedPage, YtPageHeader, YtSectionHeader, YtThumb};
use ym_engine::{DynamicEngine, EngineError, ErrorKind};
use ym_i18n::{Str, t, tf};

use crate::graph::{AppGraph, on_core};

#[derive(Debug, Clone, PartialEq)]
pub enum FeedSource {
    Home,
    Trending,
    Recommended,
    MusicHome,
    Playlist(String),
    Channel(String),
    MusicBrowse(String),
    Search(String),
}

impl FeedSource {
    /// Routes an engine-composed app link by its path prefix only; the whole link goes back to the
    /// engine verbatim. `None` = a scheme this build can't open (a newer bundle's), left inert.
    pub fn from_link(link: &str) -> Option<Self> {
        if link.starts_with("/ytmusic/browse") {
            Some(Self::MusicBrowse(link.to_owned()))
        } else if link.starts_with("/playlist?") {
            Some(Self::Playlist(link.to_owned()))
        } else if link.len() > "/channel/".len() && link.starts_with("/channel/")
            || link.len() > "/feed/".len() && link.starts_with("/feed/")
        {
            Some(Self::Channel(link.to_owned()))
        } else {
            None
        }
    }

    async fn load(
        self,
        engine: Arc<DynamicEngine>,
        continuation: Option<String>,
        log: Logger,
    ) -> Result<YtFeedPage, EngineError> {
        let continuation = continuation.as_deref();
        match &self {
            Self::Home => engine.home_feed(continuation, false, &log).await,
            Self::Trending => engine.trending_music_feed(&log).await,
            Self::Recommended => engine.recommended_feed(&log).await,
            Self::MusicHome => engine.music_home(continuation, false, &log).await,
            Self::Playlist(url) => engine.playlist_detail(url, continuation, &log).await,
            Self::Channel(url) => engine.channel_detail(url, continuation, &log).await,
            Self::MusicBrowse(url) => engine.music_browse(url, continuation, &log).await,
            Self::Search(query) => {
                let query = continuation.is_none().then_some(query.as_str());
                engine.search(query, None, continuation, &log).await
            }
        }
    }
}

/// A request to play a video; `queue` is set when its section plays in order.
pub struct Play {
    pub item: YtFeedItem,
    pub queue: Option<Vec<YtFeedItem>>,
}

/// Something to do with a video besides opening it.
pub struct ItemAction {
    pub item: YtFeedItem,
    pub action: FeedAction,
}

#[derive(Clone, Copy)]
pub enum FeedAction {
    QueueNext,
    QueueLast,
    Radio,
    Download,
}

/// A request to open a new surface on the current section's stack.
pub struct Navigate {
    pub source: FeedSource,
    pub title: Option<SharedString>,
}

enum Phase {
    Loading,
    Ready,
    Failed(EngineError),
}

pub struct FeedView {
    source: FeedSource,
    title: Option<SharedString>,
    page: YtFeedPage,
    phase: Phase,
    loading_more: bool,
    more_failed: Option<EngineError>,
    generation: u64,
    scroll: ScrollHandle,
}

impl EventEmitter<Navigate> for FeedView {}
impl EventEmitter<Play> for FeedView {}
impl EventEmitter<ItemAction> for FeedView {}

impl FeedView {
    pub fn new(source: FeedSource, title: Option<SharedString>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            source,
            title,
            page: YtFeedPage::default(),
            phase: Phase::Loading,
            loading_more: false,
            more_failed: None,
            generation: 0,
            scroll: ScrollHandle::new(),
        };
        view.reload(cx);
        view
    }

    pub fn title(&self) -> Option<SharedString> {
        self.title.clone().or_else(|| self.page.page_header.as_ref()?.title.clone().map(Into::into))
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.phase = Phase::Loading;
        self.fetch(None, false, cx);
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(continuation) = self.page.continuation.clone() else { return };
        if self.loading_more || self.more_failed.is_some() {
            return;
        }
        self.loading_more = true;
        self.fetch(Some(continuation), true, cx);
    }

    /// A chip or sort token re-serves the page in place.
    fn refilter(&mut self, token: String, cx: &mut Context<Self>) {
        self.phase = Phase::Loading;
        self.fetch(Some(token), false, cx);
    }

    fn fetch(&mut self, continuation: Option<String>, append: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine(), graph.log.fork("feed"));
        let load = on_core(cx, self.source.clone().load(engine, continuation, log));
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |view, cx| {
                if view.generation != generation {
                    return;
                }
                view.loading_more = false;
                match result {
                    Some(Ok(page)) if append => view.append(page),
                    Some(Ok(page)) => view.replace(page),
                    Some(Err(error)) if append => view.more_failed = Some(error),
                    Some(Err(error)) => view.phase = Phase::Failed(error),
                    None => {}
                }
                cx.notify();
            })
        })
        .detach();
    }

    fn replace(&mut self, mut page: YtFeedPage) {
        // A filtered re-serve omits the menus; they carry over.
        if page.chips.is_empty() {
            page.chips = std::mem::take(&mut self.page.chips);
        }
        if page.page_header.is_none() {
            page.page_header = self.page.page_header.take();
            page.page_tabs = std::mem::take(&mut self.page.page_tabs);
        }
        self.page = page;
        self.phase = Phase::Ready;
        self.more_failed = None;
    }

    fn append(&mut self, page: YtFeedPage) {
        self.page.sections.extend(page.sections);
        self.page.continuation = page.continuation;
    }

    fn open_item(&mut self, item: &YtFeedItem, queue: Option<Vec<YtFeedItem>>, cx: &mut Context<Self>) {
        let title = Some(SharedString::from(item.title.clone()));
        let source = match item.kind.as_str() {
            "video" => {
                cx.emit(Play { item: item.clone(), queue });
                return;
            }
            "playlist" => item
                .url
                .as_deref()
                .map(|url| FeedSource::from_link(url).unwrap_or(FeedSource::Playlist(url.to_owned()))),
            "channel" => {
                item.url.as_deref().map(|url| FeedSource::from_link(url).unwrap_or(FeedSource::Channel(url.to_owned())))
            }
            _ => item.url.as_deref().and_then(FeedSource::from_link),
        };
        if let Some(source) = source {
            cx.emit(Navigate { source, title });
        }
    }

    fn open_link(&mut self, link: &str, title: Option<SharedString>, cx: &mut Context<Self>) {
        if let Some(source) = FeedSource::from_link(link) {
            cx.emit(Navigate { source, title });
        }
    }

    fn open_tab(&mut self, url: String, cx: &mut Context<Self>) {
        if let Some(source) = FeedSource::from_link(&url) {
            self.source = source;
            self.phase = Phase::Loading;
            self.fetch(None, false, cx);
        }
    }
}

/// What the user sees for a failure: a localized category, the server's own message, or the
/// generic message with the code to report.
pub fn error_text(error: &EngineError) -> String {
    match error.kind {
        ErrorKind::Network => t(Str::ErrorNetwork).to_owned(),
        _ => match error.details.as_ref().and_then(|details| details.user_message.clone()) {
            Some(message) => format!("{message} ({})", error.code),
            None => tf(Str::ErrorGeneric, &[&error.code]),
        },
    }
}

fn section_title(header: &YtSectionHeader) -> Option<String> {
    let localized = header.title_key.as_deref().and_then(|key| {
        Some(match key {
            "trending_music_videos" => Str::ChartTrendingMusicVideos,
            "top_songs" => Str::ChartTopSongs,
            "top_songs_daily" => Str::ChartTopSongsDaily,
            "top_songs_weekly" => Str::ChartTopSongsWeekly,
            "top_music_videos" => Str::ChartTopMusicVideos,
            "top_music_videos_daily" => Str::ChartTopMusicVideosDaily,
            "top_music_videos_weekly" => Str::ChartTopMusicVideosWeekly,
            "top_songs_on_shorts" => Str::ChartTopSongsOnShorts,
            "top_songs_on_shorts_daily" => Str::ChartTopSongsOnShortsDaily,
            "top_songs_on_shorts_weekly" => Str::ChartTopSongsOnShortsWeekly,
            "subscribed_channels" => Str::SectionSubscribedChannels,
            _ => return None,
        })
    });
    localized.map(|key| t(key).to_owned()).or_else(|| header.title.clone())
}

/// The smallest variant at least `width` px wide, else the largest.
fn thumb_for(thumbs: &[YtThumb], width: i32) -> Option<SharedString> {
    thumbs
        .iter()
        .find(|thumb| thumb.w.is_some_and(|w| w >= width))
        .or_else(|| thumbs.last())
        .map(|thumb| absolute_url(&thumb.url).into())
}

fn absolute_url(url: &str) -> String {
    if url.starts_with("//") { format!("https:{url}") } else { url.to_owned() }
}

fn centered(content: impl IntoElement) -> Div {
    div().size_full().flex().items_center().justify_center().child(content)
}

impl FeedView {
    fn render_header(&self, header: &YtPageHeader, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let avatar = thumb_for(&header.thumbs, 160);
        h_flex()
            .gap_4()
            .pb_2()
            .when_some(avatar, |row, url| {
                row.child(img(url).size(px(88.)).rounded_full().object_fit(ObjectFit::Cover).flex_shrink_0())
            })
            .child(
                v_flex()
                    .gap_1()
                    .min_w_0()
                    .when_some(header.title.clone(), |column, title| {
                        column.child(div().text_xl().font_semibold().child(title))
                    })
                    .when_some(header.subtitle_line1.clone(), |column, line| {
                        column.child(div().text_sm().text_color(theme.muted_foreground).child(line))
                    })
                    .when_some(header.subtitle_line2.clone(), |column, line| {
                        column.child(div().text_sm().text_color(theme.muted_foreground).child(line))
                    }),
            )
    }

    fn render_item(
        &self,
        item: &YtFeedItem,
        index: usize,
        queue: Option<std::rc::Rc<Vec<YtFeedItem>>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (width, height, round) = match item.kind.as_str() {
            "channel" => (68., 68., true),
            _ => (120., 68., false),
        };
        let thumb = thumb_for(&item.thumbs, (width * 2.) as i32);
        let badge = if item.live { None } else { item.duration.clone() };
        let clicked = item.clone();
        let row = h_flex()
            .id(("feed-item", index))
            .gap_3()
            .p_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(theme.secondary))
            .child(
                div()
                    .relative()
                    .flex_shrink_0()
                    .w(px(width))
                    .h(px(height))
                    .overflow_hidden()
                    .bg(theme.secondary)
                    .when(round, |frame| frame.rounded_full())
                    .when(!round, |frame| frame.rounded_md())
                    .when_some(thumb, |frame, url| frame.child(img(url).size_full().object_fit(ObjectFit::Cover)))
                    .when_some(badge, |frame, text| {
                        frame.child(
                            div()
                                .absolute()
                                .bottom_1()
                                .right_1()
                                .px_1()
                                .rounded_sm()
                                .bg(black().opacity(0.75))
                                .text_xs()
                                .text_color(white())
                                .child(text),
                        )
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(div().text_sm().font_medium().line_clamp(2).child(item.title.clone()))
                    .when_some(item.subtitle_line1.clone(), |column, line| {
                        column.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(line))
                    })
                    .when_some(item.subtitle_line2.clone(), |column, line| {
                        column.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(line))
                    }),
            )
            .on_click(cx.listener(move |view, _, _, cx| {
                let queue =
                    queue.as_ref().map(|items| items.iter().filter(|item| item.kind == "video").cloned().collect());
                view.open_item(&clicked, queue, cx)
            }));
        if item.kind != "video" {
            return row.into_any_element();
        }
        let view = cx.entity().downgrade();
        let target = item.clone();
        row.context_menu(move |mut menu, _, _| {
            let actions = [
                (Str::MenuPlay, None),
                (Str::QueueNext, Some(FeedAction::QueueNext)),
                (Str::QueueLast, Some(FeedAction::QueueLast)),
                (Str::MenuStartRadio, Some(FeedAction::Radio)),
                (Str::DownloadTitle, Some(FeedAction::Download)),
            ];
            for (label, action) in actions {
                let (view, item) = (view.clone(), target.clone());
                menu = menu.item(PopupMenuItem::new(t(label)).on_click(move |_, _, cx| {
                    let item = item.clone();
                    view.update(cx, |_, cx| match action {
                        Some(action) => cx.emit(ItemAction { item, action }),
                        None => cx.emit(Play { item, queue: None }),
                    })
                    .ok();
                }));
            }
            menu
        })
        .into_any_element()
    }
}

impl Render for FeedView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.phase {
            Phase::Loading => return centered(Spinner::new().large()).into_any_element(),
            Phase::Failed(error) => {
                let message = error_text(error);
                return centered(
                    v_flex().gap_3().items_center().max_w(px(420.)).child(div().text_center().child(message)).child(
                        Button::new("retry")
                            .label(t(Str::Retry))
                            .on_click(cx.listener(|view, _, _, cx| view.reload(cx))),
                    ),
                )
                .into_any_element();
            }
            Phase::Ready => {}
        }

        // Infinite scroll: near the bottom, fetch the next page.
        let offset = self.scroll.offset().y.abs();
        let max = self.scroll.max_offset().y;
        if self.page.continuation.is_some() && max > px(0.) && max - offset < px(800.) {
            cx.defer_in(window, |view, _, cx| view.load_more(cx));
        }

        let theme = cx.theme().clone();
        let mut column = v_flex().gap_4().p_4();
        if let Some(header) = &self.page.page_header {
            column = column.child(self.render_header(header, cx));
        }
        if !self.page.page_tabs.is_empty() {
            let tabs = self.page.page_tabs.clone();
            let selected = tabs.iter().position(|tab| tab.selected).unwrap_or(0);
            let urls: Vec<Option<String>> = tabs.iter().map(|tab| tab.url.clone()).collect();
            let entity = cx.entity().downgrade();
            column = column.child(
                gpui_kit::component::tab::TabBar::new("page-tabs")
                    .underline()
                    .selected_index(selected)
                    .children(tabs.iter().map(|tab| gpui_kit::component::tab::Tab::new().label(tab.title.clone())))
                    .on_click(move |index, _, cx| {
                        if let Some(Some(url)) = urls.get(*index).cloned() {
                            entity.update(cx, |view, cx| view.open_tab(url, cx)).ok();
                        }
                    }),
            );
        }
        if !self.page.chips.is_empty() {
            let mut chips = h_flex().gap_2().flex_wrap();
            for (index, chip) in self.page.chips.clone().into_iter().enumerate() {
                let token = chip.token.clone();
                let mut button = Button::new(("chip", index)).label(chip.title.clone()).small();
                button = if chip.selected { button.primary() } else { button.outline() };
                chips = chips.child(button.on_click(cx.listener(move |view, _, _, cx| {
                    if let Some(token) = token.clone() {
                        view.refilter(token, cx);
                    }
                })));
            }
            column = column.child(chips);
        }

        let item_total: usize = self.page.sections.iter().map(|section| section.items.len()).sum();
        if item_total == 0 {
            let title = self.page.empty_title.clone().unwrap_or_else(|| t(Str::FeedEmpty).to_owned());
            column = column.child(
                v_flex()
                    .py_16()
                    .gap_2()
                    .items_center()
                    .child(div().text_lg().child(title))
                    .when_some(self.page.empty_message.clone(), |empty, message| {
                        empty.child(div().text_sm().text_color(theme.muted_foreground).child(message))
                    }),
            );
        }
        let mut index = 0;
        for (section_index, section) in self.page.sections.clone().into_iter().enumerate() {
            let mut block = v_flex().gap_1();
            if let Some(header) = &section.header {
                let cta = header.cta_deeplink.clone().zip(header.cta_text.clone());
                let title = section_title(header);
                block =
                    block.child(
                        h_flex()
                            .justify_between()
                            .pb_1()
                            .child(div().text_lg().font_semibold().children(title.clone()))
                            .when_some(cta, |row, (link, label)| {
                                let title = title.clone().map(SharedString::from);
                                row.child(Button::new(("cta", section_index)).ghost().small().label(label).on_click(
                                    cx.listener(move |view, _, _, cx| view.open_link(&link, title.clone(), cx)),
                                ))
                            }),
                    );
            }
            let queue =
                (section.play_mode.as_deref() == Some("queue")).then(|| std::rc::Rc::new(section.items.clone()));
            for item in &section.items {
                block = block.child(self.render_item(item, index, queue.clone(), cx));
                index += 1;
            }
            column = column.child(block);
        }
        if self.loading_more {
            column = column.child(h_flex().justify_center().py_4().child(Spinner::new()));
        }
        if let Some(error) = &self.more_failed {
            column = column.child(
                v_flex().items_center().gap_2().py_4().child(div().text_sm().child(error_text(error))).child(
                    Button::new("retry-more").small().label(t(Str::Retry)).on_click(cx.listener(|view, _, _, cx| {
                        view.more_failed = None;
                        view.load_more(cx);
                    })),
                ),
            );
        }
        div()
            .id("feed-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(column)
            .into_any_element()
    }
}
