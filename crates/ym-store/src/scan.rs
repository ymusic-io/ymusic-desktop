//! Reconciles a directory tree into the store: each directory is a bucket, each media file an
//! item; unchanged files (same size and mtime) are not probed again, vanished ones soft-delete.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use crate::{MediaItem, MediaStore, StoreError};

/// What the bundled ffmpeg demuxes: the Open With types of the JVM app.
const MEDIA_EXTENSIONS: &[&str] = &[
    "3g2", "3gp", "aac", "avi", "flac", "flv", "m2v", "m4a", "m4b", "m4v", "mkv", "mov", "mp2", "mp3", "mp4", "mpeg",
    "mpg", "oga", "ogg", "ogv", "opus", "wav", "webm", "wmv", "wv",
];

pub fn is_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| MEDIA_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

#[derive(Debug)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
}

impl ScanReport {
    pub fn scanned(&self) -> usize {
        self.added + self.updated + self.unchanged + self.errors.len()
    }
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub root_bucket: Option<i64>,
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub errors: Vec<ScanError>,
}

fn mime_type(path: &Path, has_video: bool) -> String {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    let kind = if has_video { "video" } else { "audio" };
    let subtype = match extension.as_str() {
        "mp3" => "mpeg",
        "m4a" | "m4b" | "mp4" | "m4v" | "3gp" | "3g2" => "mp4",
        "mkv" => "x-matroska",
        "webm" => "webm",
        "ogg" | "oga" | "ogv" | "opus" => "ogg",
        "flac" => "flac",
        "wav" => "wav",
        "mov" => "quicktime",
        "avi" => "x-msvideo",
        "aac" => "aac",
        other => other,
    };
    format!("{kind}/{subtype}")
}

/// Blocking and long: run on a worker. `cancel` stops between files; per-file failures are
/// collected in the report, a store failure aborts the scan.
pub fn scan_folder(store: &MediaStore, root: &Path, cancel: &AtomicBool) -> Result<ScanReport, StoreError> {
    let mut report = ScanReport::default();
    let root_name =
        root.file_name().map_or_else(|| root.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
    let root_id = store.upsert_bucket(&root.to_string_lossy(), &root_name, None)?;
    report.root_bucket = Some(root_id);
    let mut pending = vec![(root.to_path_buf(), root_id)];
    while let Some((directory, bucket_id)) = pending.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                report.errors.push(ScanError { path: directory.clone(), message: error.to_string() });
                continue;
            }
        };
        let mut present = Vec::new();
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(metadata) = entry.metadata() else { continue };
            if metadata.is_dir() {
                let child = store.upsert_bucket(&path.to_string_lossy(), &name, Some(bucket_id))?;
                pending.push((path, child));
                continue;
            }
            if !is_media_file(&path) {
                continue;
            }
            let uri = path.to_string_lossy().into_owned();
            present.push(uri.clone());
            let size = metadata.len() as i64;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |elapsed| elapsed.as_millis() as i64);
            let existing = store.find_item_by_uri(&uri)?;
            if let Some((item, deleted)) = &existing
                && !deleted
                && item.file_size == size
                && item.last_modified == modified
                && item.bucket_id == Some(bucket_id)
            {
                report.unchanged += 1;
                continue;
            }
            match ym_mse::probe(&path) {
                Ok(probe) if probe.has_audio || probe.video_codec.is_some() => {
                    let fallback_title = path
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_else(|| name.clone());
                    store.save_item(&MediaItem {
                        bucket_id: Some(bucket_id),
                        title: probe.title.clone().unwrap_or(fallback_title),
                        mime_type: mime_type(&path, probe.video_codec.is_some()),
                        artist: probe.artist,
                        album: probe.album,
                        genre: probe.genre,
                        track_number: probe.track_number,
                        year: probe.year,
                        duration_ms: probe.duration_ms,
                        audio_codec: probe.audio_codec,
                        video_codec: probe.video_codec,
                        subtitle_codec: probe.subtitle_codec,
                        bitrate: probe.bitrate,
                        width: probe.width,
                        height: probe.height,
                        thumbnail_stream_index: probe.thumbnail_stream,
                        youtube_vid: probe
                            .composers
                            .iter()
                            .find_map(|composer| video_id_from_composer(composer))
                            .or_else(|| video_id_from_file_name(&name)),
                        loudness_gain_db: probe.composers.iter().find_map(|composer| gain_from_composer(composer)),
                        file_size: size,
                        last_modified: modified,
                        uri,
                        ..MediaItem::default()
                    })?;
                    if existing.is_some() {
                        report.updated += 1;
                    } else {
                        report.added += 1;
                    }
                }
                Ok(_) => report.errors.push(ScanError { path, message: "no audio or video stream".into() }),
                Err(code) => report.errors.push(ScanError { path, message: format!("libav error {code}") }),
            }
        }
        report.removed += store.delete_missing_items(bucket_id, &present)?;
    }
    Ok(report)
}

fn is_video_id(text: &str) -> bool {
    text.len() == 11 && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Downloads tag `composer` as `videoId` or `videoId|gain=-6.69`.
fn video_id_from_composer(composer: &str) -> Option<String> {
    let id = composer.split('|').next()?;
    is_video_id(id).then(|| id.to_owned())
}

fn gain_from_composer(composer: &str) -> Option<f64> {
    let (id, gain) = composer.split_once("|gain=")?;
    if !is_video_id(id) {
        return None;
    }
    let digits = gain.strip_prefix('-').unwrap_or(gain);
    let valid = !digits.is_empty()
        && digits.split('.').count() <= 2
        && digits.split('.').all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    if valid { gain.parse().ok() } else { None }
}

/// Files named like `Title [videoId].ext`, optionally with a `(1)`-style suffix.
fn video_id_from_file_name(name: &str) -> Option<String> {
    let close = name.rfind(']')?;
    let open = name[..close].rfind('[')?;
    let id = &name[open + 1..close];
    let rest = &name[close + 1..];
    let stem_rest = rest.rsplit_once('.').map_or(rest, |(before, _)| before);
    let suffix_ok = stem_rest.trim().chars().all(|c| c.is_ascii_digit() || "-_()[]".contains(c));
    (is_video_id(id) && suffix_ok).then(|| id.to_owned())
}
