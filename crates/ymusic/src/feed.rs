//! A feed surface: one engine source (a Discover tab, search results, a playlist / channel / music
//! page), paged by continuation, rendered as titled sections of display items.

use std::sync::Arc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Sizable, StyledExt, WindowExt, h_flex, v_flex,
};
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
    MusicExplore,
    MusicNewReleases,
    MusicCharts,
    /// The signed-in account's YT Music library.
    MusicLibrary,
    /// The most-watched YouTube videos from play history.
    TopTracks,
    /// The signed-in account's library.
    Account,
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
        history: Arc<ym_history::PlayHistory>,
        continuation: Option<String>,
        log: Logger,
    ) -> Result<YtFeedPage, EngineError> {
        let continuation = continuation.as_deref();
        match &self {
            Self::TopTracks => top_tracks(history).await,
            Self::Account => engine.account_library(continuation, &log).await,
            Self::Home => engine.home_feed(continuation, false, &log).await,
            Self::Trending => engine.trending_music_feed(&log).await,
            Self::Recommended => engine.recommended_feed(&log).await,
            Self::MusicHome => engine.music_home(continuation, false, &log).await,
            Self::MusicExplore => engine.music_explore(continuation, &log).await,
            Self::MusicNewReleases => engine.music_new_releases(continuation, &log).await,
            Self::MusicCharts => engine.music_charts(continuation, &log).await,
            Self::MusicLibrary => engine.music_library(continuation, &log).await,
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

/// Play history's most-watched videos as one section that plays in order.
async fn top_tracks(history: Arc<ym_history::PlayHistory>) -> Result<YtFeedPage, EngineError> {
    let videos = tokio::task::spawn_blocking(move || history.youtube_top(ym_history::RETAIN))
        .await
        .map_err(|error| EngineError::site("R90020", format!("top tracks: {error}")))?
        .map_err(|error| EngineError::site("R90020", format!("top tracks: {error}")))?;
    let items = videos
        .into_iter()
        .map(|video| YtFeedItem {
            kind: "video".into(),
            thumbs: vec![ym_engine::yt::YtThumb {
                url: video
                    .thumbnail_url
                    .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", video.video_id)),
                w: Some(320),
                h: Some(180),
            }],
            id: video.video_id,
            title: video.title,
            subtitle_line1: video.artist.clone(),
            channel_title: video.artist,
            duration: video.duration,
            duration_ms: video.duration_ms,
            ..YtFeedItem::default()
        })
        .collect::<Vec<_>>();
    let sections = if items.is_empty() {
        Vec::new()
    } else {
        vec![ym_engine::yt::YtSection {
            layout: "grid".into(),
            items,
            play_mode: Some("queue".into()),
            ..Default::default()
        }]
    };
    Ok(YtFeedPage { sections, ..YtFeedPage::default() })
}

/// A request to play a video; `queue` is set when its section plays in order.
pub struct Play {
    pub item: YtFeedItem,
    pub queue: Option<Vec<YtFeedItem>>,
    pub shuffle: bool,
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
    /// Collapsible sections the user expanded, by index.
    expanded: std::collections::HashSet<usize>,
    /// A header action (save, subscribe) is running.
    header_busy: bool,
}

/// Which page header a detail page shows.
#[derive(Clone, Copy, PartialEq)]
enum HeaderKind {
    /// Title, meta, and the actions row ending in the shuffle circle.
    Playlist,
    /// Banner, avatar, name and Subscribe.
    Channel,
    /// A YT Music category page: Play all and Shuffle all pills.
    Category,
    Generic,
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
            expanded: std::collections::HashSet::new(),
            header_busy: false,
        };
        view.reload(cx);
        view
    }

    pub fn title(&self) -> Option<SharedString> {
        self.title.clone().or_else(|| self.page.page_header.as_ref()?.title.clone().map(Into::into))
    }

    /// Playlists title their bar "Playlist", not with the playlist's name.
    pub fn is_playlist_page(&self) -> bool {
        self.header_kind() == HeaderKind::Playlist
    }

    fn header_kind(&self) -> HeaderKind {
        let header = self.page.page_header.as_ref();
        let channel_like = header.is_some_and(|header| {
            header.channel_id.is_some() || header.subscribed.is_some() || !header.banner.is_empty()
        });
        let playlist_like =
            header.is_some_and(|header| header.save_playlist_id.is_some() || header.item_count.is_some());
        match &self.source {
            FeedSource::Playlist(_) => HeaderKind::Playlist,
            FeedSource::Channel(_) if header.is_some() => HeaderKind::Channel,
            FeedSource::MusicBrowse(_) if channel_like => HeaderKind::Channel,
            FeedSource::MusicBrowse(_) if playlist_like => HeaderKind::Playlist,
            FeedSource::MusicBrowse(_) => HeaderKind::Category,
            _ => HeaderKind::Generic,
        }
    }

    fn videos(&self) -> Vec<YtFeedItem> {
        self.page
            .sections
            .iter()
            .flat_map(|section| section.items.iter())
            .filter(|item| item.kind == "video")
            .cloned()
            .collect()
    }

    fn play_all(&mut self, shuffle: bool, cx: &mut Context<Self>) {
        let videos = self.videos();
        if videos.is_empty() {
            return;
        }
        let start = if shuffle { rand::Rng::random_range(&mut rand::rng(), 0..videos.len()) } else { 0 };
        cx.emit(Play { item: videos[start].clone(), queue: Some(videos), shuffle });
    }

    fn share_page(&self) -> Option<String> {
        match &self.source {
            FeedSource::Playlist(url) | FeedSource::Channel(url) if url.starts_with('/') => {
                Some(format!("https://www.youtube.com{url}"))
            }
            _ => {
                let header = self.page.page_header.as_ref()?;
                header
                    .save_playlist_id
                    .as_ref()
                    .map(|id| format!("https://www.youtube.com/playlist?list={id}"))
                    .or_else(|| header.channel_id.as_ref().map(|id| format!("https://www.youtube.com/channel/{id}")))
            }
        }
    }

    /// Saves the playlist to (or removes it from) the account library.
    fn toggle_saved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(header) = &self.page.page_header else { return };
        let (Some(playlist_id), Some(saved)) = (header.save_playlist_id.clone(), header.saved) else { return };
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine(), graph.log.fork("save-playlist"));
        let request = on_core(cx, async move { engine.save_playlist(&playlist_id, !saved, &log).await });
        self.run_header_action(request, window, cx, move |header| {
            header.saved = Some(!saved);
            Some(t(if saved { Str::PlaylistRemoved } else { Str::PlaylistSaved }))
        });
    }

    fn toggle_subscribed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(header) = &self.page.page_header else { return };
        let (Some(channel_id), Some(subscribed)) = (header.channel_id.clone(), header.subscribed) else { return };
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine(), graph.log.fork("subscribe"));
        let request = on_core(cx, async move { engine.subscribe_channel(&channel_id, !subscribed, &log).await });
        self.run_header_action(request, window, cx, move |header| {
            header.subscribed = Some(!subscribed);
            None
        });
    }

    /// Runs a header request: busy until it lands, then applies `done` (showing the message it
    /// returns), or shows the error.
    fn run_header_action(
        &mut self,
        request: impl Future<Output = Option<Result<(), EngineError>>> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut YtPageHeader) -> Option<&'static str> + 'static,
    ) {
        if self.header_busy {
            return;
        }
        self.header_busy = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |view, window, cx| {
                view.header_busy = false;
                match result {
                    Some(Ok(())) => {
                        if let Some(message) = view.page.page_header.as_mut().and_then(done) {
                            window.push_notification(message, cx);
                        }
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
        let (engine, history, log) = (graph.engine(), graph.history.clone(), graph.log.fork("feed"));
        let load = on_core(cx, self.source.clone().load(engine, history, continuation, log));
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
                cx.emit(Play { item: item.clone(), queue, shuffle: false });
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
    fn render_header(&self, header: &YtPageHeader, cx: &mut Context<Self>) -> AnyElement {
        match self.header_kind() {
            HeaderKind::Playlist => self.render_playlist_header(header, cx),
            HeaderKind::Channel => self.render_channel_header(header, cx),
            HeaderKind::Category => self.render_category_bar(cx),
            HeaderKind::Generic => self.render_generic_header(header, cx),
        }
    }

    /// Title and meta, then save · share ... the shuffle circle, over an inset divider.
    fn render_playlist_header(&self, header: &YtPageHeader, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let meta = [header.subtitle_line1.clone(), header.subtitle_line2.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let share = self.share_page();
        let has_videos = self.page.sections.iter().any(|section| section.items.iter().any(|item| item.kind == "video"));
        let mut actions = h_flex().px_4().py_2().gap_4().items_center();
        if let Some(saved) = header.saved.filter(|_| header.save_playlist_id.is_some()) {
            let label = t(if saved { Str::CdRemovePlaylistFromLibrary } else { Str::CdSavePlaylist });
            actions = actions.child(if self.header_busy {
                Spinner::new().into_any_element()
            } else {
                Button::new("playlist-save")
                    .ghost()
                    .icon(if saved { Lucide::ListCheck } else { Lucide::ListPlus })
                    .selected(saved)
                    .accessibility_label(label)
                    .tooltip(label)
                    .on_click(cx.listener(|view, _, window, cx| view.toggle_saved(window, cx)))
                    .into_any_element()
            });
        }
        actions = actions.child(
            Button::new("playlist-share")
                .ghost()
                .icon(Lucide::Share2)
                .disabled(share.is_none())
                .accessibility_label(t(Str::CdShare))
                .tooltip(t(Str::CdShare))
                .on_click(move |_, window, cx| {
                    if let Some(link) = share.clone() {
                        cx.write_to_clipboard(ClipboardItem::new_string(link));
                        window.push_notification(t(Str::LinkCopied), cx);
                    }
                }),
        );
        actions = actions.child(div().flex_1()).child(
            div()
                .id("playlist-shuffle")
                .role(Role::Button)
                .aria_label(t(Str::ShuffleAll))
                .tab_stop(true)
                .size(px(46.))
                .flex_shrink_0()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.primary)
                .text_color(theme.primary_foreground)
                .when(has_videos, |circle| circle.cursor_pointer())
                .when(!has_videos, |circle| circle.opacity(0.5))
                .child(Icon::new(Lucide::Shuffle))
                .on_click(cx.listener(|view, _, _, cx| view.play_all(true, cx))),
        );
        v_flex()
            .child(
                v_flex()
                    .px_4()
                    .pt_2()
                    .gap_1()
                    .when_some(header.title.clone(), |column, title| {
                        column.child(div().text_xl().font_semibold().child(title))
                    })
                    .when(!meta.is_empty(), |column| {
                        column.child(div().text_sm().text_color(theme.muted_foreground).child(meta))
                    }),
            )
            .child(actions)
            .child(div().mx_4().mb_2().h(px(1.)).bg(theme.border))
            .into_any_element()
    }

    /// The banner (when there is one), then avatar, name, meta and Subscribe.
    fn render_channel_header(&self, header: &YtPageHeader, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let avatar = thumb_for(&header.thumbs, 128);
        let banner = thumb_for(&header.banner, 1280);
        let meta = [header.subtitle_line1.clone(), header.subtitle_line2.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let subscribe = header.subscribed.filter(|_| header.channel_id.is_some()).map(|subscribed| {
            let label = t(if subscribed { Str::Subscribed } else { Str::Subscribe });
            let pill = Button::new("subscribe").rounded_full().label(label).loading(self.header_busy);
            let pill = if subscribed { pill.outline() } else { pill.primary() };
            pill.on_click(cx.listener(|view, _, window, cx| view.toggle_subscribed(window, cx)))
        });
        v_flex()
            .when_some(banner, |column, url| {
                column.child(
                    div()
                        .mx_4()
                        .mt_2()
                        .h(px(96.))
                        .rounded(px(12.))
                        .overflow_hidden()
                        .bg(theme.secondary)
                        .child(img(url).size_full().object_fit(ObjectFit::Cover)),
                )
            })
            .child(
                h_flex()
                    .px_4()
                    .py_2()
                    .gap_4()
                    .items_center()
                    .when_some(avatar, |row, url| {
                        row.child(img(url).size(px(64.)).rounded_full().object_fit(ObjectFit::Cover).flex_shrink_0())
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .when_some(header.title.clone(), |column, title| {
                                column.child(div().font_semibold().truncate().child(title))
                            })
                            .when(!meta.is_empty(), |column| {
                                column.child(
                                    div()
                                        .pt(px(2.))
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .truncate()
                                        .child(meta),
                                )
                            }),
                    )
                    .children(subscribe),
            )
            .into_any_element()
    }

    /// Play all (filled) and Shuffle all (outlined) pills.
    fn render_category_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .px_4()
            .py_2()
            .gap_2()
            .child(
                Button::new("play-all")
                    .primary()
                    .rounded_full()
                    .icon(Lucide::Play)
                    .label(t(Str::PlayAll))
                    .on_click(cx.listener(|view, _, _, cx| view.play_all(false, cx))),
            )
            .child(
                Button::new("shuffle-all")
                    .outline()
                    .rounded_full()
                    .icon(Lucide::Shuffle)
                    .label(t(Str::ShuffleAll))
                    .on_click(cx.listener(|view, _, _, cx| view.play_all(true, cx))),
            )
            .into_any_element()
    }

    fn render_generic_header(&self, header: &YtPageHeader, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let avatar = thumb_for(&header.thumbs, 160);
        h_flex()
            .px_4()
            .pt_2()
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
            .into_any_element()
    }
}

/// Grid and list geometry for a feed this wide (measured as if the rail were expanded).
#[derive(Clone, Copy)]
struct Bands {
    columns: usize,
    margin: f32,
    gutter: f32,
    /// List thumbnails: square below 320 wide, 16:9 below 470.
    square: bool,
}

const CARD_MAX: f32 = 324.;
const ROW_GAP: f32 = 16.;

impl Bands {
    fn for_width(width: f32) -> Self {
        let (margin, gutter) = if width >= 840. { (24., 16.) } else { (16., 8.) };
        let columns = if width < 470. {
            1
        } else {
            (((width - 2. * margin + gutter) / (CARD_MAX + gutter)).ceil() as usize).max(2)
        };
        Self { columns, margin, gutter, square: width < 320. }
    }

    fn inset(&self) -> f32 {
        self.gutter / 2.
    }
}

/// The feed's width as the JVM app measures it: the window, less the expanded rail in rail mode.
pub fn feed_width(window: &Window) -> f32 {
    let viewport = window.viewport_size();
    let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
    if width >= 750. && height >= 480. { width - 280. } else { width }
}

/// What a screen reader says for a card or row.
fn item_label(item: &YtFeedItem) -> String {
    let kind = match item.kind.as_str() {
        "playlist" if item.album => t(Str::A11yKindAlbum),
        "playlist" => t(Str::A11yKindPlaylist),
        "channel" => t(Str::A11yKindChannel),
        _ if item.live => t(Str::A11yKindLive),
        _ if item.music_video => t(Str::A11yKindMusicVideo),
        _ => t(Str::A11yKindVideo),
    };
    [
        Some(item.title.clone()),
        item.subtitle_line1.clone(),
        item.subtitle_line2.clone(),
        item.duration.clone(),
        Some(kind.to_owned()),
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(", ")
}

fn subtitle(item: &YtFeedItem) -> Option<String> {
    let parts: Vec<String> = [item.subtitle_line1.clone(), item.subtitle_line2.clone()].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn badge(item: &YtFeedItem) -> Option<String> {
    if item.live {
        return Some(t(Str::BadgeLive).to_owned());
    }
    match item.kind.as_str() {
        "playlist" => item.item_count.clone(),
        _ => item.duration.clone(),
    }
}

impl FeedView {
    fn more_menu(&self, item: &YtFeedItem, key: (&'static str, usize), cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity().downgrade();
        let target = item.clone();
        let top_tracks = matches!(self.source, FeedSource::TopTracks);
        Button::new(key)
            .ghost()
            .small()
            .icon(Icon::new(IconName::EllipsisVertical))
            .accessibility_label(t(Str::CdMore))
            .tooltip(t(Str::CdMore))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                let video = target.kind == "video";
                let mut actions: Vec<(Str, MenuAction)> = Vec::new();
                if video {
                    actions.push((Str::MenuPlaySingle, MenuAction::PlaySingle));
                    actions.push((Str::MenuStartRadio, MenuAction::Feed(FeedAction::Radio)));
                    actions.push((Str::QueueNext, MenuAction::Feed(FeedAction::QueueNext)));
                    actions.push((Str::QueueLast, MenuAction::Feed(FeedAction::QueueLast)));
                    if !target.live {
                        actions.push((Str::DownloadTitle, MenuAction::Feed(FeedAction::Download)));
                    }
                }
                if share_link(&target).is_some() {
                    actions.push((Str::CdShare, MenuAction::Share));
                }
                if video && top_tracks {
                    actions.push((Str::MenuRemove, MenuAction::RemoveFromTop));
                }
                for (label, action) in actions {
                    let (view, item) = (view.clone(), target.clone());
                    menu = menu.item(PopupMenuItem::new(t(label)).on_click(move |_, window, cx| {
                        let item = item.clone();
                        view.update(cx, |view, cx| view.menu_action(item, action, window, cx)).ok();
                    }));
                }
                menu
            })
            .into_any_element()
    }

    fn menu_action(&mut self, item: YtFeedItem, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            MenuAction::PlaySingle => cx.emit(Play { item: item.clone(), queue: Some(vec![item]), shuffle: false }),
            MenuAction::Feed(action) => cx.emit(ItemAction { item, action }),
            MenuAction::Share => {
                if let Some(link) = share_link(&item) {
                    cx.write_to_clipboard(ClipboardItem::new_string(link));
                    window.push_notification(t(Str::LinkCopied), cx);
                }
            }
            MenuAction::RemoveFromTop => {
                let history = cx.global::<AppGraph>().history.clone();
                let id = item.id.clone();
                let removed =
                    on_core(cx, async move { tokio::task::spawn_blocking(move || history.delete_youtube(&id)).await });
                cx.spawn(async move |this, cx| {
                    if let Some(Ok(Err(error))) = removed.await {
                        this.update(cx, |_, cx| {
                            cx.global::<AppGraph>().log.warn("top track removal failed", Some(&error))
                        })
                        .ok();
                    }
                    this.update(cx, |view, cx| view.reload(cx)).ok();
                })
                .detach();
            }
        }
    }

    fn clickable(
        &self,
        id: (&'static str, usize),
        item: &YtFeedItem,
        queue: Option<std::rc::Rc<Vec<YtFeedItem>>>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let clicked = item.clone();
        let theme = cx.theme();
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(item_label(item))
            .tab_stop(true)
            .cursor_pointer()
            .rounded(px(12.))
            .hover(|style| style.bg(theme.secondary))
            .on_click(cx.listener(move |view, _, _, cx| {
                let queue =
                    queue.as_ref().map(|items| items.iter().filter(|item| item.kind == "video").cloned().collect());
                view.open_item(&clicked, queue, cx)
            }))
    }

    /// A grid card: 16:9 thumbnail on top, title and subtitle below with the ⋮ menu beside them;
    /// a channel is a centered avatar instead.
    fn render_card(
        &self,
        item: &YtFeedItem,
        index: usize,
        bands: Bands,
        queue: Option<std::rc::Rc<Vec<YtFeedItem>>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let card = self.clickable(("feed-card", index), item, queue, cx).flex_1().min_w_0().p(px(bands.inset()));
        if item.kind == "channel" {
            return card
                .relative()
                .child(
                    v_flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .size(px(76.))
                                .rounded_full()
                                .overflow_hidden()
                                .bg(theme.secondary)
                                .when_some(thumb_for(&item.thumbs, 152), |frame, url| {
                                    frame.child(img(url).size_full().object_fit(ObjectFit::Cover))
                                }),
                        )
                        .child(div().pt_1().text_sm().font_medium().truncate().child(item.title.clone()))
                        .when_some(subtitle(item), |column, line| {
                            column.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(line))
                        }),
                )
                .child(div().absolute().top_0().right_0().child(self.more_menu(item, ("card-more", index), cx)))
                .into_any_element();
        }
        card.child(
            div()
                .relative()
                .w_full()
                .aspect_ratio(16. / 9.)
                .rounded(px(5.))
                .overflow_hidden()
                .bg(theme.secondary)
                .when_some(thumb_for(&item.thumbs, 640), |frame, url| {
                    frame.child(img(url).size_full().object_fit(ObjectFit::Cover))
                })
                .when_some(badge(item), |frame, text| {
                    frame.child(
                        div()
                            .absolute()
                            .bottom_1()
                            .right_1()
                            .px(px(5.))
                            .py(px(2.))
                            .rounded(px(3.))
                            .bg(black().opacity(0.75))
                            .text_xs()
                            .font_semibold()
                            .text_color(white())
                            .child(text),
                    )
                }),
        )
        .child(
            h_flex()
                .pt_2()
                .gap_2()
                .items_start()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().font_medium().line_clamp(2).child(item.title.clone()))
                        .when_some(subtitle(item), |column, line| {
                            column.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(line))
                        }),
                )
                .child(self.more_menu(item, ("card-more", index), cx)),
        )
        .into_any_element()
    }

    /// A list row: thumbnail (square, 16:9, or a channel circle), then title and subtitle, ⋮ at the end.
    fn render_row(
        &self,
        item: &YtFeedItem,
        index: usize,
        bands: Bands,
        queue: Option<std::rc::Rc<Vec<YtFeedItem>>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let (width, height, round) = match item.kind.as_str() {
            "channel" => (74., 74., true),
            _ if bands.square => (92., 92., false),
            _ => (160., 90., false),
        };
        self.clickable(("feed-row", index), item, queue, cx)
            .child(
                h_flex()
                    .pl_4()
                    .pr_1()
                    .py(px(10.))
                    .gap_4()
                    .items_start()
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
                            .w(px(width))
                            .h(px(height))
                            .overflow_hidden()
                            .bg(theme.secondary)
                            .when(round, |frame| frame.rounded_full())
                            .when(!round, |frame| frame.rounded(px(6.)))
                            .when_some(thumb_for(&item.thumbs, (width * 2.) as i32), |frame, url| {
                                frame.child(img(url).size_full().object_fit(ObjectFit::Cover))
                            })
                            .when_some(badge(item).filter(|_| !round), |frame, text| {
                                frame.child(
                                    div()
                                        .absolute()
                                        .bottom_1()
                                        .right_1()
                                        .px(px(5.))
                                        .py(px(2.))
                                        .rounded(px(3.))
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
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .line_clamp(if bands.square { 2 } else { 3 })
                                    .child(item.title.clone()),
                            )
                            .when_some(subtitle(item), |column, line| {
                                column
                                    .child(div().text_xs().text_color(theme.muted_foreground).line_clamp(2).child(line))
                            }),
                    )
                    .child(self.more_menu(item, ("row-more", index), cx)),
            )
            .into_any_element()
    }

    /// Accent tick, optional avatar, title, count, then "View all" or the collapse chevron.
    fn render_section_header(
        &self,
        header: Option<&YtSectionHeader>,
        section_index: usize,
        collapsible: bool,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let title = header.and_then(section_title);
        let avatar = header.and_then(|header| thumb_for(&header.icon_thumbs, 52));
        let count = header.and_then(|header| header.count.clone());
        let cta = header.and_then(|header| header.cta_deeplink.clone().zip(header.cta_text.clone()));
        h_flex()
            .px_4()
            .pt_4()
            .pb_2()
            .gap_2()
            .items_center()
            .child(div().w(px(4.)).h(px(16.)).rounded_sm().bg(theme.primary))
            .when_some(avatar, |row, url| row.child(img(url).size(px(26.)).rounded_full().object_fit(ObjectFit::Cover)))
            .child(
                div()
                    .id(("section-title", section_index))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(16.5))
                    .font_bold()
                    .role(Role::Heading)
                    .children(title.clone()),
            )
            .when_some(count, |row, count| row.child(div().text_sm().text_color(theme.muted_foreground).child(count)))
            .when_some(cta.filter(|_| !collapsible), |row, (link, label)| {
                let title = title.clone().map(SharedString::from);
                row.child(
                    Button::new(("cta", section_index))
                        .ghost()
                        .small()
                        .label(label)
                        .icon(Icon::new(IconName::ChevronRight))
                        .on_click(cx.listener(move |view, _, _, cx| view.open_link(&link, title.clone(), cx))),
                )
            })
            .when(collapsible, |row| {
                row.child(
                    Button::new(("collapse", section_index))
                        .ghost()
                        .small()
                        .icon(Icon::new(if expanded { IconName::ChevronUp } else { IconName::ChevronDown }))
                        .accessibility_label(t(if expanded { Str::CdCollapse } else { Str::CdExpand }))
                        .tooltip(t(if expanded { Str::CdCollapse } else { Str::CdExpand }))
                        .on_click(cx.listener(move |view, _, _, cx| view.toggle_section(section_index, cx))),
                )
            })
            .into_any_element()
    }

    fn toggle_section(&mut self, section_index: usize, cx: &mut Context<Self>) {
        if !self.expanded.remove(&section_index) {
            self.expanded.insert(section_index);
        }
        cx.notify();
    }

    fn render_section(
        &self,
        section: &ym_engine::yt::YtSection,
        section_index: usize,
        first_item: usize,
        bands: Bands,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let expanded = self.expanded.contains(&section_index);
        let mut block = v_flex().w_full();
        if section_index > 0 {
            block = block.child(div().px_4().pt_4().child(div().h(px(1.)).w_full().bg(theme.border)));
        }
        if section.header.is_some() || section.collapsible {
            block = block.child(self.render_section_header(
                section.header.as_ref(),
                section_index,
                section.collapsible,
                expanded,
                cx,
            ));
        }
        let queue = (section.play_mode.as_deref() == Some("queue")).then(|| std::rc::Rc::new(section.items.clone()));
        match section.layout.as_str() {
            "tags" => {
                let shown =
                    if section.collapsible && !expanded { 10.min(section.items.len()) } else { section.items.len() };
                let mut pills = h_flex().flex_wrap().gap_2().px_4().py_1();
                for (offset, item) in section.items.iter().take(shown).enumerate() {
                    let link = item.url.clone();
                    let title = SharedString::from(item.title.clone());
                    pills = pills.child(
                        Button::new(("tag", first_item + offset))
                            .small()
                            .outline()
                            .rounded_full()
                            .label(item.title.clone())
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if let Some(link) = &link {
                                    view.open_link(link, Some(title.clone()), cx);
                                }
                            })),
                    );
                }
                if section.collapsible && section.items.len() > 10 {
                    let label = if expanded {
                        t(Str::ShowLess).to_owned()
                    } else {
                        tf(Str::ShowMoreN, &[&(section.items.len() - 10)])
                    };
                    pills = pills.child(
                        Button::new(("tags-more", section_index))
                            .small()
                            .outline()
                            .rounded_full()
                            .label(label)
                            .on_click(cx.listener(move |view, _, _, cx| view.toggle_section(section_index, cx))),
                    );
                }
                return block.child(pills).into_any_element();
            }
            "carousel" => {
                let mut row = h_flex().gap_1().px_3();
                for (offset, item) in section.items.iter().enumerate() {
                    row = row.child(
                        self.clickable(("carousel", first_item + offset), item, None, cx)
                            .w(px(80.))
                            .flex_shrink_0()
                            .p_1()
                            .child(
                                v_flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .size(px(64.))
                                            .rounded_full()
                                            .overflow_hidden()
                                            .bg(theme.secondary)
                                            .when_some(thumb_for(&item.thumbs, 128), |frame, url| {
                                                frame.child(img(url).size_full().object_fit(ObjectFit::Cover))
                                            }),
                                    )
                                    .child(div().w_full().text_xs().text_center().truncate().child(item.title.clone())),
                            ),
                    );
                }
                return block
                    .child(div().id(("carousel-scroll", section_index)).overflow_x_scroll().child(row))
                    .into_any_element();
            }
            _ => {}
        }
        // A collapsed section keeps columns x rows, rows from the engine's ladder or 4/3/2.
        let ladder: Vec<usize> = if section.collapsed_rows.is_empty() {
            vec![4, 3, 2]
        } else {
            section.collapsed_rows.iter().map(|rows| (*rows).max(1) as usize).collect()
        };
        let rows = ladder.get(bands.columns - 1).or(ladder.last()).copied().unwrap_or(2);
        let limit = bands.columns * rows;
        let shown = if section.collapsible && !expanded { limit.min(section.items.len()) } else { section.items.len() };
        let items = &section.items[..shown];
        if bands.columns == 1 {
            for (offset, item) in items.iter().enumerate() {
                block = block.child(self.render_row(item, first_item + offset, bands, queue.clone(), cx));
            }
        } else {
            let inset = bands.inset();
            for (chunk_index, chunk) in items.chunks(bands.columns).enumerate() {
                let mut row = h_flex()
                    .w_full()
                    .items_start()
                    .px(px(bands.margin - inset))
                    .pb(px(ROW_GAP - 2. * inset))
                    .gap(px(bands.gutter - 2. * inset));
                for (offset, item) in chunk.iter().enumerate() {
                    let index = first_item + chunk_index * bands.columns + offset;
                    row = row.child(self.render_card(item, index, bands, queue.clone(), cx));
                }
                for _ in chunk.len()..bands.columns {
                    row = row.child(div().flex_1().min_w_0());
                }
                block = block.child(row);
            }
        }
        if section.collapsible && section.items.len() > limit {
            let label = if expanded {
                t(Str::ShowLess).to_owned()
            } else {
                tf(Str::ShowMoreN, &[&(section.items.len() - limit)])
            };
            block = block.child(
                div().px(px(bands.margin)).child(
                    Button::new(("section-more", section_index))
                        .outline()
                        .rounded_full()
                        .w_full()
                        .label(label)
                        .icon(Icon::new(if expanded { IconName::ChevronUp } else { IconName::ChevronDown }))
                        .on_click(cx.listener(move |view, _, _, cx| view.toggle_section(section_index, cx))),
                ),
            );
        }
        block.into_any_element()
    }
}

#[derive(Clone, Copy)]
enum MenuAction {
    PlaySingle,
    Feed(FeedAction),
    Share,
    RemoveFromTop,
}

fn share_link(item: &YtFeedItem) -> Option<String> {
    match item.kind.as_str() {
        "video" => Some(format!("https://youtu.be/{}", item.id)),
        "playlist" if !item.id.is_empty() => Some(format!("https://www.youtube.com/playlist?list={}", item.id)),
        "channel" => {
            item.channel_id.as_ref().or(Some(&item.id)).map(|id| format!("https://www.youtube.com/channel/{id}"))
        }
        _ => None,
    }
}

impl Render for FeedView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.phase {
            Phase::Loading => return centered(Spinner::new().large()).into_any_element(),
            Phase::Failed(error) => {
                let message = error_text(error);
                return centered(
                    v_flex()
                        .gap_3()
                        .items_center()
                        .max_w(px(420.))
                        .p_6()
                        .child(div().text_center().child(message))
                        .child(
                            Button::new("retry")
                                .label(t(Str::Retry))
                                .on_click(cx.listener(|view, _, _, cx| view.reload(cx))),
                        ),
                )
                .into_any_element();
            }
            Phase::Ready => {}
        }
        let bands = Bands::for_width(feed_width(window));
        let theme = cx.theme().clone();
        let mut column = v_flex().w_full().pb_4();
        let tabs = self.page.page_tabs.clone();
        let selected_tab = tabs.iter().position(|tab| tab.selected).unwrap_or(0);
        let on_home_tab = tabs.get(selected_tab).is_none_or(|tab| tab.home);
        let category = self.header_kind() == HeaderKind::Category;
        if on_home_tab && let Some(header) = self.page.page_header.clone() {
            column = column.child(self.render_header(&header, cx));
        } else if category {
            column = column.child(self.render_category_bar(cx));
        }
        let tab_row = (!tabs.is_empty()).then(|| {
            let urls: Vec<Option<String>> = tabs.iter().map(|tab| tab.url.clone()).collect();
            let entity = cx.entity().downgrade();
            div().px_3().border_b_1().border_color(theme.border).child(
                gpui_kit::component::tab::TabBar::new("page-tabs")
                    .underline()
                    .selected_index(selected_tab)
                    .children(tabs.iter().map(|tab| gpui_kit::component::tab::Tab::new().label(tab.title.clone())))
                    .on_click(move |index, _, cx| {
                        if let Some(Some(url)) = urls.get(*index).cloned() {
                            entity.update(cx, |view, cx| view.open_tab(url, cx)).ok();
                        }
                    }),
            )
        });
        if !self.page.chips.is_empty() {
            let mut chips = h_flex().gap_2().px_4().py_2().when(self.page.chips_wrap, |row| row.flex_wrap());
            for (index, chip) in self.page.chips.clone().into_iter().enumerate() {
                let token = chip.token.clone();
                let mut button = Button::new(("chip", index)).label(chip.title.clone()).small().rounded_full();
                button = if chip.selected { button.primary() } else { button.outline() };
                chips = chips.child(button.selected(chip.selected).on_click(cx.listener(move |view, _, _, cx| {
                    if let Some(token) = token.clone() {
                        view.refilter(token, cx);
                    }
                })));
            }
            column = column.child(div().id("chips").overflow_x_scroll().child(chips));
        } else if self.page.page_header.is_none() && !category {
            column = column.child(div().h(px(6.)));
        }

        let item_total: usize = self.page.sections.iter().map(|section| section.items.len()).sum();
        if item_total == 0 {
            let title = self.page.empty_title.clone().unwrap_or_else(|| t(Str::FeedEmpty).to_owned());
            column = column.child(
                v_flex()
                    .py_16()
                    .px_6()
                    .gap_2()
                    .items_center()
                    .child(div().text_lg().child(title))
                    .when_some(self.page.empty_message.clone(), |empty, message| {
                        empty.child(div().text_sm().text_color(theme.muted_foreground).child(message))
                    }),
            );
        }
        let mut first_item = 0;
        for (section_index, section) in self.page.sections.clone().iter().enumerate() {
            column = column.child(self.render_section(section, section_index, first_item, bands, cx));
            first_item += section.items.len();
        }
        if self.loading_more {
            column = column.child(h_flex().justify_center().p_3().child(Spinner::new()));
        } else if let Some(error) = &self.more_failed {
            column = column.child(
                v_flex().items_center().gap_2().py_4().child(div().text_sm().child(error_text(error))).child(
                    Button::new("retry-more").small().label(t(Str::Retry)).on_click(cx.listener(|view, _, _, cx| {
                        view.more_failed = None;
                        view.load_more(cx);
                    })),
                ),
            );
        } else if self.page.continuation.is_some() {
            column = column.child(
                div().px(px(bands.margin)).pt_2().child(
                    Button::new("show-more")
                        .outline()
                        .rounded_full()
                        .w_full()
                        .label(t(Str::ShowMore))
                        .on_click(cx.listener(|view, _, _, cx| view.load_more(cx))),
                ),
            );
        }
        v_flex()
            .size_full()
            .children(tab_row)
            .child(
                div().id("feed-scroll").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).child(column),
            )
            .into_any_element()
    }
}
