//! The download manager: every task with its state and progress, filtered by state, with per-task
//! and whole-queue controls. The list is the service's snapshot, kept current from its changes.

use std::path::PathBuf;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_download::{DownloadChange, DownloadService, DownloadState, DownloadTask, TaskError};
use ym_i18n::{Str, t, tf};

use crate::graph::{AppGraph, on_core};
use crate::library::PlayLocal;
use crate::playback::{QueueItem, Source};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Running,
    Waiting,
    Paused,
    Completed,
    Error,
}

impl Filter {
    const ALL: [Filter; 6] =
        [Filter::All, Filter::Running, Filter::Waiting, Filter::Paused, Filter::Completed, Filter::Error];

    fn label(self) -> &'static str {
        t(match self {
            Filter::All => Str::DownloadsFilterAll,
            Filter::Running => Str::DownloadsFilterRunning,
            Filter::Waiting => Str::DownloadsFilterWaiting,
            Filter::Paused => Str::DownloadsFilterPaused,
            Filter::Completed => Str::DownloadsFilterCompleted,
            Filter::Error => Str::DownloadsFilterError,
        })
    }

    fn empty(self) -> &'static str {
        t(match self {
            Filter::All => Str::DownloadsEmptyAll,
            Filter::Running => Str::DownloadsEmptyRunning,
            Filter::Waiting => Str::DownloadsEmptyWaiting,
            Filter::Paused => Str::DownloadsEmptyPaused,
            Filter::Completed => Str::DownloadsEmptyCompleted,
            Filter::Error => Str::DownloadsEmptyError,
        })
    }

    fn matches(self, state: DownloadState) -> bool {
        match self {
            Filter::All => state != DownloadState::Canceled,
            Filter::Running => {
                matches!(state, DownloadState::Preparing | DownloadState::Downloading | DownloadState::Processing)
            }
            Filter::Waiting => state == DownloadState::Queued,
            Filter::Paused => state == DownloadState::Paused,
            Filter::Completed => state == DownloadState::Completed,
            Filter::Error => state == DownloadState::Failed,
        }
    }
}

pub struct DownloadsView {
    service: DownloadService,
    tasks: Vec<DownloadTask>,
    filter: Filter,
    scroll: ScrollHandle,
    _changes: Task<()>,
}

impl EventEmitter<PlayLocal> for DownloadsView {}

impl DownloadsView {
    pub fn new(service: DownloadService, cx: &mut Context<Self>) -> Self {
        let (sender, mut receiver) = mpsc::unbounded::<DownloadChange>();
        let mut changes = service.subscribe();
        let forward = on_core(cx, async move {
            loop {
                match changes.recv().await {
                    Ok(change) => {
                        if sender.unbounded_send(change).is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        cx.background_executor().spawn(forward).detach();
        let changes = cx.spawn(async move |this, cx| {
            while let Some(change) = receiver.next().await {
                if this.update(cx, |view, cx| view.apply(change, cx)).is_err() {
                    return;
                }
            }
        });
        let mut view =
            Self { service, tasks: Vec::new(), filter: Filter::All, scroll: ScrollHandle::new(), _changes: changes };
        view.refresh(cx);
        view
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let service = self.service.clone();
        let load = on_core(cx, async move { service.snapshot().await });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |view, cx| {
                match result {
                    Some(Ok(mut tasks)) => {
                        tasks.reverse();
                        view.tasks = tasks;
                    }
                    Some(Err(error)) => {
                        cx.global::<AppGraph>().log.warn("the download list could not be read", Some(&error));
                    }
                    None => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply(&mut self, change: DownloadChange, cx: &mut Context<Self>) {
        let task = |tasks: &mut Vec<DownloadTask>, id: &str| tasks.iter_mut().position(|task| task.id == id);
        match change {
            DownloadChange::Added(added) => self.tasks.insert(0, *added),
            DownloadChange::Removed { id } => self.tasks.retain(|task| task.id != id),
            DownloadChange::StateChanged { id, state, error } => {
                if let Some(index) = task(&mut self.tasks, &id) {
                    self.tasks[index].state = state;
                    self.tasks[index].error = error;
                }
            }
            DownloadChange::ProgressChanged { id, done, total } => {
                if let Some(index) = task(&mut self.tasks, &id) {
                    self.tasks[index].downloaded_bytes = done;
                    self.tasks[index].total_bytes = total;
                }
            }
            DownloadChange::FormatChanged { id, format } => {
                if let Some(index) = task(&mut self.tasks, &id) {
                    self.tasks[index].format = format;
                }
            }
            DownloadChange::Completed { id, file_uri, bytes } => {
                if let Some(index) = task(&mut self.tasks, &id) {
                    let task = &mut self.tasks[index];
                    task.state = DownloadState::Completed;
                    task.file_uri = Some(file_uri);
                    task.downloaded_bytes = bytes;
                    task.total_bytes = bytes;
                    task.error = None;
                }
            }
        }
        cx.notify();
    }

    fn play(&mut self, task: &DownloadTask, cx: &mut Context<Self>) {
        let Some(path) = file_path(task) else { return };
        let media = cx.global::<AppGraph>().media.clone();
        let lookup_path = path.to_string_lossy().into_owned();
        let lookup =
            on_core(cx, async move { tokio::task::spawn_blocking(move || media.find_item_by_uri(&lookup_path)).await });
        let fallback = QueueItem {
            source: Source::Local(path),
            title: task.title.clone(),
            artist: task.artist.clone(),
            duration_ms: None,
            duration_label: None,
            thumbs: Vec::new(),
            history_code: None,
            youtube_vid: Some(task.video_id.clone()),
            loudness_gain_db: task.loudness_gain_db,
        };
        cx.spawn(async move |this, cx| {
            let item = match lookup.await {
                Some(Ok(Ok(Some((item, _))))) => QueueItem::from_local(&item),
                _ => fallback,
            };
            this.update(cx, |_, cx| cx.emit(PlayLocal { items: vec![item], index: 0 })).ok();
        })
        .detach();
    }
}

fn file_path(task: &DownloadTask) -> Option<PathBuf> {
    url::Url::parse(task.file_uri.as_deref()?).ok()?.to_file_path().ok()
}

pub fn size_label(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    if bytes >= 1e9 {
        tf(Str::DownloadSizeGb, &[&format!("{:.1}", bytes / 1e9)])
    } else if bytes >= 1e6 {
        tf(Str::DownloadSizeMb, &[&format!("{:.1}", bytes / 1e6)])
    } else {
        tf(Str::DownloadSizeKb, &[&format!("{:.0}", bytes / 1e3)])
    }
}

/// A failure as the user reads it: a category they can act on, else the code.
pub fn task_error_text(error: &TaskError) -> String {
    if let Some(message) = &error.user_message {
        return format!("{message} ({})", error.code);
    }
    match error.code.as_str() {
        "network" => t(Str::ErrorNetwork).to_owned(),
        "disk_full" => t(Str::ErrorDiskFull).to_owned(),
        "folder" => t(Str::DownloadsErrorFolderAccess).to_owned(),
        "live" => t(Str::DownloadLiveUnsupported).to_owned(),
        "processing" => t(Str::ErrorLiveStillProcessing).to_owned(),
        code => tf(Str::ErrorGeneric, &[&code]),
    }
}

fn state_label(task: &DownloadTask) -> String {
    let base = t(match task.state {
        DownloadState::Queued => Str::DownloadsStateQueued,
        DownloadState::Preparing => Str::DownloadsStatePreparing,
        DownloadState::Downloading => Str::DownloadsStateDownloading,
        DownloadState::Processing => Str::DownloadsStateProcessing,
        DownloadState::Completed => Str::DownloadsStateDone,
        DownloadState::Paused => Str::DownloadsFilterPaused,
        DownloadState::Failed => Str::DownloadsStateFailed,
        DownloadState::Canceled => Str::DownloadsStateCanceled,
    });
    let format = task.format.extension().to_uppercase();
    match task.state {
        DownloadState::Downloading if task.total_bytes > 0 => {
            format!("{base} · {} / {} · {format}", size_label(task.downloaded_bytes), size_label(task.total_bytes))
        }
        DownloadState::Completed => format!("{base} · {} · {format}", size_label(task.total_bytes)),
        DownloadState::Failed => match &task.error {
            Some(error) => format!("{base} · {}", task_error_text(error)),
            None => base.to_owned(),
        },
        _ => format!("{base} · {format}"),
    }
}

impl Render for DownloadsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let filter = self.filter;
        let visible: Vec<DownloadTask> = self.tasks.iter().filter(|task| filter.matches(task.state)).cloned().collect();
        let any_active = self.tasks.iter().any(|task| task.state.is_active());
        let any_paused = self.tasks.iter().any(|task| task.state == DownloadState::Paused);

        let mut list = v_flex().gap_1();
        for (row, task) in visible.iter().enumerate() {
            let id = task.id.clone();
            let thumb = task
                .thumbnail_url
                .clone()
                .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", task.video_id));
            let progress = match task.state {
                DownloadState::Downloading | DownloadState::Processing => task.progress(),
                _ => None,
            };
            let failed = task.state == DownloadState::Failed;
            let mut actions = h_flex().gap_1().flex_shrink_0();
            match task.state {
                DownloadState::Queued
                | DownloadState::Preparing
                | DownloadState::Downloading
                | DownloadState::Processing => {
                    let target = id.clone();
                    actions = actions.child(
                        Button::new(("pause", row))
                            .ghost()
                            .small()
                            .icon(Icon::new(Lucide::Pause))
                            .tooltip(t(Str::DownloadsActionPause))
                            .on_click(cx.listener(move |view, _, _, _| view.service.pause(&target))),
                    );
                }
                DownloadState::Paused | DownloadState::Failed => {
                    let target = id.clone();
                    actions = actions.child(
                        Button::new(("resume", row))
                            .ghost()
                            .small()
                            .icon(Icon::new(if failed { Lucide::RotateCcw } else { Lucide::Play }))
                            .tooltip(t(if failed { Str::Retry } else { Str::DownloadsActionResume }))
                            .on_click(cx.listener(move |view, _, _, _| view.service.resume(&target))),
                    );
                }
                DownloadState::Completed => {
                    let playable = task.clone();
                    actions = actions.child(
                        Button::new(("play", row))
                            .ghost()
                            .small()
                            .icon(Icon::new(Lucide::Play))
                            .tooltip(t(Str::MenuPlay))
                            .on_click(cx.listener(move |view, _, _, cx| view.play(&playable, cx))),
                    );
                    if let Some(path) = file_path(task) {
                        actions = actions.child(
                            Button::new(("reveal", row))
                                .ghost()
                                .small()
                                .icon(Icon::new(Lucide::FolderOpen))
                                .tooltip(t(Str::DownloadsActionRevealLibrary))
                                .on_click(move |_, _, cx| cx.reveal_path(&path)),
                        );
                    }
                }
                DownloadState::Canceled => {}
            }
            let target = id.clone();
            actions = actions.child(
                Button::new(("remove", row))
                    .ghost()
                    .small()
                    .icon(Icon::new(Lucide::X))
                    .tooltip(t(Str::MenuRemove))
                    .on_click(cx.listener(move |view, _, _, _| view.service.remove(&target))),
            );
            list = list.child(
                h_flex()
                    .gap_3()
                    .p_2()
                    .rounded_md()
                    .hover(|style| style.bg(theme.secondary))
                    .child(
                        div()
                            .w(px(96.))
                            .h(px(54.))
                            .flex_shrink_0()
                            .rounded_md()
                            .overflow_hidden()
                            .bg(theme.secondary)
                            .child(img(SharedString::from(thumb)).size_full().object_fit(ObjectFit::Cover)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().font_medium().truncate().child(task.title.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(if failed { theme.danger } else { theme.muted_foreground })
                                    .child(state_label(task)),
                            )
                            .when_some(progress, |column, value| {
                                column.child(Progress::new(("progress", row)).value(value * 100.))
                            }),
                    )
                    .child(actions),
            );
        }

        let selected = Filter::ALL.iter().position(|candidate| *candidate == filter).unwrap_or(0);
        v_flex()
            .size_full()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(div().text_xl().font_semibold().child(t(Str::SectionDownloads)))
                    .child(
                        h_flex()
                            .gap_2()
                            .when(any_active, |row| {
                                row.child(
                                    Button::new("pause-all")
                                        .small()
                                        .label(t(Str::DownloadsPauseAll))
                                        .on_click(cx.listener(|view, _, _, _| view.service.pause_all())),
                                )
                            })
                            .when(any_paused, |row| {
                                row.child(
                                    Button::new("resume-all")
                                        .small()
                                        .label(t(Str::DownloadsResumeAll))
                                        .on_click(cx.listener(|view, _, _, _| view.service.resume_all())),
                                )
                            }),
                    ),
            )
            .child(
                TabBar::new("download-filters")
                    .underline()
                    .selected_index(selected)
                    .children(Filter::ALL.iter().map(|filter| Tab::new().label(filter.label())))
                    .on_click(cx.listener(|view, index: &usize, _, cx| {
                        view.filter = Filter::ALL[*index];
                        cx.notify();
                    })),
            )
            .when(self.tasks.is_empty(), |column| {
                column.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(Icon::new(Lucide::Download).large().text_color(theme.muted_foreground))
                        .child(div().text_lg().child(t(Str::DownloadsNoneTitle)))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(t(Str::DownloadsNoneMessage))),
                )
            })
            .when(!self.tasks.is_empty() && visible.is_empty(), |column| {
                column.child(div().text_sm().text_color(theme.muted_foreground).child(filter.empty()))
            })
            .when(!visible.is_empty(), |column| {
                column.child(
                    div()
                        .id("downloads-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(list),
                )
            })
    }
}
