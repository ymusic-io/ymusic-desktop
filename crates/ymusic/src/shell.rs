//! The app shell: the section sidebar, a header with back / forward and search, and one
//! navigation stack per section over its root surface.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::sidebar::{Sidebar, SidebarHeader, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::*;
use ym_i18n::{Str, t};

use crate::feed::{FeedSource, FeedView, Navigate, Play};
use crate::graph::{AppGraph, on_core};
use crate::library::{LibraryView, PlayLocal};
use crate::media_session::MediaSession;
use crate::mini_player::{MiniPlayer, ToggleNowPlaying};
use crate::now_playing::NowPlaying;
use crate::playback::Playback;

const SUGGEST_DEBOUNCE: Duration = Duration::from_millis(150);

/// Shared with the JVM app: the section and Discover tab a restart resumes, by their stable ids.
const LAST_SECTION_KEY: &str = "app.lastSection";
const LAST_DISCOVER_TAB_KEY: &str = "app.lastDiscoverTab";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Section {
    Discover,
    YtMusic,
    Library,
    PlayAgain,
    Downloads,
    Settings,
    Donate,
    Telegram,
    Feedback,
}

impl Section {
    const GROUPS: [&'static [Section]; 3] = [
        &[Section::Discover, Section::YtMusic, Section::Library],
        &[Section::PlayAgain, Section::Downloads, Section::Settings],
        &[Section::Donate, Section::Telegram, Section::Feedback],
    ];

    const ALL: [Section; 9] = [
        Section::Discover,
        Section::YtMusic,
        Section::Library,
        Section::PlayAgain,
        Section::Downloads,
        Section::Settings,
        Section::Donate,
        Section::Telegram,
        Section::Feedback,
    ];

    /// The persisted id (the Compose `AppSection.id`): stable, never reused.
    fn id(self) -> i32 {
        Self::ALL.iter().position(|section| *section == self).unwrap_or(0) as i32 + 1
    }

    fn from_id(id: i32) -> Option<Self> {
        Self::ALL.get(usize::try_from(id - 1).ok()?).copied()
    }

    fn label(self) -> &'static str {
        t(match self {
            Section::Discover => Str::SectionDiscover,
            Section::YtMusic => Str::SectionYtMusic,
            Section::Library => Str::SectionLibrary,
            Section::PlayAgain => Str::SectionPlayAgain,
            Section::Downloads => Str::SectionDownloads,
            Section::Settings => Str::SectionSettings,
            Section::Donate => Str::SectionDonate,
            Section::Telegram => Str::SectionTelegram,
            Section::Feedback => Str::SectionFeedback,
        })
    }

    fn icon(self) -> Lucide {
        match self {
            Section::Discover => Lucide::MonitorPlay,
            Section::YtMusic => Lucide::CirclePlay,
            Section::Library => Lucide::LibraryBig,
            Section::PlayAgain => Lucide::RotateCcwClock,
            Section::Downloads => Lucide::Download,
            Section::Settings => Lucide::Settings,
            Section::Donate => Lucide::Heart,
            Section::Telegram => Lucide::Send,
            Section::Feedback => Lucide::MessageCircle,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DiscoverTab {
    Home,
    Trending,
    Recommended,
}

impl DiscoverTab {
    const ALL: [DiscoverTab; 3] = [DiscoverTab::Home, DiscoverTab::Trending, DiscoverTab::Recommended];

    /// The persisted id (the Compose `DiscoverTab.id`).
    fn id(self) -> i32 {
        match self {
            DiscoverTab::Home => 1,
            DiscoverTab::Trending => 2,
            DiscoverTab::Recommended => 3,
        }
    }

    fn from_id(id: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.id() == id)
    }

    fn label(self) -> &'static str {
        t(match self {
            DiscoverTab::Home => Str::TabHome,
            DiscoverTab::Trending => Str::TabMusic,
            DiscoverTab::Recommended => Str::TabRecommended,
        })
    }

    fn source(self) -> FeedSource {
        match self {
            DiscoverTab::Home => FeedSource::Home,
            DiscoverTab::Trending => FeedSource::Trending,
            DiscoverTab::Recommended => FeedSource::Recommended,
        }
    }
}

/// Surfaces pushed over a section's root; `forward` holds what back popped.
#[derive(Default)]
struct Stack {
    pushed: Vec<Entity<FeedView>>,
    forward: Vec<Entity<FeedView>>,
}

pub struct Shell {
    section: Section,
    discover_tab: DiscoverTab,
    roots: HashMap<FeedSource, Entity<FeedView>>,
    stacks: HashMap<Section, Stack>,
    search: Entity<InputState>,
    suggestions: Vec<String>,
    suggest_generation: u64,
    suggestions_open: bool,
    playback: Entity<Playback>,
    mini_player: Entity<MiniPlayer>,
    now_playing: Entity<NowPlaying>,
    now_playing_open: bool,
    library: Option<Entity<LibraryView>>,
    _media_session: Entity<MediaSession>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t(Str::SearchYoutube)));
        let subscription =
            cx.subscribe_in(&search, window, |shell, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => shell.query_changed(input.read(cx).value().to_string(), cx),
                InputEvent::PressEnter { .. } => {
                    let query = input.read(cx).value().to_string();
                    shell.submit_search(query, window, cx);
                }
                InputEvent::Focus => {
                    shell.suggestions_open = true;
                    cx.notify();
                }
                InputEvent::Blur => {}
            });
        let playback = cx.new(|cx| Playback::new(cx));
        let mini_player = cx.new(|cx| MiniPlayer::new(playback.clone(), window, cx));
        let now_playing = cx.new(|cx| NowPlaying::new(playback.clone(), cx));
        let media_session = cx.new(|cx| MediaSession::new(playback.clone(), cx));
        let toggle = cx.subscribe(&mini_player, |shell, _, _: &ToggleNowPlaying, cx| {
            shell.now_playing_open = !shell.now_playing_open;
            cx.notify();
        });
        let settings = cx.global::<AppGraph>().settings.clone();
        let section = settings.int(LAST_SECTION_KEY, 1).ok().and_then(Section::from_id).unwrap_or(Section::Discover);
        let discover_tab =
            settings.int(LAST_DISCOVER_TAB_KEY, 1).ok().and_then(DiscoverTab::from_id).unwrap_or(DiscoverTab::Home);
        Self {
            playback,
            mini_player,
            now_playing,
            now_playing_open: false,
            library: None,
            _media_session: media_session,
            section,
            discover_tab,
            roots: HashMap::new(),
            stacks: HashMap::new(),
            search,
            suggestions: Vec::new(),
            suggest_generation: 0,
            suggestions_open: false,
            _subscriptions: vec![subscription, toggle],
        }
    }

    /// Debug builds only: `YMUSIC_DEV_ROUTE` opens a surface at start (`trending`, `music`,
    /// `search:<query>`, `play:<youtube url>`, `nowplaying:<url>`, `library:<dir>`, `localplay:<dir>`, `signin-paste:<file>`, `devicelink`, `signout`,
    /// `download:<FORMAT>:<videoId>` into `YMUSIC_DEV_DOWNLOAD_DIR` or the chosen folder), for
    /// screenshots and checks without driving the UI.
    pub fn apply_dev_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cfg!(debug_assertions) {
            return;
        }
        if let Ok(spec) = std::env::var("YMUSIC_DEV_PLAYER") {
            self.dev_player(&spec, cx);
        }
        let Ok(route) = std::env::var("YMUSIC_DEV_ROUTE") else { return };
        match route.split_once(':') {
            Some(("search", query)) => self.submit_search(query.to_owned(), window, cx),
            Some(("play", url)) => self.play_link(url.to_owned(), cx),
            Some((route @ ("library" | "localplay"), path)) => {
                self.section = Section::Library;
                let view = self.library_view(cx);
                let play_first = route == "localplay";
                view.update(cx, |library, cx| library.scan_path(path.into(), play_first, cx));
            }
            Some(("signin-paste", path)) => self.dev_sign_in_paste(path.into(), cx),
            Some(("download", spec)) => self.dev_download(spec, cx),
            _ if route == "devicelink" => self.dev_device_link(cx),
            _ if route == "signout" => self.dev_sign_out(cx),
            Some(("nowplaying", url)) => {
                self.now_playing_open = true;
                self.play_link(url.to_owned(), cx);
            }
            _ if route == "trending" => self.discover_tab = DiscoverTab::Trending,
            _ if route == "music" => self.section = Section::YtMusic,
            _ => {}
        }
    }

    fn library_view(&mut self, cx: &mut Context<Self>) -> Entity<LibraryView> {
        if let Some(view) = &self.library {
            return view.clone();
        }
        let store = cx.global::<AppGraph>().media.clone();
        let view = cx.new(|cx| LibraryView::new(store, cx));
        let play = cx.subscribe(&view, |shell, _, event: &PlayLocal, cx| {
            let (items, index) = (event.items.clone(), event.index);
            shell.playback.update(cx, |playback, cx| playback.play_queue(items, index, cx));
        });
        self._subscriptions.push(play);
        self.library = Some(view.clone());
        view
    }

    /// Debug route: signs in with a cookie paste read from a file; the outcome goes to the log.
    fn dev_sign_in_paste(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        let graph = cx.global::<AppGraph>();
        let (jar, engines, log) = (graph.cookies.clone(), graph.engines.clone(), graph.log.fork("signin"));
        graph.tokio.spawn(async move {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let cookies = ym_signin::parse_cookie_paste(&text);
            log.info(&format!("paste parsed {} cookies", cookies.len()));
            match crate::signin::sign_in_with_cookies(cookies, jar, engines, &log).await {
                Ok(account) => log.info(&format!("signed in as {}", account.name.unwrap_or_default())),
                Err(error) => log.warn("sign-in failed", Some(&error)),
            }
        });
    }

    /// Debug: `speed=2,pitch=1,preset=1,loop=One,shuffle=1,sleep=<seconds>` applied at start.
    fn dev_player(&mut self, spec: &str, cx: &mut Context<Self>) {
        let mut effects = self.playback.read(cx).effects;
        for (key, value) in spec.split(',').filter_map(|pair| pair.split_once('=')) {
            let number = value.parse::<f32>().ok();
            match key {
                "speed" => effects.speed = number.unwrap_or(effects.speed),
                "pitch" => effects.pitch = number.unwrap_or(effects.pitch),
                "preset" => {
                    if let Some((_, bands)) =
                        value.parse::<usize>().ok().and_then(|index| crate::audio_effects::EQ_PRESETS.get(index))
                    {
                        effects.tone.eq_db = *bands;
                    }
                }
                "loop" => {
                    let mode = crate::queue_nav::LoopMode::from_name(value);
                    self.playback.update(cx, |playback, cx| playback.set_loop_mode(mode, cx));
                }
                "shuffle" => self.playback.update(cx, |playback, cx| playback.set_shuffle(value == "1", cx)),
                "sleep" => {
                    let duration = value.parse().ok().map(std::time::Duration::from_secs);
                    self.playback.update(cx, |playback, cx| playback.set_sleep_timer(duration, cx));
                }
                _ => {}
            }
        }
        self.playback.update(cx, |playback, cx| playback.set_effects(effects, cx));
    }

    /// Debug route: probes and downloads one video, logging every queue change.
    fn dev_download(&mut self, spec: &str, cx: &mut Context<Self>) {
        let graph = cx.global::<AppGraph>();
        let (downloads, settings, log) =
            (graph.downloads.clone(), graph.settings.clone(), graph.log.fork("dev-download"));
        let (format, video_id) = spec.split_once(':').unwrap_or(("M4A", spec));
        let Some(format) = ym_download::DownloadFormat::from_name(format) else {
            log.warn(&format!("unknown download format {format}"), None);
            return;
        };
        let video_id = video_id.to_owned();
        graph.tokio.spawn(async move {
            if let Some(dir) = std::env::var_os("YMUSIC_DEV_DOWNLOAD_DIR")
                && let Ok(uri) = url::Url::from_directory_path(std::path::PathBuf::from(dir))
                && let Err(error) = settings.put_raw(crate::downloads::DOWNLOAD_LOCATION_KEY, uri.to_string())
            {
                log.warn(&format!("the dev download folder could not be saved: {error}"), None);
            }
            let mut changes = downloads.subscribe();
            match downloads.probe(&video_id).await {
                Ok(options) => log.info(&format!(
                    "options: {}ms, {} video heights, audio {:?}, {} captions",
                    options.duration_ms,
                    options.video.len(),
                    options.audio.iter().map(|option| option.format).collect::<Vec<_>>(),
                    options.captions.len()
                )),
                Err(error) => log.warn("download probe failed", Some(&error)),
            }
            let id = downloads.enqueue(ym_download::DownloadRequest {
                video_id: video_id.clone(),
                title: video_id.clone(),
                artist: None,
                thumbnail_url: None,
                format,
                requested_height: format.is_video().then_some(360),
                file_name: None,
                audio_track_id: None,
                captions: None,
                audio_language: None,
                caption_language: None,
                audio_quality: ym_download::AudioQuality::Best,
            });
            while let Ok(change) = changes.recv().await {
                match change {
                    ym_download::DownloadChange::ProgressChanged { .. } => {}
                    ym_download::DownloadChange::Completed { id: done, file_uri, bytes } if done == id => {
                        log.info(&format!("download completed: {file_uri} ({bytes} bytes)"));
                        return;
                    }
                    ym_download::DownloadChange::StateChanged { id: changed, state, error } if changed == id => {
                        log.info(&format!("download {state:?} {error:?}"));
                        if state == ym_download::DownloadState::Failed {
                            return;
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    /// Debug route: opens a device link and logs the QR payload and code, then waits for the phone.
    fn dev_device_link(&mut self, cx: &mut Context<Self>) {
        let graph = cx.global::<AppGraph>();
        let (service, jar, engines, log) =
            (graph.remote.clone(), graph.cookies.clone(), graph.engines.clone(), graph.log.fork("devicelink"));
        graph.tokio.spawn(async move {
            match crate::signin::start_device_link(&service).await {
                Ok(offer) => {
                    log.info(&format!("scan {} and confirm {}", offer.qr_payload(), offer.confirmation_code()));
                    match crate::signin::await_device_link(service, offer, jar, engines, &log).await {
                        Ok(account) => log.info(&format!("signed in as {}", account.name.unwrap_or_default())),
                        Err(error) => log.warn("device link failed", Some(&error)),
                    }
                }
                Err(error) => log.warn("device link unavailable", Some(&error)),
            }
        });
    }

    fn dev_sign_out(&mut self, cx: &mut Context<Self>) {
        let graph = cx.global::<AppGraph>();
        let (jar, engines, log) = (graph.cookies.clone(), graph.engines.clone(), graph.log.fork("signout"));
        graph.tokio.spawn(async move {
            match crate::signin::sign_out(jar, engines, &log).await {
                Ok(()) => log.info("signed out"),
                Err(error) => log.warn("sign-out failed", Some(&error)),
            }
        });
    }

    /// Resolves a shared YouTube link and plays it when it names a video.
    fn play_link(&mut self, url: String, cx: &mut Context<Self>) {
        let graph = cx.global::<AppGraph>();
        let (engine, log) = (graph.engine(), graph.log.fork("link"));
        let resolve = on_core(cx, async move { engine.resolve_link(&url, &log).await });
        cx.spawn(async move |this, cx| {
            if let Some(Ok(Some(ym_engine::yt::YtLinkTarget::Video { item, .. }))) = resolve.await {
                this.update(cx, |shell, cx| {
                    shell.playback.update(cx, |playback, cx| playback.play_feed(*item, None, cx))
                })
                .ok();
            }
        })
        .detach();
    }

    fn feed_view(
        &mut self,
        source: FeedSource,
        title: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> Entity<FeedView> {
        let view = cx.new(|cx| FeedView::new(source, title, cx));
        let navigate = cx.subscribe(&view, |shell, _, event: &Navigate, cx| {
            shell.push(event.source.clone(), event.title.clone(), cx);
        });
        let play = cx.subscribe(&view, |shell, _, event: &Play, cx| {
            let (item, queue) = (event.item.clone(), event.queue.clone());
            shell.playback.update(cx, |playback, cx| playback.play_feed(item, queue, cx));
        });
        self._subscriptions.extend([navigate, play]);
        view
    }

    fn root(&mut self, source: FeedSource, cx: &mut Context<Self>) -> Entity<FeedView> {
        if let Some(view) = self.roots.get(&source) {
            return view.clone();
        }
        let view = self.feed_view(source.clone(), None, cx);
        self.roots.insert(source, view.clone());
        view
    }

    fn push(&mut self, source: FeedSource, title: Option<SharedString>, cx: &mut Context<Self>) {
        self.now_playing_open = false;
        let view = self.feed_view(source, title, cx);
        let stack = self.stacks.entry(self.section).or_default();
        stack.pushed.push(view);
        stack.forward.clear();
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        let stack = self.stacks.entry(self.section).or_default();
        if let Some(view) = stack.pushed.pop() {
            stack.forward.push(view);
            cx.notify();
        }
    }

    fn forward(&mut self, cx: &mut Context<Self>) {
        let stack = self.stacks.entry(self.section).or_default();
        if let Some(view) = stack.forward.pop() {
            stack.pushed.push(view);
            cx.notify();
        }
    }

    fn select_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.now_playing_open = false;
        if self.section == section {
            // Re-selecting the current section returns to its root.
            let stack = self.stacks.entry(section).or_default();
            stack.forward.clear();
            stack.pushed.clear();
        }
        self.section = section;
        persist_setting(cx, LAST_SECTION_KEY, section.id().to_string());
        cx.notify();
    }

    fn query_changed(&mut self, query: String, cx: &mut Context<Self>) {
        self.suggest_generation += 1;
        let generation = self.suggest_generation;
        self.suggestions_open = true;
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
            if this.read_with(cx, |shell, _| shell.suggest_generation != generation).unwrap_or(true) {
                return;
            }
            let fetch = cx.update(|cx| on_core(cx, async move { engine.search_suggestions(&query, &log).await }));
            let result = fetch.await;
            this.update(cx, |shell, cx| {
                if shell.suggest_generation == generation
                    && let Some(Ok(suggestions)) = result
                {
                    shell.suggestions = suggestions;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn submit_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.suggest_generation += 1;
        self.suggestions_open = false;
        self.search.update(cx, |input, cx| input.set_value(query.clone(), window, cx));
        self.push(FeedSource::Search(query.clone()), Some(query.into()), cx);
    }

    fn content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.now_playing_open && self.playback.read(cx).current().is_some() {
            return self.now_playing.clone().into_any_element();
        }
        self.now_playing.read(cx).hide_video();
        if let Some(top) = self.stacks.get(&self.section).and_then(|stack| stack.pushed.last()) {
            return top.clone().into_any_element();
        }
        match self.section {
            Section::Discover => {
                let view = self.root(self.discover_tab.source(), cx);
                let selected = DiscoverTab::ALL.iter().position(|tab| *tab == self.discover_tab).unwrap_or(0);
                let entity = cx.entity().downgrade();
                v_flex()
                    .size_full()
                    .child(
                        div().px_4().pt_2().child(
                            TabBar::new("discover-tabs")
                                .underline()
                                .selected_index(selected)
                                .children(DiscoverTab::ALL.iter().map(|tab| Tab::new().label(tab.label())))
                                .on_click(move |index, _, cx| {
                                    if let Some(tab) = DiscoverTab::ALL.get(*index).copied() {
                                        entity
                                            .update(cx, |shell, cx| {
                                                shell.discover_tab = tab;
                                                persist_setting(cx, LAST_DISCOVER_TAB_KEY, tab.id().to_string());
                                                cx.notify();
                                            })
                                            .ok();
                                    }
                                }),
                        ),
                    )
                    .child(div().flex_1().min_h_0().child(view))
                    .into_any_element()
            }
            Section::YtMusic => self.root(FeedSource::MusicHome, cx).into_any_element(),
            Section::Library => self.library_view(cx).into_any_element(),
            other => {
                let theme = cx.theme();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(Icon::new(other.icon()).large().text_color(theme.muted_foreground))
                    .child(div().text_lg().child(other.label()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(t(Str::FeedEmpty)))
                    .into_any_element()
            }
        }
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut sidebar = Sidebar::new("sections")
            .w(px(232.))
            .header(SidebarHeader::new().child(div().text_lg().font_semibold().child(t(Str::AppName))));
        for group in Section::GROUPS {
            let mut menu = SidebarMenu::new();
            for section in group.iter().copied() {
                menu = menu.child(
                    SidebarMenuItem::new(section.label())
                        .icon(section.icon())
                        .active(self.section == section)
                        .on_click(cx.listener(move |shell, _, _, cx| shell.select_section(section, cx))),
                );
            }
            sidebar = sidebar.child(menu);
        }
        sidebar
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let stack = self.stacks.get(&self.section);
        let can_back = stack.is_some_and(|stack| !stack.pushed.is_empty());
        let can_forward = stack.is_some_and(|stack| !stack.forward.is_empty());
        let title = stack.and_then(|stack| stack.pushed.last()).and_then(|view| view.read(cx).title());
        let theme = cx.theme();
        h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("back")
                    .ghost()
                    .icon(IconName::ChevronLeft)
                    .tooltip(t(Str::CdBack))
                    .disabled(!can_back)
                    .on_click(cx.listener(|shell, _, _, cx| shell.back(cx))),
            )
            .child(
                Button::new("forward")
                    .ghost()
                    .icon(IconName::ChevronRight)
                    .disabled(!can_forward)
                    .on_click(cx.listener(|shell, _, _, cx| shell.forward(cx))),
            )
            .child(div().flex_1().min_w_0().truncate().font_semibold().children(title))
            .child(
                div()
                    .w(px(360.))
                    .child(Input::new(&self.search).prefix(Icon::new(IconName::Search).small()).cleanable(true)),
            )
    }

    fn render_suggestions(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.suggestions_open || self.suggestions.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let mut list = v_flex()
            .absolute()
            .top(px(48.))
            .right(px(12.))
            .w(px(360.))
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .shadow_lg();
        for (index, suggestion) in self.suggestions.iter().take(10).enumerate() {
            let query = suggestion.clone();
            list = list.child(
                h_flex()
                    .id(("suggestion", index))
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary))
                    .child(Icon::new(IconName::Search).small().text_color(theme.muted_foreground))
                    .child(div().text_sm().truncate().child(suggestion.clone()))
                    .on_click(cx.listener(move |shell, _, window, cx| shell.submit_search(query.clone(), window, cx))),
            );
        }
        Some(deferred(list.on_mouse_down_out(cx.listener(|shell, _, _, cx| {
            shell.suggestions_open = false;
            cx.notify();
        }))))
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = self.content(cx);
        let header = self.render_header(cx);
        let suggestions = self.render_suggestions(cx);
        let sidebar = self.render_sidebar(cx);
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(
                h_flex().flex_1().min_h_0().w_full().child(sidebar).child(
                    v_flex()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(header)
                        .child(div().flex_1().min_h_0().child(content))
                        .children(suggestions),
                ),
            )
            .child(self.mini_player.clone())
    }
}

/// Writes a setting off the UI thread; a failed write is logged, the UI state stands.
fn persist_setting(cx: &App, key: &'static str, value: String) {
    let graph = cx.global::<AppGraph>();
    let (settings, log) = (graph.settings.clone(), graph.log.clone());
    graph.tokio.spawn_blocking(move || {
        if let Err(error) = settings.put_raw(key, value) {
            log.warn(&format!("R90006 setting {key} not saved"), Some(error.as_ref()));
        }
    });
}

impl std::hash::Hash for FeedSource {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        format!("{self:?}").hash(state);
    }
}

impl Eq for FeedSource {}
