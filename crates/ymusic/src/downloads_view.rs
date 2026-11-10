//! The download manager: every task with its state and progress, filtered by state, with per-task
//! and whole-queue controls. The list is the service's snapshot, kept current from its changes.

use std::path::PathBuf;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Selectable, Sizable, StyledExt, WindowExt, h_flex, v_flex};
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

fn state_label(state: DownloadState) -> &'static str {
    t(match state {
        DownloadState::Queued => Str::DownloadsStateQueued,
        DownloadState::Preparing => Str::DownloadsStatePreparing,
        DownloadState::Downloading => Str::DownloadsStateDownloading,
        DownloadState::Processing => Str::DownloadsStateProcessing,
        DownloadState::Completed => Str::DownloadsStateDone,
        DownloadState::Paused => Str::DownloadsFilterPaused,
        DownloadState::Failed => Str::DownloadsStateFailed,
        DownloadState::Canceled => Str::DownloadsStateCanceled,
    })
}

const CARD_MIN: f32 = 340.;
const THUMB: f32 = 56.;

impl DownloadsView {
    /// Deletes the finished file, then the task; a failed delete keeps the task and says so.
    fn remove_from_disk(&mut self, task: DownloadTask, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = file_path(&task) else {
            self.service.remove(&task.id);
            return;
        };
        let delete = on_core(cx, async move { tokio::task::spawn_blocking(move || std::fs::remove_file(&path)).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = delete.await;
            this.update_in(cx, |view, window, cx| match result {
                Some(Ok(Ok(()))) => view.service.remove(&task.id),
                Some(Ok(Err(error))) if error.kind() == std::io::ErrorKind::NotFound => view.service.remove(&task.id),
                Some(Ok(Err(error))) => {
                    cx.global::<AppGraph>().log.warn("R90022 downloaded file not deleted", Some(&error));
                    window.push_notification(tf(Str::ErrorGeneric, &[&"R90022"]), cx);
                }
                Some(Err(error)) => {
                    cx.global::<AppGraph>().log.warn("R90022 downloaded file not deleted", Some(&error));
                    window.push_notification(tf(Str::ErrorGeneric, &[&"R90022"]), cx);
                }
                None => {}
            })
            .ok();
        })
        .detach();
    }

    fn menu(&self, row: usize, task: &DownloadTask, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let task = task.clone();
        Button::new(("download-more", row))
            .ghost()
            .small()
            .icon(IconName::EllipsisVertical)
            .accessibility_label(t(Str::CdMore))
            .tooltip(t(Str::CdMore))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                let id = task.id.clone();
                let act = |label: &'static str, action: fn(&DownloadService, &str)| {
                    let (view, id) = (view.clone(), id.clone());
                    PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        view.update(cx, |view, _| action(&view.service, &id)).ok();
                    })
                };
                match task.state {
                    DownloadState::Queued
                    | DownloadState::Preparing
                    | DownloadState::Downloading
                    | DownloadState::Processing => {
                        menu = menu
                            .item(act(t(Str::DownloadsActionPause), DownloadService::pause))
                            .item(act(t(Str::DialogCancel), DownloadService::cancel));
                    }
                    DownloadState::Paused => {
                        menu = menu
                            .item(act(t(Str::DownloadsActionResume), DownloadService::resume))
                            .item(act(t(Str::DialogCancel), DownloadService::cancel));
                    }
                    DownloadState::Failed => {
                        menu = menu
                            .item(act(t(Str::Retry), DownloadService::retry))
                            .item(act(t(Str::MenuRemove), DownloadService::remove));
                    }
                    DownloadState::Completed => {
                        let (play_view, play_task) = (view.clone(), task.clone());
                        menu = menu.item(PopupMenuItem::new(t(Str::MenuPlay)).on_click(move |_, _, cx| {
                            play_view.update(cx, |view, cx| view.play(&play_task, cx)).ok();
                        }));
                        if let Some(path) = file_path(&task) {
                            menu = menu.item(
                                PopupMenuItem::new(crate::library::reveal_label())
                                    .on_click(move |_, _, cx| cx.reveal_path(&path)),
                            );
                        }
                        let (disk_view, disk_task) = (view.clone(), task.clone());
                        menu = menu.separator().item(act(t(Str::MenuRemove), DownloadService::remove)).item(
                            PopupMenuItem::new(t(Str::MenuRemoveFromDisk)).on_click(move |_, window, cx| {
                                disk_view
                                    .update(cx, |view, cx| view.remove_from_disk(disk_task.clone(), window, cx))
                                    .ok();
                            }),
                        );
                    }
                    DownloadState::Canceled => {
                        menu = menu.item(act(t(Str::MenuRemove), DownloadService::remove));
                    }
                }
                menu
            })
    }

    fn card(&self, row: usize, task: &DownloadTask, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let thumb = task
            .thumbnail_url
            .clone()
            .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", task.video_id));
        let active = task.state.is_active() || task.state == DownloadState::Paused;
        let fraction = task.progress();
        let percent =
            fraction.filter(|_| task.state.is_active()).map(|value| format!("{}%", (value * 100.).round() as i32));
        let show_bar = matches!(task.state, DownloadState::Preparing)
            || (matches!(task.state, DownloadState::Downloading | DownloadState::Processing | DownloadState::Paused)
                && fraction.is_some());
        let failed = task.state == DownloadState::Failed;
        let (pill_bg, pill_fg) = match task.state {
            DownloadState::Failed => (theme.danger, theme.danger_foreground),
            DownloadState::Completed => (theme.secondary, theme.foreground),
            _ if active => (theme.primary, theme.primary_foreground),
            _ => (theme.secondary, theme.muted_foreground),
        };
        let status = state_label(task.state);
        let detail: AnyElement = match task.state {
            DownloadState::Downloading | DownloadState::Paused if task.total_bytes > 0 => div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("{} / {}", size_label(task.downloaded_bytes), size_label(task.total_bytes)))
                .into_any_element(),
            DownloadState::Completed => div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(size_label(task.total_bytes))
                .into_any_element(),
            DownloadState::Failed => {
                let id = task.id.clone();
                h_flex()
                    .flex_1()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.danger)
                            .line_clamp(2)
                            .child(task.error.as_ref().map(task_error_text).unwrap_or_default()),
                    )
                    .child(
                        Button::new(("retry", row))
                            .outline()
                            .xsmall()
                            .rounded_full()
                            .label(t(Str::Retry))
                            .on_click(cx.listener(move |view, _, _, _| view.service.retry(&id))),
                    )
                    .into_any_element()
            }
            _ => div().into_any_element(),
        };
        let label =
            [task.title.clone(), status.to_owned()].into_iter().chain(percent.clone()).collect::<Vec<_>>().join(", ");
        v_flex()
            .id(("download", row))
            .role(Role::ListItem)
            .aria_label(label)
            .rounded(px(6.))
            .border_1()
            .border_color(theme.border)
            .p(px(14.))
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(
                        div()
                            .size(px(THUMB))
                            .flex_shrink_0()
                            .rounded(px(6.))
                            .overflow_hidden()
                            .bg(theme.secondary)
                            .child(img(SharedString::from(thumb)).size_full().object_fit(ObjectFit::Cover)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().font_medium().line_clamp(2).child(task.title.clone()))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .px_2()
                                            .py(px(2.))
                                            .rounded_full()
                                            .bg(pill_bg)
                                            .text_color(pill_fg)
                                            .text_xs()
                                            .child(status),
                                    )
                                    .when_some(percent, |row, percent| {
                                        row.child(div().text_xs().text_color(theme.muted_foreground).child(percent))
                                    }),
                            ),
                    )
                    .child(self.menu(row, task, cx)),
            )
            .when(show_bar, |card| {
                card.child(
                    div()
                        .pl(px(THUMB + 12.))
                        .child(Progress::new(("progress", row)).value(fraction.unwrap_or(0.) * 100.)),
                )
            })
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div().w(px(THUMB)).flex().justify_center().child(
                            div()
                                .px_1()
                                .rounded(px(3.))
                                .border_1()
                                .border_color(theme.border)
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(task.format.extension().to_uppercase()),
                        ),
                    )
                    .child(if failed { detail } else { div().flex_1().child(detail).into_any_element() }),
            )
            .into_any_element()
    }
}

impl Render for DownloadsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if self.tasks.iter().all(|task| task.state == DownloadState::Canceled) {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(Lucide::Download).size(px(56.)).text_color(theme.muted_foreground))
                .child(div().text_lg().child(t(Str::DownloadsNoneTitle)))
                .child(div().text_sm().text_color(theme.muted_foreground).child(t(Str::DownloadsNoneMessage)))
                .into_any_element();
        }
        let filter = self.filter;
        let visible: Vec<DownloadTask> = self.tasks.iter().filter(|task| filter.matches(task.state)).cloned().collect();
        let any_active = self.tasks.iter().any(|task| task.state.is_active());
        let any_paused = self.tasks.iter().any(|task| task.state == DownloadState::Paused);

        let mut chips = h_flex().id("download-filters").role(Role::TabList).gap_2().overflow_x_scroll();
        for (index, candidate) in Filter::ALL.into_iter().enumerate() {
            let count = self.tasks.iter().filter(|task| candidate.matches(task.state)).count();
            let label = if candidate == Filter::All {
                candidate.label().to_owned()
            } else {
                format!("{} {count}", candidate.label())
            };
            let selected = candidate == filter;
            let chip = Button::new(("filter", index)).small().rounded_full().label(label).selected(selected);
            let chip = if selected { chip.primary() } else { chip.outline() };
            chips = chips.child(chip.on_click(cx.listener(move |view, _, _, cx| {
                view.filter = candidate;
                cx.notify();
            })));
        }

        let width = crate::feed::feed_width(window);
        let columns = ((width - 32. + 16.) / (CARD_MIN + 16.)).floor().max(1.) as usize;
        let mut grid = v_flex().w_full().px_4().pt_2().pb(px(88.)).gap(px(12.)).child(chips);
        if visible.is_empty() {
            grid = grid
                .child(div().py_16().flex().justify_center().text_color(theme.muted_foreground).child(filter.empty()));
        }
        for (row_index, row) in visible.chunks(columns).enumerate() {
            let mut cells = h_flex().w_full().gap_4().items_start();
            for (offset, task) in row.iter().enumerate() {
                cells = cells.child(div().flex_1().min_w_0().child(self.card(row_index * columns + offset, task, cx)));
            }
            for _ in row.len()..columns {
                cells = cells.child(div().flex_1());
            }
            grid = grid.child(cells);
        }
        let fab = if any_active {
            Some((t(Str::DownloadsPauseAll), Lucide::Pause, true))
        } else if any_paused {
            Some((t(Str::DownloadsResumeAll), Lucide::Play, false))
        } else {
            None
        };
        div()
            .relative()
            .size_full()
            .child(div().id("downloads-scroll").size_full().overflow_y_scroll().track_scroll(&self.scroll).child(grid))
            .when_some(fab, |frame, (label, icon, pause)| {
                frame.child(div().absolute().right_4().bottom_4().child(
                    Button::new("downloads-fab").primary().large().rounded(px(16.)).icon(icon).label(label).on_click(
                        cx.listener(
                            move |view, _, _, _| {
                                if pause { view.service.pause_all() } else { view.service.resume_all() }
                            },
                        ),
                    ),
                ))
            })
            .into_any_element()
    }
}
