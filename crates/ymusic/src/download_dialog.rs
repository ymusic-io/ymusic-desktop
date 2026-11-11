//! The download options dialog: what a video can be saved as (audio formats and tiers, video
//! heights and containers, captions), where it goes, and the size it will take.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_download::options::DownloadOptions;
use ym_download::{AudioQuality, DownloadError, DownloadFormat, DownloadRequest, DownloadService, names};
use ym_i18n::{Str, t, tf};

use crate::downloads::{DOWNLOAD_LOCATION_KEY, download_folder, offline_ids};
use crate::downloads_view::{size_label, task_error_text};
use crate::graph::{AppGraph, on_core};
use crate::quick_download::{QuickPreset, confirm_redownload, save_preset};
use crate::spinner::Spinner;

/// The video to download, as the card showed it.
#[derive(Clone)]
pub struct DownloadTarget {
    pub video_id: String,
    pub title: String,
    pub artist: Option<String>,
    pub thumbnail_url: Option<String>,
}

impl DownloadTarget {
    pub fn from_feed(item: &ym_engine::yt::YtFeedItem) -> Self {
        Self {
            video_id: item.id.clone(),
            title: item.title.clone(),
            artist: item.channel_title.clone().or_else(|| item.subtitle_line1.clone()),
            thumbnail_url: item.thumbs.last().map(|thumb| {
                if thumb.url.starts_with("//") { format!("https:{}", thumb.url) } else { thumb.url.clone() }
            }),
        }
    }
}

enum Phase {
    Loading,
    Ready(Box<DownloadOptions>),
    Failed(String),
}

pub struct DownloadPicker {
    target: DownloadTarget,
    service: DownloadService,
    phase: Phase,
    video: bool,
    audio_format: DownloadFormat,
    quality: AudioQuality,
    height: i32,
    container: DownloadFormat,
    caption: Option<String>,
    folder: Option<PathBuf>,
    already_downloaded: bool,
    /// Also keep these picks as the Quick download preset; unticked on every open, by design.
    save_as_quick: bool,
    /// The `audioTrack.id` to download; none = the default track (or a video without dubs).
    audio_track: Option<String>,
    file_name: Entity<InputState>,
    /// The last edit dropped a character no file name may hold.
    stripped: bool,
    /// The next change is the stripped value written back, not an edit.
    stripping: bool,
    _name_changes: Subscription,
}

impl DownloadPicker {
    /// Opens the dialog for `target`.
    pub fn open(target: DownloadTarget, window: &mut Window, cx: &mut App) {
        let picker = cx.new(|cx| DownloadPicker::new(target, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title(t(Str::DownloadTitle)).width(px(560.)).child(picker.clone())
        });
    }

    fn new(target: DownloadTarget, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let stem = Some(names::from_title(&target.title)).filter(|stem| !stem.is_empty());
        let stem = stem.unwrap_or_else(|| target.video_id.clone());
        let file_name = cx.new(|cx| InputState::new(window, cx).default_value(stem));
        let name_changes = cx.subscribe_in(&file_name, window, |picker: &mut Self, input, event, window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let raw = input.read(cx).value().to_string();
            if names::has_forbidden(&raw) {
                let clean = names::strip_forbidden(&raw);
                input.update(cx, |input, cx| input.set_value(clean, window, cx));
                picker.stripped = true;
                picker.stripping = true;
            } else if !std::mem::take(&mut picker.stripping) {
                picker.stripped = false;
            }
            cx.notify();
        });
        let graph = cx.global::<AppGraph>();
        let service = graph.downloads.clone();
        let folder = download_folder(graph.settings.as_ref());
        let probe_service = service.clone();
        let video_id = target.video_id.clone();
        let probe = on_core(cx, async move { probe_service.probe(&video_id).await });
        let offline = offline_ids(cx);
        let offline_id = target.video_id.clone();
        cx.spawn(async move |this, cx| {
            let downloaded = offline.await.contains(&offline_id);
            this.update(cx, |picker, _| picker.already_downloaded = downloaded).ok();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = probe.await;
            this.update(cx, |picker, cx| {
                picker.phase = match result {
                    Some(Ok(options)) => {
                        picker.pick_defaults(&options);
                        Phase::Ready(Box::new(options))
                    }
                    Some(Err(error)) => Phase::Failed(probe_error(&error, cx)),
                    None => return,
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            target,
            service,
            phase: Phase::Loading,
            video: false,
            audio_format: DownloadFormat::M4a,
            quality: AudioQuality::Best,
            height: 0,
            container: DownloadFormat::VideoMp4,
            caption: None,
            folder,
            already_downloaded: false,
            save_as_quick: false,
            audio_track: None,
            file_name,
            stripped: false,
            stripping: false,
            _name_changes: name_changes,
        }
    }

    fn pick_defaults(&mut self, options: &DownloadOptions) {
        if !options.audio.iter().any(|option| option.format == self.audio_format)
            && let Some(first) = options.audio.first()
        {
            self.audio_format = first.format;
        }
        self.height = options
            .video
            .iter()
            .map(|option| option.height)
            .filter(|height| *height <= 720)
            .max()
            .or_else(|| options.video.last().map(|option| option.height))
            .unwrap_or(0);
        self.audio_track = options
            .audio_tracks
            .iter()
            .find(|track| track.is_default)
            .or(options.audio_tracks.first())
            .map(|track| track.id.clone());
    }

    fn finalized_name(&self, cx: &App) -> String {
        names::finalize(&self.file_name.read(cx).value())
    }

    fn format(&self) -> DownloadFormat {
        if self.video { self.container } else { self.audio_format }
    }

    fn estimated_bytes(&self, options: &DownloadOptions) -> Option<i64> {
        if self.video {
            let option = options.video.iter().find(|option| option.height == self.height)?;
            if self.container == DownloadFormat::VideoWebm { option.webm_bytes } else { Some(option.mp4_bytes) }
        } else {
            options.audio_bytes(self.audio_format, self.quality)
        }
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let pick = rfd::AsyncFileDialog::new().set_title(t(Str::FolderPickerTitle)).pick_folder();
        cx.spawn(async move |this, cx| {
            let Some(folder) = pick.await else { return };
            let folder = folder.path().to_path_buf();
            this.update(cx, |picker, cx| {
                {
                    let graph = cx.global::<AppGraph>();
                    let (settings, log) = (graph.settings.clone(), graph.log.fork("downloads"));
                    let uri = crate::downloads::folder_uri(&folder);
                    graph.tokio.spawn_blocking(move || {
                        if let Err(error) = settings.put_raw(DOWNLOAD_LOCATION_KEY, uri) {
                            log.warn(&format!("the download folder could not be saved: {error}"), None);
                        }
                    });
                }
                picker.folder = Some(folder);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn download(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.already_downloaded {
            let picker = cx.entity().downgrade();
            confirm_redownload(&self.target.title, window, cx, move |window, cx| {
                picker.update(cx, |picker, cx| picker.start(window, cx)).ok();
            });
        } else {
            self.start(window, cx);
        }
    }

    /// These picks as languages, for other videos.
    fn quick_preset(&self, options: &DownloadOptions) -> QuickPreset {
        QuickPreset {
            format: self.format(),
            height: self.video.then_some(self.height),
            audio_language: self
                .audio_track
                .as_deref()
                .filter(|_| !options.audio_tracks.is_empty())
                .map(|id| ym_download::selection::audio_track_language(id).to_owned()),
            caption_language: self
                .caption
                .as_ref()
                .and_then(|key| options.captions.iter().find(|caption| caption.key() == *key))
                .map(|caption| caption.language_code.clone()),
            audio_quality: options
                .audio
                .iter()
                .find(|option| option.format == self.audio_format)
                .and_then(|option| option.pick(self.quality))
                .map_or(self.quality, |picked| picked.quality),
        }
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.save_as_quick
            && let Phase::Ready(options) = &self.phase
        {
            save_preset(&self.quick_preset(options), cx);
        }
        let target = self.target.clone();
        self.service.enqueue(DownloadRequest {
            video_id: target.video_id,
            title: target.title,
            artist: target.artist,
            thumbnail_url: target.thumbnail_url,
            format: self.format(),
            requested_height: self.video.then_some(self.height),
            file_name: Some(self.finalized_name(cx)).filter(|name| !name.is_empty()),
            audio_track_id: self.audio_track.clone(),
            captions: Some(self.caption.iter().cloned().collect()),
            audio_language: None,
            caption_language: None,
            audio_quality: self.quality,
        });
        window.close_dialog(cx);
        window.push_notification(t(Str::DownloadAdded), cx);
    }
}

fn probe_error(error: &DownloadError, cx: &App) -> String {
    match error {
        DownloadError::Live(_) => t(Str::DownloadLiveUnsupported).to_owned(),
        DownloadError::StillProcessing(_) => t(Str::ErrorLiveStillProcessing).to_owned(),
        DownloadError::Network(_) => t(Str::ErrorNetwork).to_owned(),
        DownloadError::Engine(engine) => crate::feed::error_text(engine),
        other => {
            if !other.is_environmental() {
                cx.global::<AppGraph>().log.warn("download options failed", Some(other));
            }
            tf(
                Str::DownloadFailed,
                &[&task_error_text(&ym_download::TaskError { code: other.code(), user_message: None })],
            )
        }
    }
}

pub fn format_name(format: DownloadFormat) -> &'static str {
    match format {
        DownloadFormat::Mp3 => "MP3",
        DownloadFormat::M4a => "M4A",
        DownloadFormat::AudioWebm => "Opus",
        DownloadFormat::VideoMp4 => "MP4",
        DownloadFormat::VideoWebm => "WebM",
    }
}

fn format_detail(format: DownloadFormat) -> &'static str {
    t(match format {
        DownloadFormat::Mp3 => Str::DownloadMp3Detail,
        DownloadFormat::AudioWebm => Str::DownloadWebmDetail,
        _ => Str::DownloadM4aDetail,
    })
}

pub fn quality_name(quality: AudioQuality) -> &'static str {
    t(match quality {
        AudioQuality::Save => Str::DownloadQualitySave,
        AudioQuality::Medium => Str::DownloadQualityMedium,
        AudioQuality::Best => Str::DownloadQualityBest,
    })
}

fn section(label: &'static str, body: impl IntoElement, cx: &App) -> impl IntoElement {
    v_flex()
        .gap_1p5()
        .child(div().text_xs().font_semibold().text_color(cx.theme().muted_foreground).child(label))
        .child(body)
}

impl Render for DownloadPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let options = match &self.phase {
            Phase::Loading => {
                return h_flex()
                    .gap_2()
                    .py_6()
                    .justify_center()
                    .child(Spinner::new())
                    .child(div().text_sm().child(t(Str::DownloadLoading)))
                    .into_any_element();
            }
            Phase::Failed(message) => {
                return div().py_6().text_sm().text_color(theme.danger).child(message.clone()).into_any_element();
            }
            Phase::Ready(options) => options.clone(),
        };

        let kind = h_flex()
            .gap_2()
            .child(
                Button::new("kind-audio")
                    .small()
                    .label(t(Str::DownloadAudioOnly))
                    .when(!self.video, |button| button.primary())
                    .on_click(cx.listener(|picker, _, _, cx| {
                        picker.video = false;
                        cx.notify();
                    })),
            )
            .when(!options.video.is_empty(), |row| {
                row.child(
                    Button::new("kind-video")
                        .small()
                        .label(t(Str::DownloadVideo))
                        .when(self.video, |button| button.primary())
                        .on_click(cx.listener(|picker, _, _, cx| {
                            picker.video = true;
                            cx.notify();
                        })),
                )
            });

        let mut body = v_flex().gap_4().child(kind);
        if self.video {
            let mut heights = h_flex().gap_2().flex_wrap();
            for (index, option) in options.video.iter().enumerate() {
                let height = option.height;
                let label = match option.fps {
                    fps if fps > 30 => format!("{height}p{fps}"),
                    _ => format!("{height}p"),
                };
                heights = heights.child(
                    Button::new(("height", index))
                        .small()
                        .label(label)
                        .when(self.height == height, |button| button.primary())
                        .on_click(cx.listener(move |picker, _, _, cx| {
                            picker.height = height;
                            cx.notify();
                        })),
                );
            }
            let webm_available =
                options.video.iter().any(|option| option.height == self.height && option.webm_bytes.is_some());
            let mut containers = h_flex().gap_2();
            for (index, container) in [DownloadFormat::VideoMp4, DownloadFormat::VideoWebm].into_iter().enumerate() {
                let enabled = container == DownloadFormat::VideoMp4 || webm_available;
                containers = containers.child(
                    Button::new(("container", index))
                        .small()
                        .label(format_name(container))
                        .disabled(!enabled)
                        .when(self.container == container, |button| button.primary())
                        .on_click(cx.listener(move |picker, _, _, cx| {
                            picker.container = container;
                            cx.notify();
                        })),
                );
            }
            body = body.child(section(t(Str::DownloadQuality), heights, cx)).child(section(
                t(Str::DownloadContainer),
                containers,
                cx,
            ));
        } else {
            let mut formats = v_flex().gap_1();
            for (index, option) in options.audio.iter().enumerate() {
                let format = option.format;
                formats = formats.child(
                    h_flex()
                        .id(("audio-format", index))
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .when(self.audio_format == format, |row| row.bg(theme.secondary))
                        .hover(|style| style.bg(theme.secondary))
                        .child(div().w(px(56.)).text_sm().font_medium().child(format_name(format)))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(format_detail(format)))
                        .on_click(cx.listener(move |picker, _, _, cx| {
                            picker.audio_format = format;
                            cx.notify();
                        })),
                );
            }
            let mut tiers = h_flex().gap_2().flex_wrap();
            if let Some(option) = options.audio.iter().find(|option| option.format == self.audio_format) {
                for (index, tier) in option.qualities.iter().enumerate() {
                    let quality = tier.quality;
                    let label = match tier.kbps {
                        Some(kbps) => format!("{} · {}", quality_name(quality), tf(Str::DownloadKbps, &[&kbps])),
                        None => format!("{} · {}", quality_name(quality), size_label(tier.bytes)),
                    };
                    let selected = option.pick(self.quality).map(|picked| picked.quality) == Some(quality);
                    tiers = tiers.child(
                        Button::new(("tier", index))
                            .small()
                            .label(label)
                            .when(selected, |button| button.primary())
                            .on_click(cx.listener(move |picker, _, _, cx| {
                                picker.quality = quality;
                                cx.notify();
                            })),
                    );
                }
            }
            body = body.child(section(t(Str::DownloadFormat), formats, cx)).child(section(
                t(Str::DownloadQuality),
                tiers,
                cx,
            ));
        }

        if !options.audio_tracks.is_empty() {
            let current = self
                .audio_track
                .as_ref()
                .and_then(|id| options.audio_tracks.iter().find(|track| track.id == *id))
                .map_or_else(|| t(Str::DownloadDefaultTrack).to_owned(), |track| track.name.clone());
            let (tracks, selected) = (options.audio_tracks.clone(), self.audio_track.clone());
            let picker = cx.entity().downgrade();
            let menu = Button::new("audio-track")
                .small()
                .label(current.clone())
                .accessibility_label(format!("{}, {current}", t(Str::DownloadAudioLanguage)))
                .dropdown_menu(move |mut menu, _, _| {
                    menu = menu.scrollable(true).max_h(px(320.));
                    for track in &tracks {
                        let (target, id) = (picker.clone(), track.id.clone());
                        menu = menu.item(
                            PopupMenuItem::new(track.name.clone())
                                .checked(selected.as_deref() == Some(track.id.as_str()))
                                .on_click(move |_, _, cx| {
                                    let id = id.clone();
                                    target
                                        .update(cx, |picker, cx| {
                                            picker.audio_track = Some(id);
                                            cx.notify();
                                        })
                                        .ok();
                                }),
                        );
                    }
                    menu
                });
            body = body.child(section(t(Str::DownloadAudioLanguage), menu, cx));
        }

        if !options.captions.is_empty() {
            let current = self
                .caption
                .as_ref()
                .and_then(|key| options.captions.iter().find(|caption| caption.key() == *key))
                .map(caption_label)
                .unwrap_or_else(|| t(Str::DownloadCaptionsOff).to_owned());
            let current = if self.format().captions_as_lyrics() && self.caption.is_some() {
                tf(Str::DownloadCaptionsAsLyrics, &[&current])
            } else {
                current
            };
            let captions = options.captions.clone();
            let picker = cx.entity().downgrade();
            let menu = Button::new("captions").small().label(current).dropdown_menu(move |mut menu, _, _| {
                menu = menu.scrollable(true).max_h(px(320.));
                let off = picker.clone();
                menu = menu.item(PopupMenuItem::new(t(Str::DownloadCaptionsOff)).on_click(move |_, _, cx| {
                    off.update(cx, |picker, cx| {
                        picker.caption = None;
                        cx.notify();
                    })
                    .ok();
                }));
                for caption in &captions {
                    let key = caption.key();
                    let target = picker.clone();
                    menu = menu.item(PopupMenuItem::new(caption_label(caption)).on_click(move |_, _, cx| {
                        let key = key.clone();
                        target
                            .update(cx, |picker, cx| {
                                picker.caption = Some(key);
                                cx.notify();
                            })
                            .ok();
                    }));
                }
                menu
            });
            body = body.child(section(t(Str::DownloadCaptions), menu, cx));
        }

        let name_empty = self.finalized_name(cx).is_empty();
        let name_hint = if name_empty {
            t(Str::DownloadNameEmpty).to_owned()
        } else {
            tf(Str::DownloadForbiddenHint, &[&names::FORBIDDEN_DISPLAY])
        };
        body = body.child(section(
            t(Str::DownloadFileName),
            v_flex()
                .gap_1()
                .child(Input::new(&self.file_name).small().suffix(
                    div().text_sm().text_color(theme.muted_foreground).child(format!(".{}", self.format().extension())),
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(if name_empty || self.stripped { theme.danger } else { theme.muted_foreground })
                        .child(name_hint),
                ),
            cx,
        ));

        let folder_label = self
            .folder
            .as_ref()
            .map(|folder| folder.display().to_string())
            .unwrap_or_else(|| t(Str::DownloadLocationNotSet).to_owned());
        body = body.child(section(
            t(Str::DownloadSaveTo),
            h_flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().min_w_0().text_sm().truncate().child(folder_label))
                .child(
                    Button::new("folder")
                        .small()
                        .ghost()
                        .label(t(Str::MenuDownloadLocation))
                        .on_click(cx.listener(|picker, _, _, cx| picker.choose_folder(cx))),
                ),
            cx,
        ));

        body = body.child(
            Checkbox::new("save-as-quick").checked(self.save_as_quick).label(t(Str::DownloadSaveQuick)).on_click(
                cx.listener(|picker, checked: &bool, _, cx| {
                    picker.save_as_quick = *checked;
                    cx.notify();
                }),
            ),
        );

        let size = self.estimated_bytes(&options).map(size_label);
        let label = match size {
            Some(size) => tf(Str::DownloadActionDetail, &[&format_name(self.format()), &size]),
            None => t(Str::DownloadAction).to_owned(),
        };
        body.child(
            h_flex().justify_end().child(
                Button::new("download")
                    .primary()
                    .label(label)
                    .disabled(self.folder.is_none() || name_empty)
                    .on_click(cx.listener(|picker, _, window, cx| picker.download(window, cx))),
            ),
        )
        .into_any_element()
    }
}

fn caption_label(caption: &ym_download::options::CaptionOption) -> String {
    if caption.is_translated { tf(Str::DownloadCaptionTranslated, &[&caption.name]) } else { caption.name.clone() }
}
