//! The local Library: scanned folders as a browsable tree, their media files, and the scan.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab as Tab_, TabBar};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Folders,
    Songs,
    Artists,
    Albums,
    Recent,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::Folders, Tab::Songs, Tab::Artists, Tab::Albums, Tab::Recent];

    fn label(self) -> &'static str {
        t(match self {
            Tab::Folders => Str::LibraryTabFolders,
            Tab::Songs => Str::LibraryTabSongs,
            Tab::Artists => Str::LibraryTabArtists,
            Tab::Albums => Str::LibraryTabAlbums,
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
}

impl EventEmitter<PlayLocal> for LibraryView {}

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

    /// Debug routes: opens the tab at `index`.
    pub fn dev_select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
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
        counts.into_iter().collect()
    }

    fn group_of(&self, item: &MediaItem) -> Option<String> {
        let name = match self.tab {
            Tab::Artists => item.artist.clone(),
            Tab::Albums => item.album.clone(),
            _ => None,
        };
        name.filter(|name| !name.trim().is_empty())
    }

    /// The files the open tab lists, in the order they play.
    fn shown_items(&self) -> Vec<MediaItem> {
        match self.tab {
            Tab::Folders => self.items.clone(),
            Tab::Songs => self.all_items.clone(),
            Tab::Recent => self.recent.clone(),
            Tab::Artists | Tab::Albums => match &self.group {
                Some(group) => {
                    self.all_items.iter().filter(|item| self.group_of(item).as_ref() == Some(group)).cloned().collect()
                }
                None => Vec::new(),
            },
        }
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
                    tokio::task::spawn_blocking(move || ym_store::scan_folder(&store, &root, &AtomicBool::new(false)))
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

impl Render for LibraryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut crumbs = h_flex().gap_1().items_center().child(
            Button::new("crumb-root")
                .ghost()
                .small()
                .label(t(Str::LibraryCrumbRoot))
                .on_click(cx.listener(|view, _, _, cx| view.open_crumb(0, cx))),
        );
        for (depth, bucket) in self.path.iter().enumerate() {
            crumbs = crumbs.child(div().text_color(theme.muted_foreground).child("/")).child(
                Button::new(("crumb", depth))
                    .ghost()
                    .small()
                    .label(bucket.alias_name.clone().unwrap_or_else(|| bucket.name.clone()))
                    .on_click(cx.listener(move |view, _, _, cx| view.open_crumb(depth + 1, cx))),
            );
        }
        let crumbs: AnyElement = match (self.tab, &self.group) {
            (Tab::Folders, _) => crumbs.into_any_element(),
            (_, Some(group)) => h_flex()
                .gap_1()
                .items_center()
                .child(Button::new("group-back").ghost().small().label(self.tab.label()).on_click(cx.listener(
                    |view, _, _, cx| {
                        view.group = None;
                        cx.notify();
                    },
                )))
                .child(div().text_color(theme.muted_foreground).child("/"))
                .child(div().text_sm().px_2().child(group.clone()))
                .into_any_element(),
            _ => div().into_any_element(),
        };
        let header = h_flex().justify_between().items_center().child(crumbs).child(
            h_flex()
                .gap_2()
                .items_center()
                .when(self.scanning, |row| {
                    row.child(Spinner::new()).child(div().text_sm().child(t(Str::LibraryScanning)))
                })
                .child(
                    Button::new("scan")
                        .label(t(Str::LibraryScanFolder))
                        .on_click(cx.listener(|view, _, _, cx| view.scan(cx))),
                ),
        );

        let mut list = v_flex().gap_1();
        let folders = if self.tab == Tab::Folders { self.folders.clone() } else { Vec::new() };
        let groups = if self.group.is_none() && matches!(self.tab, Tab::Artists | Tab::Albums) {
            self.groups()
        } else {
            Vec::new()
        };
        for (index, (name, count)) in groups.iter().cloned().enumerate() {
            list = list.child(
                h_flex()
                    .id(("group", index))
                    .gap_3()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary))
                    .child(
                        Icon::new(if self.tab == Tab::Artists { Lucide::User } else { Lucide::Disc })
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().flex_1().text_sm().truncate().child(name.clone()))
                    .child(div().text_xs().text_color(theme.muted_foreground).child(count.to_string()))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.group = Some(name.clone());
                        cx.notify();
                    })),
            );
        }
        for (index, bucket) in folders.into_iter().enumerate() {
            let name = bucket.alias_name.clone().unwrap_or_else(|| bucket.name.clone());
            list = list.child(
                h_flex()
                    .id(("folder", index))
                    .gap_3()
                    .px_2()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary))
                    .child(Icon::new(Lucide::Folder).text_color(theme.muted_foreground))
                    .child(div().text_sm().child(name))
                    .on_click(cx.listener(move |view, _, _, cx| view.open_folder(bucket.clone(), cx))),
            );
        }
        let shown = self.shown_items();
        for (index, item) in shown.iter().enumerate() {
            list = list.child(
                h_flex()
                    .id(("local-item", index))
                    .gap_3()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary))
                    .child(
                        Icon::new(if item.video_codec.is_some() { Lucide::Film } else { Lucide::Music })
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(item.title.clone()))
                            .when_some(item.artist.clone(), |column, artist| {
                                column
                                    .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(artist))
                            }),
                    )
                    .when_some(item.duration_ms, |row, duration| {
                        row.child(div().text_xs().text_color(theme.muted_foreground).child(clock(duration)))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| view.play(index, cx))),
            );
        }
        let empty = groups.is_empty() && shown.is_empty() && (self.tab != Tab::Folders || self.folders.is_empty());
        let selected = Tab::ALL.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        let tabs = TabBar::new("library-tabs")
            .underline()
            .selected_index(selected)
            .children(Tab::ALL.iter().map(|tab| Tab_::new().label(tab.label())))
            .on_click(cx.listener(|view, index: &usize, _, cx| view.select_tab(Tab::ALL[*index], cx)));
        v_flex()
            .size_full()
            .gap_3()
            .p_4()
            .child(tabs)
            .child(header)
            .when_some(self.summary.clone(), |column, summary| {
                column.child(div().text_sm().text_color(theme.muted_foreground).child(summary))
            })
            .when(empty, |column| {
                let message = t(match self.tab {
                    Tab::Folders if self.path.is_empty() => Str::LibraryEmpty,
                    Tab::Folders => Str::LibraryEmptyFolder,
                    Tab::Songs | Tab::Recent => Str::LibraryNoSongs,
                    Tab::Artists => Str::LibraryNoArtists,
                    Tab::Albums => Str::LibraryNoAlbums,
                });
                column.child(div().py_16().flex().justify_center().text_color(theme.muted_foreground).child(message))
            })
            .child(
                div()
                    .id("library-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(list),
            )
    }
}
