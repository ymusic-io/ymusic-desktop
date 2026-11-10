//! The local Library, laid out like the JVM app: pinned text tabs, then per tab a 40-tall toolbar
//! (folders: Add folder, rescan, breadcrumb; songs: Shuffle all; all: the sort control) over a grid
//! of folder/group cards (at most 176 wide) and song cells (at least 360 wide); a list below 480.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab as Tab_, TabBar};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_i18n::{Str, t, tf};
use ym_store::{Bucket, MediaItem, MediaStore};

use crate::graph::{AppGraph, on_core};
use crate::mini_player::clock;
use crate::playback::QueueItem;

/// Play these local files in order from `index`.
pub struct PlayLocal {
    pub items: Vec<QueueItem>,
    pub index: usize,
}

/// Put a local file next in the queue, or at its end.
pub struct QueueLocal {
    pub item: QueueItem,
    pub next: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SongKey {
    Name,
    DateAdded,
    Type,
    TrackNumber,
}

impl SongKey {
    const ALL: [SongKey; 4] = [SongKey::Name, SongKey::DateAdded, SongKey::Type, SongKey::TrackNumber];

    fn label(self) -> &'static str {
        t(match self {
            SongKey::Name => Str::SortName,
            SongKey::DateAdded => Str::SortDateAdded,
            SongKey::Type => Str::SortType,
            SongKey::TrackNumber => Str::SortTrackNumber,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupKey {
    Name,
    SongCount,
}

impl GroupKey {
    const ALL: [GroupKey; 2] = [GroupKey::Name, GroupKey::SongCount];

    fn label(self) -> &'static str {
        t(match self {
            GroupKey::Name => Str::SortName,
            GroupKey::SongCount => Str::SortSongCount,
        })
    }
}

const GRID_MIN_WIDTH: f32 = 480.;
const CARD_MAX: f32 = 176.;
const SONG_MIN: f32 = 360.;
const GRID_PADDING: f32 = 10.;
const THUMB: f32 = 56.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Folders,
    Songs,
    Artists,
    Albums,
    Genres,
    Recent,
}

impl Tab {
    const ALL: [Tab; 6] = [Tab::Folders, Tab::Songs, Tab::Artists, Tab::Albums, Tab::Genres, Tab::Recent];

    fn label(self) -> &'static str {
        t(match self {
            Tab::Folders => Str::LibraryTabFolders,
            Tab::Songs => Str::LibraryTabSongs,
            Tab::Artists => Str::LibraryTabArtists,
            Tab::Albums => Str::LibraryTabAlbums,
            Tab::Genres => Str::LibraryTabGenres,
            Tab::Recent => Str::PlaylistRecentPlay,
        })
    }
}

/// The play-history key of a local file: its YouTube id when tagged, else size and length.
fn history_code(item: &MediaItem) -> String {
    item.youtube_vid.clone().unwrap_or_else(|| format!("@{}:{}", item.file_size, item.duration_ms.unwrap_or(0) / 1000))
}

pub struct LibraryView {
    store: Arc<MediaStore>,
    history: Arc<ym_history::PlayHistory>,
    tab: Tab,
    /// Every file, for the Songs / Artists / Albums tabs.
    all_items: Vec<MediaItem>,
    recent: Vec<MediaItem>,
    /// The artist or album opened from its tab.
    group: Option<String>,
    /// The open folder chain; empty is the root.
    path: Vec<Bucket>,
    folders: Vec<Bucket>,
    items: Vec<MediaItem>,
    scanning: bool,
    summary: Option<String>,
    scroll: ScrollHandle,
    song_sort: (SongKey, bool),
    group_sort: (GroupKey, bool),
}

impl EventEmitter<PlayLocal> for LibraryView {}
impl EventEmitter<QueueLocal> for LibraryView {}

impl LibraryView {
    pub fn new(store: Arc<MediaStore>, cx: &mut Context<Self>) -> Self {
        let history = cx.global::<AppGraph>().history.clone();
        let mut view = Self {
            store,
            history,
            tab: Tab::Folders,
            all_items: Vec::new(),
            recent: Vec::new(),
            group: None,
            path: Vec::new(),
            folders: Vec::new(),
            items: Vec::new(),
            scanning: false,
            summary: None,
            scroll: ScrollHandle::new(),
            song_sort: (SongKey::Name, true),
            group_sort: (GroupKey::Name, true),
        };
        view.refresh(cx);
        view
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let folder = self.path.last().map(|bucket| bucket.id);
        let load = on_core(cx, async move {
            tokio::task::spawn_blocking(move || match folder {
                None => Ok((store.root_buckets()?, Vec::new())),
                Some(id) => Ok::<_, ym_store::StoreError>((store.child_buckets(id)?, store.items_in_bucket(id)?)),
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |view, cx| {
                match result {
                    Some(Ok(Ok((folders, items)))) => {
                        view.folders = folders;
                        view.items = items;
                    }
                    Some(Ok(Err(error))) => {
                        cx.global::<AppGraph>().log.warn("R90008 library read failed", Some(&error));
                        view.summary = Some(tf(Str::ErrorGeneric, &[&"R90008"]));
                    }
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn select_tab_index(&mut self, index: usize, cx: &mut Context<Self>) {
        self.select_tab(Tab::ALL[index.min(Tab::ALL.len() - 1)], cx);
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.group = None;
        if tab == Tab::Folders {
            self.refresh(cx);
        } else {
            self.refresh_all(cx);
        }
        cx.notify();
    }

    /// Every file, plus the recently played ones in play order.
    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        let (store, history) = (self.store.clone(), self.history.clone());
        let load = on_core(cx, async move {
            tokio::task::spawn_blocking(move || {
                let items = store.all_items().map_err(|error| error.to_string())?;
                let codes = history.local_recent(ym_history::RETAIN).map_err(|error| error.to_string())?;
                let by_code: std::collections::HashMap<String, &MediaItem> =
                    items.iter().map(|item| (history_code(item), item)).collect();
                let recent = codes.iter().filter_map(|code| by_code.get(code).map(|item| (*item).clone())).collect();
                Ok::<_, String>((items, recent))
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |view, cx| {
                match result {
                    Some(Ok(Ok((mut items, recent)))) => {
                        items.sort_by_key(|item| item.title.to_lowercase());
                        view.all_items = items;
                        view.recent = recent;
                    }
                    Some(Ok(Err(message))) => {
                        cx.global::<AppGraph>().log.warn(&format!("R90008 library read failed: {message}"), None);
                        view.summary = Some(tf(Str::ErrorGeneric, &[&"R90008"]));
                    }
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The artist (or album) names of the open tab with their song counts, by name.
    fn groups(&self) -> Vec<(String, usize)> {
        let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for item in &self.all_items {
            if let Some(name) = self.group_of(item) {
                *counts.entry(name).or_default() += 1;
            }
        }
        let mut groups: Vec<(String, usize)> = counts.into_iter().collect();
        let (key, ascending) = self.group_sort;
        if key == GroupKey::SongCount {
            groups.sort_by_key(|(_, count)| *count);
        }
        if !ascending {
            groups.reverse();
        }
        groups
    }

    fn group_of(&self, item: &MediaItem) -> Option<String> {
        let name = match self.tab {
            Tab::Artists => item.artist.clone(),
            Tab::Albums => item.album.clone(),
            Tab::Genres => item.genre.clone(),
            _ => None,
        };
        name.filter(|name| !name.trim().is_empty())
    }

    /// The files the open tab lists, in the order they play.
    fn shown_items(&self) -> Vec<MediaItem> {
        let mut items = match self.tab {
            Tab::Recent => return self.recent.clone(),
            Tab::Folders => self.items.clone(),
            Tab::Songs => self.all_items.clone(),
            Tab::Artists | Tab::Albums | Tab::Genres => match &self.group {
                Some(group) => {
                    self.all_items.iter().filter(|item| self.group_of(item).as_ref() == Some(group)).cloned().collect()
                }
                None => Vec::new(),
            },
        };
        let (key, ascending) = self.song_sort;
        match key {
            SongKey::Name => items.sort_by_key(|item| item.title.to_lowercase()),
            SongKey::DateAdded => items.sort_by_key(|item| std::cmp::Reverse(item.id)),
            SongKey::Type => items.sort_by_key(|item| (item.video_codec.is_some(), item.mime_type.clone())),
            SongKey::TrackNumber => {
                items.sort_by_key(|item| (item.track_number.unwrap_or(i32::MAX), item.title.to_lowercase()))
            }
        }
        if !ascending {
            items.reverse();
        }
        items
    }

    fn shuffle_all(&mut self, cx: &mut Context<Self>) {
        let mut items: Vec<QueueItem> = self.shown_items().iter().map(QueueItem::from_local).collect();
        if items.is_empty() {
            return;
        }
        rand::seq::SliceRandom::shuffle(items.as_mut_slice(), &mut rand::rng());
        cx.emit(PlayLocal { items, index: 0 });
    }

    /// Re-scans every root folder.
    fn rescan(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let roots = on_core(cx, async move { tokio::task::spawn_blocking(move || store.root_buckets()).await });
        cx.spawn(async move |this, cx| {
            let result = roots.await;
            this.update(cx, |view, cx| match result {
                Some(Ok(Ok(roots))) => {
                    let paths = roots.iter().map(|bucket| std::path::PathBuf::from(&bucket.uri)).collect();
                    view.scan_paths(paths, false, cx);
                }
                Some(Ok(Err(error))) => {
                    cx.global::<AppGraph>().log.warn("R90008 library read failed", Some(&error));
                    view.summary = Some(tf(Str::ErrorGeneric, &[&"R90008"]));
                    cx.notify();
                }
                _ => {}
            })
            .ok();
        })
        .detach();
    }

    fn open_folder(&mut self, bucket: Bucket, cx: &mut Context<Self>) {
        self.path.push(bucket);
        self.refresh(cx);
    }

    fn open_crumb(&mut self, depth: usize, cx: &mut Context<Self>) {
        self.path.truncate(depth);
        self.refresh(cx);
    }

    fn scan(&mut self, cx: &mut Context<Self>) {
        if self.scanning {
            return;
        }
        let pick = rfd::AsyncFileDialog::new().set_title(t(Str::FolderPickerTitle)).pick_folder();
        cx.spawn(async move |this, cx| {
            let Some(folder) = pick.await else { return };
            this.update(cx, |view, cx| view.scan_path(folder.path().to_path_buf(), false, cx)).ok();
        })
        .detach();
    }

    /// Scans `root`, then opens it; `play_first` starts its first file (debug routes).
    pub fn scan_path(&mut self, root: std::path::PathBuf, play_first: bool, cx: &mut Context<Self>) {
        self.scan_paths(vec![root], play_first, cx);
    }

    fn scan_paths(&mut self, roots: Vec<std::path::PathBuf>, play_first: bool, cx: &mut Context<Self>) {
        if self.scanning || roots.is_empty() {
            return;
        }
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let started = this.update(cx, |view, cx| {
                view.scanning = true;
                view.summary = None;
                cx.notify();
            });
            if started.is_err() {
                return;
            }
            let scan = cx.update(|cx| {
                on_core(cx, async move {
                    tokio::task::spawn_blocking(move || {
                        let cancel = AtomicBool::new(false);
                        let mut total = ym_store::ScanReport::default();
                        for root in &roots {
                            let report = ym_store::scan_folder(&store, root, &cancel)?;
                            total.added += report.added;
                            total.updated += report.updated;
                            total.unchanged += report.unchanged;
                            total.removed += report.removed;
                            total.errors.extend(report.errors);
                        }
                        Ok::<_, ym_store::StoreError>(total)
                    })
                    .await
                })
            });
            let result = scan.await;
            this.update(cx, |view, cx| {
                view.scanning = false;
                let log = cx.global::<AppGraph>().log.fork("library");
                view.summary = Some(match result {
                    Some(Ok(Ok(report))) => {
                        for error in &report.errors {
                            log.warn(&format!("scan skipped {}: {}", error.path.display(), error.message), None);
                        }
                        let (scanned, added, updated, removed) =
                            (report.scanned(), report.added, report.updated, report.removed);
                        if report.errors.is_empty() {
                            tf(Str::LibraryScanSummary, &[&scanned, &added, &updated, &removed])
                        } else {
                            tf(
                                Str::LibraryScanSummaryFailed,
                                &[&scanned, &added, &updated, &removed, &report.errors.len()],
                            )
                        }
                    }
                    Some(Ok(Err(error))) => {
                        log.warn("R90009 library scan failed", Some(&error));
                        tf(Str::LibraryScanFailed, &[&"R90009"])
                    }
                    _ => tf(Str::LibraryScanFailed, &[&"R90009"]),
                });
                if play_first {
                    view.open_root_and_play(cx);
                } else {
                    view.refresh(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_root_and_play(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let load = on_core(cx, async move {
            tokio::task::spawn_blocking(move || {
                let root = store.root_buckets()?.into_iter().next();
                let items = match &root {
                    Some(bucket) => store.items_in_bucket(bucket.id)?,
                    None => Vec::new(),
                };
                Ok::<_, ym_store::StoreError>((root, items))
            })
            .await
        });
        cx.spawn(async move |this, cx| {
            if let Some(Ok(Ok((Some(root), items)))) = load.await {
                this.update(cx, |view, cx| {
                    view.path = vec![root];
                    view.items = items;
                    view.play(0, cx);
                    view.refresh(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn play(&mut self, index: usize, cx: &mut Context<Self>) {
        let items = self.shown_items().iter().map(QueueItem::from_local).collect();
        cx.emit(PlayLocal { items, index });
    }
}

fn reveal_label() -> &'static str {
    t(if cfg!(target_os = "macos") {
        Str::MenuRevealFinder
    } else if cfg!(target_os = "windows") {
        Str::MenuRevealExplorer
    } else {
        Str::MenuRevealFileManager
    })
}

/// Splits `cells` into rows of `columns` equal cells, padding a short last row.
fn grid_rows(cells: Vec<AnyElement>, columns: usize) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    let mut cells = cells.into_iter().peekable();
    while cells.peek().is_some() {
        let mut row = h_flex().w_full().items_start();
        for _ in 0..columns {
            row = row.child(match cells.next() {
                Some(cell) => div().flex_1().min_w_0().child(cell),
                None => div().flex_1(),
            });
        }
        rows.push(row.into_any_element());
    }
    rows
}

impl LibraryView {
    /// A 56 thumbnail: the file kind's icon, with the duration badged at the bottom-end.
    fn thumb(icon: Lucide, duration_ms: Option<i64>, cx: &App) -> Div {
        let theme = cx.theme();
        div()
            .relative()
            .size(px(THUMB))
            .flex_shrink_0()
            .rounded(px(10.))
            .bg(theme.secondary)
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(icon).text_color(theme.muted_foreground))
            .when_some(duration_ms, |thumb, duration| {
                thumb.child(
                    div()
                        .absolute()
                        .bottom(px(4.))
                        .right(px(4.))
                        .px(px(4.))
                        .py(px(1.))
                        .rounded(px(3.))
                        .bg(black().opacity(0.7))
                        .text_color(white())
                        .text_size(px(10.))
                        .child(clock(duration)),
                )
            })
    }

    fn song_cell(&self, index: usize, item: &MediaItem, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let detail = [item.artist.clone(), item.album.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let label = [Some(item.title.clone()), item.artist.clone(), item.duration_ms.map(clock)]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        let view = cx.entity().downgrade();
        let local = QueueItem::from_local(item);
        let path = std::path::PathBuf::from(&item.uri);
        let more = Button::new(("song-more", index))
            .ghost()
            .small()
            .icon(IconName::EllipsisVertical)
            .accessibility_label(t(Str::CdMore))
            .tooltip(t(Str::CdMore))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let (play, next, last) = (view.clone(), view.clone(), view.clone());
                let (next_item, last_item, path) = (local.clone(), local.clone(), path.clone());
                menu.item(PopupMenuItem::new(t(Str::MenuPlay)).on_click(move |_, _, cx| {
                    play.update(cx, |view, cx| view.play(index, cx)).ok();
                }))
                .item(PopupMenuItem::new(t(Str::QueueNext)).on_click(move |_, _, cx| {
                    next.update(cx, |_, cx| cx.emit(QueueLocal { item: next_item.clone(), next: true })).ok();
                }))
                .item(PopupMenuItem::new(t(Str::QueueLast)).on_click(move |_, _, cx| {
                    last.update(cx, |_, cx| cx.emit(QueueLocal { item: last_item.clone(), next: false })).ok();
                }))
                .separator()
                .item(PopupMenuItem::new(reveal_label()).on_click(move |_, _, cx| cx.reveal_path(&path)))
            });
        h_flex()
            .id(("local-item", index))
            .role(Role::ListItem)
            .aria_label(label)
            .tab_stop(true)
            .pl_4()
            .pr_1()
            .py_2()
            .gap_4()
            .items_start()
            .cursor_pointer()
            .rounded(px(10.))
            .hover(|style| style.bg(theme.secondary))
            .child(Self::thumb(
                if item.video_codec.is_some() { Lucide::Film } else { Lucide::Music },
                item.duration_ms,
                cx,
            ))
            .child(v_flex().flex_1().min_w_0().child(div().text_sm().line_clamp(2).child(item.title.clone())).when(
                !detail.is_empty(),
                |column| {
                    column.child(div().pt_1().text_xs().text_color(theme.muted_foreground).truncate().child(detail))
                },
            ))
            .child(div().pt(px(2.)).child(more))
            .on_click(cx.listener(move |view, _, _, cx| view.play(index, cx)))
            .into_any_element()
    }

    /// A grid card: square art, the title (2 lines) and a subtitle; `grid` false draws a list row.
    fn card(
        id: ElementId,
        icon: Lucide,
        title: String,
        subtitle: String,
        grid: bool,
        dimmed: bool,
        cx: &App,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let label = format!("{title}, {subtitle}");
        let base = div()
            .id(id)
            .role(Role::ListItem)
            .aria_label(label)
            .tab_stop(true)
            .cursor_pointer()
            .rounded(px(10.))
            .hover(|style| style.bg(theme.secondary))
            .when(dimmed, |card| card.opacity(0.4));
        if grid {
            base.flex()
                .flex_col()
                .p(px(6.))
                .child(
                    div()
                        .w_full()
                        .aspect_ratio(1.)
                        .rounded(px(10.))
                        .bg(theme.secondary)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(icon).size(px(40.)).text_color(theme.muted_foreground)),
                )
                .child(div().pt(px(7.)).text_sm().line_clamp(2).child(title))
                .child(div().pt(px(3.)).text_xs().text_color(theme.muted_foreground).truncate().child(subtitle))
        } else {
            base.flex().px_4().py_2().gap_4().items_start().child(Self::thumb(icon, None, cx)).child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().line_clamp(2).child(title))
                    .child(div().pt_1().text_xs().text_color(theme.muted_foreground).child(subtitle)),
            )
        }
    }

    /// The sort chip (current key, a menu of keys) and the direction toggle.
    fn sort_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let songs = self.group.is_some() || matches!(self.tab, Tab::Folders | Tab::Songs);
        let (current, ascending) = if songs {
            (self.song_sort.0.label(), self.song_sort.1)
        } else {
            (self.group_sort.0.label(), self.group_sort.1)
        };
        let view = cx.entity().downgrade();
        let (song_key, group_key) = (self.song_sort.0, self.group_sort.0);
        let chip = Button::new("sort")
            .outline()
            .small()
            .rounded_full()
            .icon(Lucide::ArrowUpDown)
            .label(current)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                if songs {
                    for key in SongKey::ALL {
                        let view = view.clone();
                        menu = menu.item(PopupMenuItem::new(key.label()).checked(key == song_key).on_click(
                            move |_, _, cx| {
                                view.update(cx, |view, cx| {
                                    let ascending = if view.song_sort.0 == key { !view.song_sort.1 } else { true };
                                    view.song_sort = (key, ascending);
                                    cx.notify();
                                })
                                .ok();
                            },
                        ));
                    }
                } else {
                    for key in GroupKey::ALL {
                        let view = view.clone();
                        menu = menu.item(PopupMenuItem::new(key.label()).checked(key == group_key).on_click(
                            move |_, _, cx| {
                                view.update(cx, |view, cx| {
                                    let ascending = if view.group_sort.0 == key { !view.group_sort.1 } else { true };
                                    view.group_sort = (key, ascending);
                                    cx.notify();
                                })
                                .ok();
                            },
                        ));
                    }
                }
                menu
            });
        let direction = Button::new("sort-direction")
            .ghost()
            .small()
            .icon(if ascending { IconName::ArrowUp } else { IconName::ArrowDown })
            .accessibility_label(current)
            .tooltip(current)
            .on_click(cx.listener(move |view, _, _, cx| {
                if songs {
                    view.song_sort.1 = !view.song_sort.1;
                } else {
                    view.group_sort.1 = !view.group_sort.1;
                }
                cx.notify();
            }));
        h_flex().gap_1().items_center().child(chip).child(direction).into_any_element()
    }

    fn rescan_button(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.scanning {
            return div().size(px(40.)).flex().items_center().justify_center().child(Spinner::new()).into_any_element();
        }
        Button::new("rescan")
            .ghost()
            .icon(Lucide::RefreshCw)
            .accessibility_label(t(Str::LibraryRescan))
            .tooltip(t(Str::LibraryRescan))
            .on_click(cx.listener(|view, _, _, cx| view.rescan(cx)))
            .into_any_element()
    }

    /// The 40-tall bar over the tab's content.
    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let bar = h_flex().h(px(40.)).pl_4().pr_2().gap_2().items_center();
        let bar = match (self.tab, &self.group) {
            (Tab::Folders, _) if self.path.is_empty() => bar
                .child(
                    Button::new("add-folder")
                        .outline()
                        .small()
                        .rounded_full()
                        .label(if self.scanning { t(Str::LibraryScanning) } else { t(Str::LibraryAddFolder) })
                        .on_click(cx.listener(|view, _, _, cx| view.scan(cx))),
                )
                .child(self.rescan_button(cx)),
            (Tab::Folders, _) => {
                let mut crumbs = h_flex().id("crumbs").flex_1().min_w_0().overflow_x_scroll().gap_1().items_center();
                crumbs = crumbs.child(
                    div()
                        .id("crumb-root")
                        .role(Role::Link)
                        .aria_label(t(Str::LibraryCrumbRoot))
                        .tab_stop(true)
                        .px_1()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.secondary))
                        .text_sm()
                        .child(t(Str::LibraryCrumbRoot))
                        .on_click(cx.listener(|view, _, _, cx| view.open_crumb(0, cx))),
                );
                let last = self.path.len() - 1;
                for (depth, bucket) in self.path.iter().enumerate() {
                    let name = bucket.alias_name.clone().unwrap_or_else(|| bucket.name.clone());
                    crumbs = crumbs
                        .child(Icon::new(IconName::ChevronRight).size(px(16.)).text_color(theme.muted_foreground));
                    crumbs = crumbs.child(if depth == last {
                        div().px_1().text_sm().font_semibold().truncate().child(name).into_any_element()
                    } else {
                        div()
                            .id(("crumb", depth))
                            .role(Role::Link)
                            .aria_label(name.clone())
                            .tab_stop(true)
                            .px_1()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.secondary))
                            .text_sm()
                            .truncate()
                            .child(name)
                            .on_click(cx.listener(move |view, _, _, cx| view.open_crumb(depth + 1, cx)))
                            .into_any_element()
                    });
                }
                bar.child(crumbs).child(self.rescan_button(cx))
            }
            (_, Some(group)) => bar
                .child(
                    div()
                        .id("group-back")
                        .role(Role::Link)
                        .aria_label(self.tab.label())
                        .tab_stop(true)
                        .px_1()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.secondary))
                        .text_sm()
                        .child(self.tab.label())
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.group = None;
                            cx.notify();
                        })),
                )
                .child(Icon::new(IconName::ChevronRight).size(px(16.)).text_color(theme.muted_foreground))
                .child(div().text_sm().font_semibold().truncate().child(group.clone()))
                .child(self.shuffle_chip(cx)),
            (Tab::Songs | Tab::Recent, _) => bar.child(self.shuffle_chip(cx)),
            _ => bar,
        };
        let sortable = self.tab != Tab::Recent;
        bar.child(div().flex_1()).when(sortable, |bar| bar.child(self.sort_control(cx))).into_any_element()
    }

    fn shuffle_chip(&self, cx: &mut Context<Self>) -> Button {
        Button::new("shuffle-all")
            .outline()
            .small()
            .rounded_full()
            .icon(Lucide::Shuffle)
            .label(t(Str::ShuffleAll))
            .on_click(cx.listener(|view, _, _, cx| view.shuffle_all(cx)))
    }
}

impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let width = crate::feed::feed_width(window);
        let grid = width >= GRID_MIN_WIDTH;
        let card_columns = ((width - 2. * GRID_PADDING) / CARD_MAX).ceil().max(2.) as usize;
        let song_columns = ((width - 2. * GRID_PADDING) / SONG_MIN).floor().max(1.) as usize;

        let selected = Tab::ALL.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        let tabs = div().w_full().px_3().border_b_1().border_color(theme.border).child(
            TabBar::new("library-tabs")
                .underline()
                .selected_index(selected)
                .children(Tab::ALL.iter().map(|tab| Tab_::new().label(tab.label())))
                .on_click(cx.listener(|view, index: &usize, _, cx| view.select_tab(Tab::ALL[*index], cx))),
        );

        let nothing_indexed = self.tab == Tab::Folders && self.path.is_empty() && self.folders.is_empty();
        if nothing_indexed && !self.scanning {
            return v_flex()
                .size_full()
                .child(tabs)
                .child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap(px(14.))
                        .child(div().text_color(theme.muted_foreground).child(t(Str::LibraryEmpty)))
                        .child(
                            Button::new("scan-empty")
                                .primary()
                                .rounded_full()
                                .label(t(Str::LibraryScanFolder))
                                .on_click(cx.listener(|view, _, _, cx| view.scan(cx))),
                        )
                        .when_some(self.summary.clone(), |column, summary| {
                            column.child(div().text_sm().text_color(theme.muted_foreground).child(summary))
                        }),
                )
                .into_any_element();
        }

        let mut cards: Vec<AnyElement> = Vec::new();
        if self.tab == Tab::Folders {
            for (index, bucket) in self.folders.clone().into_iter().enumerate() {
                let name = bucket.alias_name.clone().unwrap_or_else(|| bucket.name.clone());
                let path = std::path::PathBuf::from(&bucket.uri);
                let more = Button::new(("folder-more", index))
                    .ghost()
                    .small()
                    .icon(IconName::EllipsisVertical)
                    .accessibility_label(t(Str::CdMore))
                    .tooltip(t(Str::CdMore))
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                        let path = path.clone();
                        menu.item(PopupMenuItem::new(reveal_label()).on_click(move |_, _, cx| cx.reveal_path(&path)))
                    });
                let card = Self::card(
                    ("folder", index).into(),
                    Lucide::Folder,
                    name,
                    t(Str::LibraryFolder).to_owned(),
                    grid,
                    bucket.excluded,
                    cx,
                )
                .relative()
                .child(div().absolute().top(px(8.)).right(px(if grid { 8. } else { 4. })).child(more))
                .on_click(cx.listener(move |view, _, _, cx| view.open_folder(bucket.clone(), cx)));
                cards.push(card.into_any_element());
            }
        }
        if self.group.is_none() && matches!(self.tab, Tab::Artists | Tab::Albums | Tab::Genres) {
            let icon = match self.tab {
                Tab::Artists => Lucide::User,
                Tab::Albums => Lucide::Disc,
                _ => Lucide::Tag,
            };
            for (index, (name, count)) in self.groups().into_iter().enumerate() {
                let subtitle = ym_i18n::plural(ym_i18n::Plural::NSongs, count as i64, &[&count]);
                let open = name.clone();
                let card = Self::card(("group", index).into(), icon, name, subtitle, grid, false, cx).on_click(
                    cx.listener(move |view, _, _, cx| {
                        view.group = Some(open.clone());
                        cx.notify();
                    }),
                );
                cards.push(card.into_any_element());
            }
        }
        let shown = self.shown_items();
        let songs: Vec<AnyElement> =
            shown.iter().enumerate().map(|(index, item)| self.song_cell(index, item, cx)).collect();

        let mut body = v_flex().w_full().pt_1().pb_4().px(px(if grid { GRID_PADDING } else { 0. }));
        body = body.child(div().mx(px(if grid { -GRID_PADDING } else { 0. })).child(self.toolbar(cx)));
        if let Some(summary) = self.summary.clone() {
            body = body.child(div().px_4().pb_2().text_sm().text_color(theme.muted_foreground).child(summary));
        }
        let empty = cards.is_empty() && songs.is_empty();
        if grid {
            body = body.children(grid_rows(cards, card_columns)).children(grid_rows(songs, song_columns));
        } else {
            body = body.children(cards).children(songs);
        }
        if empty {
            let message = t(match self.tab {
                Tab::Folders => Str::LibraryEmptyFolder,
                Tab::Songs | Tab::Recent => Str::LibraryNoSongs,
                Tab::Artists => Str::LibraryNoArtists,
                Tab::Albums => Str::LibraryNoAlbums,
                Tab::Genres => Str::LibraryNoGenres,
            });
            body = body.child(div().py_16().flex().justify_center().text_color(theme.muted_foreground).child(message));
        }
        v_flex()
            .size_full()
            .child(tabs)
            .child(
                div()
                    .id("library-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
            .into_any_element()
    }
}
