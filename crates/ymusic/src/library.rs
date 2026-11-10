//! The local Library: scanned folders as a browsable tree, their media files, and the scan.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
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

pub struct LibraryView {
    store: Arc<MediaStore>,
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
        let mut view = Self {
            store,
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
        let items = self.items.iter().map(QueueItem::from_local).collect();
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
        for (index, bucket) in self.folders.clone().into_iter().enumerate() {
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
        for (index, item) in self.items.iter().enumerate() {
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
        let empty = self.folders.is_empty() && self.items.is_empty();
        v_flex()
            .size_full()
            .gap_3()
            .p_4()
            .child(header)
            .when_some(self.summary.clone(), |column, summary| {
                column.child(div().text_sm().text_color(theme.muted_foreground).child(summary))
            })
            .when(empty, |column| {
                let message = if self.path.is_empty() { t(Str::LibraryEmpty) } else { t(Str::LibraryEmptyFolder) };
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
