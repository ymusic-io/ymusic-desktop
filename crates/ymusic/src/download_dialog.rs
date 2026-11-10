//! The download options dialog: what a video can be saved as (audio formats and tiers, video
//! heights and containers, captions), where it goes, and the size it will take.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, StyledExt, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use ym_download::options::DownloadOptions;
use ym_download::{AudioQuality, DownloadError, DownloadFormat, DownloadRequest, DownloadService};
use ym_i18n::{Str, t, tf};

use crate::downloads::{DOWNLOAD_LOCATION_KEY, download_folder};
use crate::downloads_view::{size_label, task_error_text};
use crate::graph::{AppGraph, on_core};

/// The video to download, as the card showed it.
#[derive(Clone)]
pub struct DownloadTarget {
    pub video_id: String,
    pub title: String,
    pub artist: Option<String>,
    pub thumbnail_url: Option<String>,
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
}

impl DownloadPicker {
    /// Opens the dialog for `target`.
    pub fn open(target: DownloadTarget, window: &mut Window, cx: &mut App) {
        let picker = cx.new(|cx| DownloadPicker::new(target, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title(t(Str::DownloadTitle)).width(px(560.)).child(picker.clone())
        });
    }

    fn new(target: DownloadTarget, cx: &mut Context<Self>) -> Self {
        let graph = cx.global::<AppGraph>();
        let service = graph.downloads.clone();
        let folder = download_folder(graph.settings.as_ref());
        let probe_service = service.clone();
        let video_id = target.video_id.clone();
        let probe = on_core(cx, async move { probe_service.probe(&video_id).await });
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
                if let Ok(uri) = url::Url::from_directory_path(&folder) {
                    let graph = cx.global::<AppGraph>();
                    let (settings, log) = (graph.settings.clone(), graph.log.fork("downloads"));
                    let uri = uri.to_string();
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
        let target = self.target.clone();
        self.service.enqueue(DownloadRequest {
            video_id: target.video_id,
            title: target.title,
            artist: target.artist,
            thumbnail_url: target.thumbnail_url,
            format: self.format(),
            requested_height: self.video.then_some(self.height),
            file_name: None,
            audio_track_id: None,
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

fn format_name(format: DownloadFormat) -> &'static str {
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

fn quality_name(quality: AudioQuality) -> &'static str {
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

        if !options.captions.is_empty() {
            let current = self
                .caption
                .as_ref()
                .and_then(|key| options.captions.iter().find(|caption| caption.key() == *key))
                .map(|caption| caption_label(caption))
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
                    .disabled(self.folder.is_none())
                    .on_click(cx.listener(|picker, _, window, cx| picker.download(window, cx))),
            ),
        )
        .into_any_element()
    }
}

fn caption_label(caption: &ym_download::options::CaptionOption) -> String {
    if caption.is_translated { tf(Str::DownloadCaptionTranslated, &[&caption.name]) } else { caption.name.clone() }
}
