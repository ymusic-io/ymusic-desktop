//! The download service over the app's YouTube session, ffmpeg and stores.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use futures::future::BoxFuture;
use gpui_kit::App;
use serde_json::Value;
use ym_common::log::Logger;
use ym_download::spec::ResolvedVideo;
use ym_download::streams::SpecResolver;
use ym_download::{DownloadConfig, DownloadError, DownloadService, DownloadState};
use ym_engine::CookieJar;
use ym_i18n::{Str, t};
use ym_settings::SettingsStore;
use ym_store::MediaStore;
use ym_ytmedia::sabr::SabrHttp;

use crate::graph::{AppGraph, on_core};
use crate::youtube_session::YoutubeSessions;

/// Where finished downloads go: a `file://` folder URI, shared with the JVM app's key.
pub const DOWNLOAD_LOCATION_KEY: &str = "download.location";
const MAX_CONCURRENT: usize = 3;
/// Every googlevideo request must carry the User-Agent player.js presents.
const DESKTOP_CHROME_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

struct EngineResolver {
    sessions: Arc<YoutubeSessions>,
    log: Logger,
}

impl SpecResolver for EngineResolver {
    fn resolve<'a>(&'a self, video_id: &'a str) -> BoxFuture<'a, Result<ResolvedVideo, DownloadError>> {
        Box::pin(async move {
            let engine = self.sessions.get().await?.engine.clone();
            let response = engine.player_response(video_id, &self.log).await?;
            let po_token = engine.mint_content_po_token(video_id, &self.log).await?;
            ym_download::spec::resolve(video_id, &response, Some(&po_token))
        })
    }

    fn player_response<'a>(&'a self, video_id: &'a str) -> BoxFuture<'a, Result<Value, DownloadError>> {
        Box::pin(async move {
            let engine = self.sessions.get().await?.engine.clone();
            Ok(engine.player_response(video_id, &self.log).await?)
        })
    }
}

pub struct DownloadInputs {
    pub tokio: tokio::runtime::Handle,
    pub data_dir: PathBuf,
    pub http: ym_common::http::HttpClients,
    pub cookies: Arc<dyn CookieJar>,
    pub settings: Arc<dyn SettingsStore>,
    pub media: Arc<MediaStore>,
    pub sessions: Arc<YoutubeSessions>,
    pub log: Logger,
}

/// Blocking (opens SQLite): call from a worker.
pub fn open(inputs: DownloadInputs) -> Result<DownloadService, String> {
    let log = inputs.log;
    let store = ym_download::store::DownloadStore::open(&inputs.data_dir.join("downloads.db"), log.clone())
        .map_err(|error| format!("downloads.db: {error}"))?;
    let index = ym_ytmedia::segment_index::SegmentIndex::open(&inputs.data_dir.join("download-segments.db"))
        .map_err(|error| format!("download-segments.db: {error}"))?;
    if let Err(error) = index.cleanup() {
        log.warn("old download segments could not be pruned", Some(&error));
    }
    let ffmpeg = ffmpeg_binary();
    if ffmpeg.is_none() {
        log.warn("no ffmpeg binary: downloads offer M4A only", None);
    }
    let cookies = inputs.cookies;
    let settings = inputs.settings;
    let folder_settings = settings.clone();
    let media = inputs.media;
    let completion_log = log.fork("download-library");
    let completion_tokio = inputs.tokio.clone();
    Ok(DownloadService::new(DownloadConfig {
        tokio: inputs.tokio,
        store: Arc::new(store),
        storage: Arc::new(ym_download::storage::DownloadStorage::new(inputs.data_dir.join("download-work"))),
        resolver: Arc::new(EngineResolver { sessions: inputs.sessions, log: log.fork("download-resolver") }),
        sabr: SabrHttp {
            client: inputs.http.not_following.clone(),
            user_agent: DESKTOP_CHROME_UA.to_owned(),
            cookies: Some(Arc::new(move |url| {
                let pairs = cookies.load_for_request(url).ok()?;
                (!pairs.is_empty())
                    .then(|| pairs.iter().map(|(name, value)| format!("{name}={value}")).collect::<Vec<_>>().join("; "))
            })),
        },
        client: inputs.http.following.clone(),
        ffmpeg: ffmpeg.map(ym_download::ffmpeg::Ffmpeg::new),
        segment_index: Some(Arc::new(index)),
        max_concurrent: MAX_CONCURRENT,
        folder: Arc::new(move || download_folder(folder_settings.as_ref())),
        on_completed: Some(Arc::new(move |_, result| {
            let Some(file) = url::Url::parse(&result.uri).ok().and_then(|uri| uri.to_file_path().ok()) else {
                completion_log.warn(&format!("a finished download has no file path: {}", result.uri), None);
                return;
            };
            let (media, log) = (media.clone(), completion_log.clone());
            completion_tokio.spawn_blocking(move || add_to_library(&media, &file, &log));
        })),
        log,
    }))
}

/// The chosen download folder; none until the user picks one.
pub fn download_folder(settings: &dyn SettingsStore) -> Option<PathBuf> {
    let uri = settings.get_raw(DOWNLOAD_LOCATION_KEY).ok().flatten().filter(|uri| !uri.is_empty())?;
    url::Url::parse(&uri).ok()?.to_file_path().ok()
}

/// Saves `folder` as the download location, off the UI thread.
pub fn save_download_folder(folder: &Path, cx: &App) {
    let Ok(uri) = url::Url::from_directory_path(folder) else { return };
    let graph = cx.global::<AppGraph>();
    let (settings, log, uri) = (graph.settings.clone(), graph.log.fork("downloads"), uri.to_string());
    graph.tokio.spawn_blocking(move || {
        if let Err(error) = settings.put_raw(DOWNLOAD_LOCATION_KEY, uri) {
            log.warn(&format!("the download folder could not be saved: {error}"), None);
        }
    });
}

/// The OS folder picker; a pick becomes the download location. None when canceled.
pub fn pick_download_folder(cx: &mut App) -> impl Future<Output = Option<PathBuf>> + use<> {
    let pick = rfd::AsyncFileDialog::new().set_title(t(Str::FolderPickerTitle)).pick_folder();
    let saver = cx.to_async();
    async move {
        let folder = pick.await?.path().to_path_buf();
        saver.update(|cx| save_download_folder(&folder, cx));
        Some(folder)
    }
}

/// The download folder while it still exists, else the picker first (a cancel gives none).
pub fn usable_download_folder(
    cx: &mut App,
) -> futures::future::Either<futures::future::Ready<Option<PathBuf>>, impl Future<Output = Option<PathBuf>> + use<>> {
    match download_folder(cx.global::<AppGraph>().settings.as_ref()).filter(|folder| folder.is_dir()) {
        Some(folder) => futures::future::Either::Left(futures::future::ready(Some(folder))),
        None => futures::future::Either::Right(pick_download_folder(cx)),
    }
}

/// Video ids already saved: Library files tagged with their YouTube id, and finished downloads.
pub fn offline_ids(cx: &App) -> impl Future<Output = HashSet<String>> + use<> {
    let graph = cx.global::<AppGraph>();
    let (media, service, log) = (graph.media.clone(), graph.downloads.clone(), graph.log.fork("available-offline"));
    let load = on_core(cx, async move {
        let mut ids = HashSet::new();
        match tokio::task::spawn_blocking(move || media.youtube_video_ids()).await {
            Ok(Ok(library)) => ids.extend(library),
            Ok(Err(error)) => log.warn("library video ids could not be read", Some(&error)),
            Err(error) => log.warn("library video ids could not be read", Some(&error)),
        }
        match service.snapshot().await {
            Ok(tasks) => ids.extend(
                tasks.into_iter().filter(|task| task.state == DownloadState::Completed).map(|task| task.video_id),
            ),
            Err(error) => log.warn("finished downloads could not be read", Some(&error)),
        }
        ids
    });
    async move { load.await.unwrap_or_default() }
}

/// A file in a Library folder shows up there: that folder's next scan picks it up, so run it now.
fn add_to_library(media: &MediaStore, file: &Path, log: &Logger) {
    let roots = match media.root_buckets() {
        Ok(roots) => roots,
        Err(error) => {
            log.warn("library folders could not be read for a finished download", Some(&error));
            return;
        }
    };
    let Some(root) = roots.iter().map(|bucket| PathBuf::from(&bucket.uri)).find(|root| file.starts_with(root)) else {
        return;
    };
    match ym_store::scan_folder(media, &root, &AtomicBool::new(false)) {
        Ok(report) if !report.errors.is_empty() => {
            log.warn(&format!("library scan after a download: {} files failed", report.errors.len()), None)
        }
        Ok(_) => {}
        Err(error) => log.warn("library scan after a download failed", Some(&error)),
    }
}

/// `YMUSIC_FFMPEG`, else next to the executable (packaged), else the dev build's.
fn ffmpeg_binary() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("YMUSIC_FFMPEG").map(PathBuf::from),
        std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("ffmpeg"))),
        option_env!("YM_FFMPEG_BIN").map(PathBuf::from),
    ];
    candidates.into_iter().flatten().find(|path| path.is_file())
}
