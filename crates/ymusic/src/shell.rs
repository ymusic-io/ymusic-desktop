//! The app shell, laid out like the JVM desktop app: a navigation rail (72 collapsed, 280
//! expanded) from 750 wide, a modal drawer below that; a top bar per section (title, Discover's
//! icon tabs, search); pushed layers (search, results, detail pages) that bring their own back bar;
//! and the mini player along the bottom.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, IconName, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t, tf};

use crate::a11y::{DragRow, FocusList, focusable};
use crate::download_dialog::{DownloadPicker, DownloadTarget};
use crate::downloads_view::DownloadsView;
use crate::feed::{FeedAction, FeedSource, FeedView, ItemAction, Navigate, Play};
use crate::feedback_view::FeedbackView;
use crate::graph::{AppGraph, on_core};
use crate::library::{LibraryView, PlayLocal, QueueLocal};
use crate::media_session::MediaSession;
use crate::mini_player::{MiniPlayer, ToggleNowPlaying};
use crate::now_playing::{NowPlaying, NowPlayingEvent};
use crate::play_again::{PlayAgainView, ResumeQueue};
use crate::play_history::HistoryRecorder;
use crate::playback::{Playback, QueueItem};
use crate::search_view::{SearchEvent, SearchView};
use crate::settings_view::SettingsView;
use crate::sponsor_block::SponsorBlock;
use crate::tab_row::tab_row;

const RAIL_COLLAPSED: f32 = 72.;
const DRAWER_ICON_START: f32 = 20.;
/// Centres the expanded rail's section icons on the collapsed rail's centre line.
const RAIL_ICON_START: f32 = RAIL_COLLAPSED / 2. - SECTION_ICON / 2.;
const SECTION_ICON: f32 = 16.;
const RAIL_EXPANDED: f32 = 280.;
const DRAWER_WIDTH: f32 = 300.;
const WEBSITE: &str = "https://ymusic.io";

/// Shared with the JVM app: the section and Discover tab a restart resumes, by their stable ids.
const LAST_SECTION_KEY: &str = "app.lastSection";
const LAST_DISCOVER_TAB_KEY: &str = "app.lastDiscoverTab";
const SEEK_STEP_MS: i64 = 10_000;
const VOLUME_STEP: f32 = 0.05;
const DOUBLE_ESCAPE: Duration = Duration::from_millis(500);

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
        &[Section::Telegram, Section::Feedback],
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

/// YT Music's text tabs.
#[derive(Clone, Copy, PartialEq)]
enum MusicTab {
    Home,
    Explore,
    NewReleases,
    Charts,
    Library,
}

impl MusicTab {
    const ALL: [MusicTab; 5] =
        [MusicTab::Home, MusicTab::Explore, MusicTab::NewReleases, MusicTab::Charts, MusicTab::Library];

    fn label(self) -> &'static str {
        t(match self {
            MusicTab::Home => Str::YtmusicTabHome,
            MusicTab::Explore => Str::YtmusicTabExplore,
            MusicTab::NewReleases => Str::YtmusicTabNewReleases,
            MusicTab::Charts => Str::YtmusicTabCharts,
            MusicTab::Library => Str::YtmusicTabLibrary,
        })
    }

    fn source(self) -> FeedSource {
        match self {
            MusicTab::Home => FeedSource::MusicHome,
            MusicTab::Explore => FeedSource::MusicExplore,
            MusicTab::NewReleases => FeedSource::MusicNewReleases,
            MusicTab::Charts => FeedSource::MusicCharts,
            MusicTab::Library => FeedSource::MusicLibrary,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DiscoverTab {
    Home,
    Trending,
    Recommended,
    TopTracks,
    Account,
}

impl DiscoverTab {
    const ALL: [DiscoverTab; 5] = [
        DiscoverTab::Home,
        DiscoverTab::Trending,
        DiscoverTab::Recommended,
        DiscoverTab::TopTracks,
        DiscoverTab::Account,
    ];

    /// The persisted id (the Compose `DiscoverTab.id`).
    fn id(self) -> i32 {
        match self {
            DiscoverTab::Home => 1,
            DiscoverTab::Trending => 2,
            DiscoverTab::Recommended => 3,
            DiscoverTab::TopTracks => 4,
            DiscoverTab::Account => 5,
        }
    }

    fn icon(self) -> Lucide {
        match self {
            DiscoverTab::Home => Lucide::House,
            DiscoverTab::Trending => Lucide::Music,
            DiscoverTab::Recommended => Lucide::Star,
            DiscoverTab::TopTracks => Lucide::TrendingUp,
            DiscoverTab::Account => Lucide::User,
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
            DiscoverTab::TopTracks => Str::TabTopTracks,
            DiscoverTab::Account => Str::TabAccountLibrary,
        })
    }

    fn source(self) -> FeedSource {
        match self {
            DiscoverTab::Home => FeedSource::Home,
            DiscoverTab::Trending => FeedSource::Trending,
            DiscoverTab::Recommended => FeedSource::Recommended,
            DiscoverTab::TopTracks => FeedSource::TopTracks,
            DiscoverTab::Account => FeedSource::Account,
        }
    }
}

/// A surface pushed over a section's root; each draws its own back bar.
enum Layer {
    Feed { view: Entity<FeedView>, title: SharedString },
    Search(Entity<SearchView>),
    Results { view: Entity<FeedView>, query: String },
}

#[derive(Default)]
struct Stack {
    pushed: Vec<Layer>,
}

pub struct Shell {
    section: Section,
    discover_tab: DiscoverTab,
    music_tab: MusicTab,
    roots: HashMap<FeedSource, Entity<FeedView>>,
    stacks: HashMap<Section, Stack>,
    /// The rail's state the user chose; cleared when a width breakpoint flips.
    rail_manual: Option<bool>,
    /// The (rail mode, expanded-by-width) the window last had.
    rail_auto: Option<(bool, bool)>,
    drawer_open: bool,
    /// Holds keyboard focus when nothing else does, so the shortcuts always reach the Shell.
    focus: FocusHandle,
    nav_focus: FocusList,
    tab_focus: FocusList,
    music_tab_focus: FocusList,
    music_tab_drag: DragRow,
    /// The last Esc that had nothing to close; a second one soon after goes back.
    idle_escape_at: Option<Instant>,
    playback: Entity<Playback>,
    mini_player: Entity<MiniPlayer>,
    now_playing: Entity<NowPlaying>,
    now_playing_open: bool,
    library: Option<Entity<LibraryView>>,
    play_again: Option<Entity<PlayAgainView>>,
    downloads: Option<Entity<DownloadsView>>,
    settings: Option<Entity<SettingsView>>,
    feedback: Option<Entity<FeedbackView>>,
    /// Debug: open the sign-in dialog on the next frame.
    pending_signin: bool,
    /// Debug: open the Sound panel on the next frame.
    pending_sound: bool,
    /// A download dialog to open on the next frame (opening needs the window).
    pending_download: Option<DownloadTarget>,
    _media_session: Entity<MediaSession>,
    _history: Entity<HistoryRecorder>,
    _sponsor_block: Entity<SponsorBlock>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let playback = cx.new(|cx| Playback::new(cx));
        let mini_player = cx.new(|cx| MiniPlayer::new(playback.clone(), window, cx));
        let sponsor_block = cx.new(|cx| SponsorBlock::new(playback.clone(), cx));
        let now_playing = cx.new(|cx| NowPlaying::new(playback.clone(), sponsor_block.clone(), window, cx));
        let media_session = cx.new(|cx| MediaSession::new(playback.clone(), cx));
        let history = cx.new(|cx| HistoryRecorder::new(playback.clone(), cx));
        let toggle = cx.subscribe(&mini_player, |shell, _, _: &ToggleNowPlaying, cx| {
            shell.now_playing_open = !shell.now_playing_open;
            cx.notify();
        });
        let now_playing_events = cx.subscribe(&now_playing, |shell, _, event: &NowPlayingEvent, cx| {
            match event {
                NowPlayingEvent::Close => shell.now_playing_open = false,
                NowPlayingEvent::Download(target) => shell.pending_download = Some(target.clone()),
                NowPlayingEvent::OpenSound => shell.pending_sound = true,
            }
            cx.notify();
        });
        crate::video_layer::forward_accessibility_focus(window);
        window.on_next_frame(|window, _| crate::video_layer::sync_accessibility_focus(window));
        let visibility = cx.observe_window_visibility(window, |shell, visibility, window, cx| {
            let hidden = !visibility.is_visible() && crate::video_layer::window_out_of_sight(window);
            shell.playback.update(cx, |playback, cx| playback.set_window_hidden(hidden, cx));
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
            play_again: None,
            downloads: None,
            settings: None,
            feedback: None,
            pending_signin: false,
            pending_sound: false,
            pending_download: None,
            _media_session: media_session,
            _history: history,
            _sponsor_block: sponsor_block,
            section,
            discover_tab,
            music_tab: MusicTab::Home,
            roots: HashMap::new(),
            stacks: HashMap::new(),
            rail_manual: None,
            rail_auto: None,
            drawer_open: false,
            focus: cx.focus_handle(),
            nav_focus: FocusList::new(None),
            tab_focus: FocusList::new(None),
            music_tab_focus: FocusList::new(None),
            music_tab_drag: DragRow::default(),
            idle_escape_at: None,
            _subscriptions: vec![toggle, now_playing_events, visibility],
        }
    }

    /// Debug builds only: `YMUSIC_DEV_ROUTE` opens a surface at start (`trending`, `music`,
    /// `search:<query>`, `open:<app link>`, `musictab:<n>`, `play:<youtube url>`, `nowplaying:<url>`, `library:<dir>`, `localplay:<dir>`, `signin-paste:<file>`, `devicelink`, `signout`,
    /// `download:<FORMAT>:<videoId>` into `YMUSIC_DEV_DOWNLOAD_DIR` or the chosen folder), for
    /// screenshots and checks without driving the UI.
    pub fn apply_dev_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cfg!(debug_assertions) {
            return;
        }
        if let Some(seconds) = std::env::var("YMUSIC_DEV_QUIT_AFTER").ok().and_then(|value| value.parse().ok()) {
            let executor = cx.background_executor().clone();
            cx.spawn(async move |_, cx| {
                executor.timer(std::time::Duration::from_secs(seconds)).await;
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
        // `YMUSIC_DEV_KEYS=secondary-n,escape` presses those keys 4 s apart, through the window's
        // own key dispatch, to check shortcuts without driving the real keyboard.
        if let Ok(keys) = std::env::var("YMUSIC_DEV_KEYS") {
            let keys: Vec<&'static str> =
                keys.split(',').map(|key| &*Box::leak(key.trim().to_owned().into_boxed_str())).collect();
            let executor = cx.background_executor().clone();
            cx.spawn_in(window, async move |_, cx| {
                for key in keys {
                    executor.timer(Duration::from_secs(4)).await;
                    cx.update(|window, cx| match Keystroke::parse(key) {
                        Ok(keystroke) => {
                            window.dispatch_keystroke(keystroke, cx);
                        }
                        Err(error) => eprintln!("YMUSIC_DEV_KEYS: {key}: {error}"),
                    })
                    .ok();
                }
            })
            .detach();
        }
        crate::a11y::dev_dump_tree(window, cx);
        // `YMUSIC_DEV_DRAG=x0,y,x1` presses at (x0, y) after 8 s and drags to x1, to check
        // drag-to-scroll without driving the real mouse.
        if let Some((from, y, to)) = std::env::var("YMUSIC_DEV_DRAG").ok().and_then(|spec| {
            let values: Vec<f32> = spec.split(',').filter_map(|value| value.trim().parse().ok()).collect();
            (values.len() == 3).then(|| (values[0], values[1], values[2]))
        }) {
            let executor = cx.background_executor().clone();
            cx.spawn_in(window, async move |_, cx| {
                executor.timer(Duration::from_secs(8)).await;
                let press = MouseDownEvent {
                    button: MouseButton::Left,
                    position: point(px(from), px(y)),
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                };
                cx.update(|window, cx| window.dispatch_event(PlatformInput::MouseDown(press), cx)).ok();
                for step in 1..=20 {
                    executor.timer(Duration::from_millis(16)).await;
                    let motion = MouseMoveEvent {
                        position: point(px(from + (to - from) * step as f32 / 20.), px(y)),
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    };
                    cx.update(|window, cx| window.dispatch_event(PlatformInput::MouseMove(motion), cx)).ok();
                }
                let release = MouseUpEvent {
                    button: MouseButton::Left,
                    position: point(px(to), px(y)),
                    modifiers: Modifiers::default(),
                    click_count: 1,
                };
                cx.update(|window, cx| window.dispatch_event(PlatformInput::MouseUp(release), cx)).ok();
            })
            .detach();
        }
        if let Ok(spec) = std::env::var("YMUSIC_DEV_PLAYER") {
            self.dev_player(&spec, cx);
        }
        let Ok(route) = std::env::var("YMUSIC_DEV_ROUTE") else { return };
        match route.split_once(':') {
            Some(("search", query)) => self.show_results(query.to_owned(), cx),
            Some(("musictab", index)) => {
                self.section = Section::YtMusic;
                self.music_tab = MusicTab::ALL[index.parse::<usize>().unwrap_or(0).min(4)];
            }
            Some(("open", link)) => {
                if let Some(source) = FeedSource::from_link(link) {
                    self.push(source, None, cx);
                }
            }
            Some(("play", url)) => self.play_link(url.to_owned(), cx),
            Some((route @ ("library" | "localplay"), path)) => {
                self.section = Section::Library;
                let view = self.library_view(cx);
                let play_first = route == "localplay";
                view.update(cx, |library, cx| library.scan_path(path.into(), play_first, cx));
            }
            Some(("librarytab", index)) => {
                self.section = Section::Library;
                let view = self.library_view(cx);
                let index = index.parse().unwrap_or(0);
                view.update(cx, |library, cx| library.select_tab_index(index, cx));
            }
            Some(("signin-paste", path)) => self.dev_sign_in_paste(path.into(), cx),
            Some(("downloaddialog", video_id)) => {
                self.pending_download = Some(DownloadTarget {
                    video_id: video_id.to_owned(),
                    title: video_id.to_owned(),
                    artist: None,
                    thumbnail_url: None,
                });
            }
            Some(("download", spec)) => {
                self.section = Section::Downloads;
                self.dev_download(spec, cx);
            }
            _ if route == "devicelink" => self.dev_device_link(cx),
            _ if route == "signout" => self.dev_sign_out(cx),
            Some(("nowplaying", url)) => {
                self.now_playing_open = true;
                self.play_link(url.to_owned(), cx);
            }
            _ if route == "trending" => self.discover_tab = DiscoverTab::Trending,
            Some(("discovertab", index)) => {
                self.section = Section::Discover;
                self.discover_tab = DiscoverTab::ALL[index.parse::<usize>().unwrap_or(0).min(4)];
            }
            _ if route == "searchinput" => self.open_search(String::new(), window, cx),
            _ if route == "drawer" => self.drawer_open = true,
            _ if route == "music" => self.section = Section::YtMusic,
            _ if route == "playagain" => self.section = Section::PlayAgain,
            _ if route == "downloads" => self.section = Section::Downloads,
            _ if route == "settings" => self.section = Section::Settings,
            _ if route == "feedback" => self.section = Section::Feedback,
            _ if route == "sound" => self.pending_sound = true,
            _ if route == "signin" => {
                self.section = Section::Settings;
                self.pending_signin = true;
            }
            Some(("playagain", "resume")) => {
                self.section = Section::PlayAgain;
                let view = self.play_again_view(cx);
                view.update(cx, |view, cx| view.resume_newest(cx));
            }
            _ => {}
        }
    }

    fn downloads_view(&mut self, cx: &mut Context<Self>) -> Entity<DownloadsView> {
        if let Some(view) = &self.downloads {
            return view.clone();
        }
        let service = cx.global::<AppGraph>().downloads.clone();
        let view = cx.new(|cx| DownloadsView::new(service, cx));
        let play = cx.subscribe(&view, |shell, _, event: &PlayLocal, cx| {
            let (items, index) = (event.items.clone(), event.index);
            shell.playback.update(cx, |playback, cx| playback.play_queue(items, index, cx));
        });
        self._subscriptions.push(play);
        self.downloads = Some(view.clone());
        view
    }

    fn play_again_view(&mut self, cx: &mut Context<Self>) -> Entity<PlayAgainView> {
        if let Some(view) = &self.play_again {
            return view.clone();
        }
        let history = cx.global::<AppGraph>().history.clone();
        let view = cx.new(|cx| PlayAgainView::new(history, cx));
        let resume = cx.subscribe(&view, |shell, _, event: &ResumeQueue, cx| {
            let (queue_id, items, index, start_ms) =
                (event.queue_id.clone(), event.items.clone(), event.index, event.start_ms);
            shell.playback.update(cx, |playback, cx| playback.resume_queue(queue_id, items, index, start_ms, cx));
        });
        self._subscriptions.push(resume);
        self.play_again = Some(view.clone());
        view
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
        let queue =
            cx.subscribe(&view, |shell, _, event: &QueueLocal, cx| {
                let (item, next) = (event.item.clone(), event.next);
                shell.playback.update(cx, |playback, cx| {
                    if next { playback.queue_next(item, cx) } else { playback.queue_last(item, cx) }
                });
            });
        self._subscriptions.extend([play, queue]);
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

    /// Debug: `speed=2,pitch=1,preset=1,loop=One,shuffle=1,sleep=<seconds>,video=0,boot=1` applied at start.
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
                "video" => self.playback.update(cx, |playback, cx| playback.set_video(value != "0", cx)),
                "sound" => {
                    let seconds = value.parse().unwrap_or(0);
                    let executor = cx.background_executor().clone();
                    cx.spawn(async move |shell, cx| {
                        executor.timer(std::time::Duration::from_secs(seconds)).await;
                        shell
                            .update(cx, |shell, cx| {
                                shell.pending_sound = true;
                                cx.notify();
                            })
                            .ok();
                    })
                    .detach();
                }
                "boot" => {
                    let graph = cx.global::<AppGraph>();
                    let (sessions, log) = (graph.youtube.clone(), graph.log.fork("dev-player"));
                    graph.tokio.spawn(async move {
                        match sessions.get().await {
                            Ok(_) => log.info("youtube session booted"),
                            Err(error) => log.warn("youtube session boot failed", Some(&error)),
                        }
                    });
                }
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
            let (item, queue, shuffle) = (event.item.clone(), event.queue.clone(), event.shuffle);
            shell.playback.update(cx, |playback, cx| {
                playback.play_feed(item, queue, cx);
                if shuffle {
                    playback.set_shuffle(true, cx);
                }
            });
        });
        let action = cx.subscribe(&view, |shell, _, event: &ItemAction, cx| {
            let item = event.item.clone();
            match event.action {
                FeedAction::QueueNext => {
                    shell.playback.update(cx, |playback, cx| playback.queue_next(QueueItem::from_feed(&item), cx))
                }
                FeedAction::QueueLast => {
                    shell.playback.update(cx, |playback, cx| playback.queue_last(QueueItem::from_feed(&item), cx))
                }
                FeedAction::Radio => shell.playback.update(cx, |playback, cx| playback.play_feed(item, None, cx)),
                FeedAction::Download => {
                    shell.pending_download = Some(DownloadTarget {
                        video_id: item.id.clone(),
                        title: item.title.clone(),
                        artist: item.channel_title.clone().or_else(|| item.subtitle_line1.clone()),
                        thumbnail_url: item.thumbs.last().map(|thumb| {
                            if thumb.url.starts_with("//") { format!("https:{}", thumb.url) } else { thumb.url.clone() }
                        }),
                    });
                    cx.notify();
                }
            }
        });
        self._subscriptions.extend([navigate, play, action]);
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

    fn push_layer(&mut self, layer: Layer, cx: &mut Context<Self>) {
        self.now_playing_open = false;
        self.stacks.entry(self.section).or_default().pushed.push(layer);
        cx.notify();
    }

    fn push(&mut self, source: FeedSource, title: Option<SharedString>, cx: &mut Context<Self>) {
        let title = title.unwrap_or_else(|| match source {
            FeedSource::Playlist(_) | FeedSource::MusicBrowse(_) => t(Str::A11yKindPlaylist).into(),
            _ => SharedString::default(),
        });
        let view = self.feed_view(source, None, cx);
        self.push_layer(Layer::Feed { view, title }, cx);
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.stacks.entry(self.section).or_default().pushed.pop().is_some() {
            cx.notify();
        }
    }

    fn open_search(&mut self, query: String, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| SearchView::new(&query, window, cx));
        let events = cx.subscribe(&view, |shell, _, event: &SearchEvent, cx| match event {
            SearchEvent::Submit(query) => shell.show_results(query.clone(), cx),
            SearchEvent::Back => shell.back(cx),
        });
        self._subscriptions.push(events);
        self.push_layer(Layer::Search(view), cx);
    }

    /// Results replace the search input they came from.
    fn show_results(&mut self, query: String, cx: &mut Context<Self>) {
        let stack = self.stacks.entry(self.section).or_default();
        if matches!(stack.pushed.last(), Some(Layer::Search(_))) {
            stack.pushed.pop();
        }
        let view = self.feed_view(FeedSource::Search(query.clone()), None, cx);
        self.push_layer(Layer::Results { view, query }, cx);
    }

    fn select_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if section == Section::Telegram {
            let url =
                crate::feedback_view::link(cx.global::<AppGraph>(), "telegramUrl", crate::feedback_view::TELEGRAM_URL);
            cx.open_url(&url);
            return;
        }
        self.now_playing_open = false;
        self.drawer_open = false;
        if self.section == section {
            // Re-selecting the current section returns to its root.
            self.stacks.entry(section).or_default().pushed.clear();
        }
        self.section = section;
        if section == Section::PlayAgain
            && let Some(view) = &self.play_again
        {
            view.update(cx, |view, cx| view.refresh(cx));
        }
        persist_setting(cx, LAST_SECTION_KEY, section.id().to_string());
        cx.notify();
    }

    /// The section's root surface (below its top bar).
    fn root_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match self.section {
            Section::Discover => self.root(self.discover_tab.source(), cx).into_any_element(),
            Section::YtMusic => self.root(self.music_tab.source(), cx).into_any_element(),
            Section::Library => self.library_view(cx).into_any_element(),
            Section::PlayAgain => self.play_again_view(cx).into_any_element(),
            Section::Downloads => self.downloads_view(cx).into_any_element(),
            Section::Feedback => {
                self.feedback.get_or_insert_with(|| cx.new(|_| FeedbackView::new())).clone().into_any_element()
            }
            Section::Settings => {
                let playback = self.playback.clone();
                self.settings
                    .get_or_insert_with(|| cx.new(|cx| SettingsView::new(playback, cx)))
                    .clone()
                    .into_any_element()
            }
            Section::Donate | Section::Telegram => div().into_any_element(),
        }
    }

    /// The top layer with its own bar, or the section's bar over its root.
    fn render_main(&mut self, rail_mode: bool, wide: bool, cx: &mut Context<Self>) -> AnyElement {
        let top = self.stacks.get(&self.section).and_then(|stack| stack.pushed.last());
        match top {
            Some(Layer::Search(view)) => return view.clone().into_any_element(),
            Some(Layer::Feed { view, title }) => {
                let view = view.clone();
                // A channel's name arrives with its page header.
                let title = if view.read(cx).is_playlist_page() {
                    t(Str::DetailPlaylist).into()
                } else if title.is_empty() {
                    view.read(cx).title().unwrap_or_default()
                } else {
                    title.clone()
                };
                return v_flex()
                    .size_full()
                    .child(self.detail_bar(title, None, cx))
                    .child(div().flex_1().min_h_0().child(view))
                    .into_any_element();
            }
            Some(Layer::Results { view, query }) => {
                let (view, query) = (view.clone(), query.clone());
                return v_flex()
                    .size_full()
                    .child(self.detail_bar(query.clone().into(), Some(query), cx))
                    .child(div().flex_1().min_h_0().child(view))
                    .into_any_element();
            }
            None => {}
        }
        let content = self.root_content(cx);
        let tab_row = match self.section {
            Section::Discover if !wide => Some(self.discover_tabs(false, cx)),
            Section::YtMusic => Some(self.music_tabs(cx)),
            _ => None,
        };
        v_flex()
            .size_full()
            .child(self.root_bar(rail_mode, wide, cx))
            .children(tab_row)
            .child(div().flex_1().min_h_0().child(content))
            .into_any_element()
    }

    /// Back, title and search, for a pushed page. `search_query` reopens the input with it.
    fn detail_bar(&self, title: SharedString, search_query: Option<String>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        h_flex()
            .gap_2()
            .px_2()
            .pt(px(6.))
            .pb(px(7.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("layer-back")
                    .ghost()
                    .icon(Icon::new(IconName::ArrowLeft))
                    .accessibility_label(t(Str::CdBack))
                    .tooltip(t(Str::CdBack))
                    .on_click(cx.listener(|shell, _, _, cx| shell.back(cx))),
            )
            .child(div().flex_1().min_w_0().truncate().text_lg().font_semibold().child(title))
            .child(
                Button::new("layer-search")
                    .ghost()
                    .icon(Icon::new(IconName::Search))
                    .accessibility_label(t(Str::CdSearch))
                    .tooltip(t(Str::CdSearch))
                    .on_click(cx.listener(move |shell, _, window, cx| {
                        shell.open_search(search_query.clone().unwrap_or_default(), window, cx)
                    })),
            )
            .into_any_element()
    }

    /// Hamburger (drawer mode), the section title, Discover's inline tabs when wide, search.
    fn root_bar(&self, rail_mode: bool, wide: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let discover = self.section == Section::Discover;
        let title = if discover { self.discover_tab.label() } else { self.section.label() };
        let searchable = matches!(self.section, Section::Discover | Section::YtMusic | Section::Library);
        // The title slot keeps the widest tab label's room, so the inline tabs never shift.
        let reserve = DiscoverTab::ALL.iter().map(|tab| tab.label().chars().count()).max().unwrap_or(0) as f32 * 9.5;
        h_flex()
            .gap_2()
            .px_2()
            .pt(px(6.))
            .pb(px(7.))
            .items_center()
            .when(!wide || !discover, |bar| bar.border_b_1().border_color(theme.border))
            .when(!rail_mode, |bar| {
                bar.child(
                    Button::new("open-drawer")
                        .ghost()
                        .icon(Icon::new(Lucide::Menu))
                        .accessibility_label(t(Str::CdMenu))
                        .tooltip(t(Str::CdMenu))
                        .on_click(cx.listener(|shell, _, _, cx| {
                            shell.drawer_open = true;
                            cx.notify();
                        })),
                )
            })
            .when(rail_mode, |bar| bar.child(div().w(px(8.))))
            .child(
                div()
                    .text_lg()
                    .font_semibold()
                    .truncate()
                    .when(discover && wide, |slot| slot.min_w(px(reserve)))
                    .when(!(discover && wide), |slot| slot.flex_1().min_w_0())
                    .child(title),
            )
            .when(discover && wide, |bar| bar.child(div().pl_4().flex_1().child(self.discover_tabs(true, cx))))
            .when(searchable, |bar| {
                bar.child(
                    Button::new("open-search")
                        .ghost()
                        .icon(Icon::new(IconName::Search))
                        .accessibility_label(t(Str::CdSearch))
                        .tooltip(t(Str::CdSearch))
                        .on_click(cx.listener(|shell, _, window, cx| shell.open_search(String::new(), window, cx))),
                )
            })
            .into_any_element()
    }

    /// Discover's icon tabs: inline in the bar (88 each at most) or a full-width row under it.
    fn discover_tabs(&self, inline: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mut row = h_flex().id("discover-tabs").role(Role::TabList).aria_label(t(Str::SectionDiscover));
        if !inline {
            row = row.w_full().border_b_1().border_color(theme.border);
        }
        for (index, tab) in DiscoverTab::ALL.iter().copied().enumerate() {
            let selected = tab == self.discover_tab;
            let cell = div()
                .id(("discover-tab", index))
                .role(Role::Tab)
                .aria_label(tab.label())
                .aria_selected(selected)
                .h(px(44.))
                .flex()
                .items_center()
                .justify_center()
                .relative()
                .cursor_pointer()
                .rounded(px(10.))
                .hover(|style| style.bg(theme.secondary))
                .child(Icon::new(tab.icon()).text_color(if selected { theme.primary } else { theme.muted_foreground }))
                .when(selected, |cell| {
                    cell.child(div().absolute().bottom_0().left_2().right_2().h(px(2.)).bg(theme.primary))
                })
                .on_click(cx.listener(move |shell, _, _, cx| {
                    shell.discover_tab = tab;
                    persist_setting(cx, LAST_DISCOVER_TAB_KEY, tab.id().to_string());
                    cx.notify();
                }));
            let cell = self.tab_focus.item(index, cell, cx);
            row = row.child(if inline { cell.w(px(88.)) } else { cell.flex_1() });
        }
        self.tab_focus.container(row, 0..DiscoverTab::ALL.len(), cx).into_any_element()
    }

    /// YT Music's text tabs with a sliding underline, pinned under the section bar.
    fn music_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let selected = MusicTab::ALL.iter().position(|tab| *tab == self.music_tab).unwrap_or(0);
        let shell = cx.entity().downgrade();
        div()
            .w_full()
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .child(tab_row(
                "music-tabs",
                MusicTab::ALL.iter().map(|tab| SharedString::from(tab.label())).collect(),
                selected,
                &self.music_tab_focus,
                &self.music_tab_drag,
                move |index, _, cx| {
                    if let Some(tab) = MusicTab::ALL.get(index).copied() {
                        shell
                            .update(cx, |shell, cx| {
                                shell.music_tab = tab;
                                cx.notify();
                            })
                            .ok();
                    }
                },
                cx,
            ))
            .into_any_element()
    }

    fn toggle_rail(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.rail_manual = Some(!expanded);
        cx.notify();
    }

    /// The app mark, name and version, beside the rail toggle or atop the drawer.
    fn brand(&self, logo: f32, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .size(px(logo))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(theme.primary)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(Lucide::Music).size(px(logo * 0.6)).text_color(theme.primary_foreground)),
            )
            .child(v_flex().min_w_0().child(div().font_semibold().child(t(Str::AppName))).child(
                div().text_xs().text_color(theme.muted_foreground).child(format!("v{}", env!("CARGO_PKG_VERSION"))),
            ))
    }

    /// One section entry: an icon-only 48 square collapsed, else a full-width icon-and-label row whose
    /// icon starts `icon_start` in.
    fn section_entry(&self, section: Section, labelled: bool, icon_start: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let selected = self.section == section;
        let entry = div()
            .id(("section", section.id() as usize))
            .role(Role::Tab)
            .aria_label(section.label())
            .aria_selected(selected)
            .cursor_pointer()
            .relative()
            .when(selected, |entry| entry.bg(theme.secondary))
            .hover(|style| style.bg(theme.secondary))
            .on_click(cx.listener(move |shell, _, _, cx| shell.select_section(section, cx)));
        let position = Section::GROUPS.iter().flat_map(|group| group.iter()).position(|entry| *entry == section);
        let entry = self.nav_focus.item(position.unwrap_or(0), entry, cx);
        let tint = if selected { theme.primary } else { theme.foreground };
        if !labelled {
            return entry
                .size(px(48.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(section.icon()).text_color(tint))
                .into_any_element();
        }
        entry
            .w_full()
            .child(
                h_flex()
                    .gap_4()
                    .pl(px(icon_start))
                    .pr(px(DRAWER_ICON_START))
                    .py(px(11.))
                    .items_center()
                    .child(Icon::new(section.icon()).text_color(tint))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_color(tint)
                            .when(selected, |label| label.font_bold())
                            .child(section.label()),
                    ),
            )
            .when(selected, |entry| {
                entry.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .flex()
                        .items_center()
                        .child(div().w(px(3.)).h(px(33.)).rounded_r(px(3.)).bg(theme.primary)),
                )
            })
            .into_any_element()
    }

    fn section_groups(&self, labelled: bool, icon_start: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut list = v_flex().gap_1().items_center().w_full();
        for (index, group) in Section::GROUPS.iter().enumerate() {
            if index > 0 {
                list = list.child(
                    div()
                        .w_full()
                        .py_1()
                        .when(!labelled, |divider| divider.px_2())
                        .child(div().h(px(1.)).w_full().bg(theme.border)),
                );
            }
            for section in group.iter().copied() {
                list = list.child(self.section_entry(section, labelled, icon_start, cx));
            }
        }
        let count = Section::GROUPS.iter().map(|group| group.len()).sum();
        let list = self.nav_focus.container(list, 0..count, cx);
        list.into_any_element()
    }

    fn render_rail(&self, expanded: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let toggle = Button::new("rail-toggle")
            .ghost()
            .icon(Icon::new(Lucide::Menu))
            .accessibility_label(t(Str::CdMenu))
            .tooltip(t(Str::CdMenu))
            .on_click(cx.listener(move |shell, _, _, cx| shell.toggle_rail(expanded, cx)));
        // The toggle and every section icon share the collapsed rail's centre line, so expanding moves
        // none of them.
        let toggle = div().w(px(RAIL_COLLAPSED)).flex_shrink_0().flex().justify_center().child(toggle);
        let header = h_flex()
            .min_h(px(48.))
            .items_center()
            .child(toggle)
            .when(expanded, |header| header.pr_4().child(self.brand(24., cx)))
            .into_any_element();
        let website = expanded.then(|| {
            focusable(div().id("website").role(Role::Link).aria_label("ymusic.io"), cx)
                .ml(px(RAIL_ICON_START - 4.))
                .p_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .child("ymusic.io")
                .on_click(|_, _, cx| cx.open_url(WEBSITE))
        });
        v_flex()
            .id("rail")
            .role(Role::Navigation)
            .w(px(if expanded { RAIL_EXPANDED } else { RAIL_COLLAPSED }))
            .h_full()
            .flex_shrink_0()
            .border_r_1()
            .border_color(theme.border)
            .child(div().pt_1().child(header))
            .child(
                div()
                    .id("rail-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(!expanded, |list| list.px_2())
                    .py_1()
                    .child(self.section_groups(expanded, RAIL_ICON_START, cx))
                    .children(website),
            )
            .into_any_element()
    }

    fn render_drawer(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let panel = v_flex()
            .id("drawer")
            .role(Role::Navigation)
            .w(px(DRAWER_WIDTH))
            .h_full()
            .bg(theme.background)
            .border_r_1()
            .border_color(theme.border)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().pl(px(18.)).pr(px(18.)).pt_4().pb(px(13.)).child(self.brand(46., cx)))
            .child(div().h(px(1.)).w_full().bg(theme.border))
            .child(div().id("drawer-scroll").flex_1().overflow_y_scroll().py_2().child(self.section_groups(
                true,
                DRAWER_ICON_START,
                cx,
            )));
        div()
            .absolute()
            .inset_0()
            .bg(black().opacity(0.4))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|shell, _, _, cx| {
                    shell.drawer_open = false;
                    cx.notify();
                }),
            )
            .child(panel)
            .into_any_element()
    }
}

impl Shell {
    fn step_volume(&mut self, steps: f32, window: &mut Window, cx: &mut Context<Self>) {
        let volume = self.playback.read(cx).volume;
        let snapped = (((volume / VOLUME_STEP).round() + steps) * VOLUME_STEP).clamp(0., 1.);
        self.playback.update(cx, |playback, cx| playback.set_volume(snapped, cx));
        window.push_notification(tf(Str::SoundVolume, &[&((snapped * 100.).round() as i32)]), cx);
    }

    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        match self.section {
            Section::Discover => {
                if let Some(tab) = DiscoverTab::ALL.get(index).copied() {
                    self.discover_tab = tab;
                    persist_setting(cx, LAST_DISCOVER_TAB_KEY, tab.id().to_string());
                }
            }
            Section::YtMusic => {
                if let Some(tab) = MusicTab::ALL.get(index).copied() {
                    self.music_tab = tab;
                }
            }
            Section::Library => {
                let view = self.library_view(cx);
                view.update(cx, |library, cx| library.select_tab_index(index, cx));
            }
            _ => return,
        }
        cx.notify();
    }

    /// Peels back full screen, Now Playing, then the drawer; with none open, a second Esc within
    /// the double-click window goes back.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if window.is_fullscreen() {
            window.toggle_fullscreen();
        } else if self.now_playing_open {
            self.now_playing_open = false;
        } else if self.drawer_open {
            self.drawer_open = false;
        } else if self.idle_escape_at.take().is_some_and(|at| at.elapsed() <= DOUBLE_ESCAPE) {
            self.back(cx);
            return;
        } else {
            self.idle_escape_at = Some(Instant::now());
            return;
        }
        cx.notify();
    }

    fn shortcut_listeners(&self, root: Div, cx: &mut Context<Self>) -> Div {
        use crate::shortcuts::*;
        root.track_focus(&self.focus)
            .key_context(CONTEXT)
            .on_action(cx.listener(|shell, _: &TogglePlay, _, cx| shell.playback.update(cx, |p, cx| p.toggle(cx))))
            .on_action(cx.listener(|shell, _: &SeekBackward, _, cx| {
                shell.playback.update(cx, |p, cx| p.seek_by(-SEEK_STEP_MS, cx))
            }))
            .on_action(cx.listener(|shell, _: &SeekForward, _, cx| {
                shell.playback.update(cx, |p, cx| p.seek_by(SEEK_STEP_MS, cx))
            }))
            .on_action(cx.listener(|shell, _: &PreviousTrack, _, cx| shell.playback.update(cx, |p, cx| p.previous(cx))))
            .on_action(cx.listener(|shell, _: &NextTrack, _, cx| shell.playback.update(cx, |p, cx| p.next(cx))))
            .on_action(cx.listener(|shell, _: &VolumeUp, window, cx| shell.step_volume(1., window, cx)))
            .on_action(cx.listener(|shell, _: &VolumeDown, window, cx| shell.step_volume(-1., window, cx)))
            .on_action(cx.listener(|shell, _: &ToggleNowPlaying, _, cx| {
                if shell.playback.read(cx).current().is_some() {
                    shell.now_playing_open = !shell.now_playing_open;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|shell, _: &ToggleDrawer, window, cx| {
                let viewport = window.viewport_size();
                let rail_mode = f32::from(viewport.width) >= 750. && f32::from(viewport.height) >= 480.;
                if !rail_mode {
                    shell.drawer_open = !shell.drawer_open;
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|shell, _: &OpenSearch, window, cx| {
                let searchable = matches!(shell.section, Section::Discover | Section::YtMusic | Section::Library);
                let searching = matches!(
                    shell.stacks.get(&shell.section).and_then(|stack| stack.pushed.last()),
                    Some(Layer::Search(_))
                );
                if searchable && !searching {
                    shell.open_search(String::new(), window, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &ToggleFullscreen, window, _| window.toggle_fullscreen()))
            .on_action(cx.listener(|shell, _: &Escape, window, cx| shell.escape(window, cx)))
            .on_action(cx.listener(|shell, _: &SelectTab1, _, cx| shell.select_tab(0, cx)))
            .on_action(cx.listener(|shell, _: &SelectTab2, _, cx| shell.select_tab(1, cx)))
            .on_action(cx.listener(|shell, _: &SelectTab3, _, cx| shell.select_tab(2, cx)))
            .on_action(cx.listener(|shell, _: &SelectTab4, _, cx| shell.select_tab(3, cx)))
            .on_action(cx.listener(|shell, _: &SelectTab5, _, cx| shell.select_tab(4, cx)))
            .on_action(cx.listener(|shell, _: &SelectTab6, _, cx| shell.select_tab(5, cx)))
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.focused(cx).is_none() {
            self.focus.focus(window, cx);
        }
        if let Some(target) = self.pending_download.take() {
            DownloadPicker::open(target, window, cx);
        }
        if std::mem::take(&mut self.pending_sound) {
            crate::sound_panel::SoundPanel::open(self.playback.clone(), window, cx);
        }
        if std::mem::take(&mut self.pending_signin) {
            crate::signin_dialog::SignInDialog::open(window, cx, |_| {});
        }
        let viewport = window.viewport_size();
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        let rail_mode = width >= 750. && height >= 480.;
        let auto_expanded = width >= 1200.;
        if self.rail_auto != Some((rail_mode, auto_expanded)) {
            self.rail_auto = Some((rail_mode, auto_expanded));
            self.rail_manual = None;
        }
        if rail_mode {
            self.drawer_open = false;
        }
        let expanded = self.rail_manual.unwrap_or(auto_expanded);
        // Measured as if the rail were expanded, so toggling it never moves the tabs.
        let shell_width = if rail_mode { width - RAIL_EXPANDED } else { width };
        let wide = (shell_width > height && shell_width >= 600.) || shell_width >= 650.;

        let now_playing = self.now_playing_open && self.playback.read(cx).current().is_some();
        if !now_playing {
            self.now_playing.read(cx).hide_video();
        }
        self.playback.update(cx, |playback, cx| playback.set_video_hidden(!now_playing, cx));
        let body = if now_playing {
            self.now_playing.clone().into_any_element()
        } else {
            let main = self.render_main(rail_mode, wide, cx);
            h_flex()
                .size_full()
                .when(rail_mode, |row| row.child(self.render_rail(expanded, cx)))
                .child(div().flex_1().min_w_0().h_full().child(main))
                .into_any_element()
        };
        let drawer = (self.drawer_open && !now_playing).then(|| self.render_drawer(cx));
        let root = self.shortcut_listeners(v_flex(), cx);
        root.relative()
            .size_full()
            .when(!now_playing, |shell| shell.bg(cx.theme().background))
            .child(crate::a11y::frame_marker())
            .child(div().flex_1().min_h_0().w_full().child(body))
            .when(!now_playing, |shell| shell.child(self.mini_player.clone()))
            .children(drawer)
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
