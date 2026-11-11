//! The local media library: `:media-store`'s schema on SQLite (buckets = folders, media items,
//! playlists) and the folder scan that reconciles a directory tree into it.

mod scan;

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};

pub use scan::{DescribedFile, ScanError, ScanReport, describe_file, is_media_file, scan_folder};

/// The YMusicV4 media-store schema after its five migrations (SQLDelight version 6), so a file
/// from the JVM app opens unchanged.
const SCHEMA_VERSION: i32 = 6;
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS bucket (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    parentId  INTEGER REFERENCES bucket(id) ON DELETE CASCADE,
    uri       TEXT    NOT NULL UNIQUE,
    name      TEXT    NOT NULL,
    aliasName TEXT,
    deleted   INTEGER NOT NULL DEFAULT 0,
    excluded  INTEGER NOT NULL DEFAULT 0,
    createdAt INTEGER NOT NULL,
    updatedAt INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS bucket_parentId ON bucket(parentId);
CREATE TABLE IF NOT EXISTS mediaItem (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    bucketId             INTEGER REFERENCES bucket(id) ON DELETE CASCADE,
    uri                  TEXT    NOT NULL UNIQUE,
    title                TEXT    NOT NULL,
    mimeType             TEXT    NOT NULL,
    artist               TEXT,
    album                TEXT,
    genre                TEXT,
    trackNumber          INTEGER,
    year                 INTEGER,
    durationMs           INTEGER,
    audioCodec           TEXT,
    videoCodec           TEXT,
    subtitleCodec        TEXT,
    bitrate              INTEGER,
    width                INTEGER,
    height               INTEGER,
    thumbnailStreamIndex INTEGER,
    youtubeVid           TEXT,
    youtubeItags         TEXT,
    fileSize             INTEGER NOT NULL,
    lastModified         INTEGER NOT NULL,
    deleted              INTEGER NOT NULL DEFAULT 0,
    createdAt            INTEGER NOT NULL,
    updatedAt            INTEGER NOT NULL,
    loudnessGainDb       REAL
);
CREATE INDEX IF NOT EXISTS mediaItem_bucketId   ON mediaItem(bucketId);
CREATE INDEX IF NOT EXISTS mediaItem_fileSize   ON mediaItem(fileSize);
CREATE INDEX IF NOT EXISTS mediaItem_updatedAt  ON mediaItem(updatedAt);
CREATE INDEX IF NOT EXISTS mediaItem_youtubeVid ON mediaItem(youtubeVid);
CREATE TABLE IF NOT EXISTS playlist (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    name      TEXT    NOT NULL,
    createdAt INTEGER NOT NULL,
    updatedAt INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS playlistItem (
    playlistId INTEGER NOT NULL REFERENCES playlist(id) ON DELETE CASCADE,
    code       TEXT    NOT NULL,
    sortKey    INTEGER NOT NULL,
    addedAt    INTEGER NOT NULL,
    PRIMARY KEY (playlistId, code)
);
CREATE INDEX IF NOT EXISTS playlistItem_order ON playlistItem(playlistId, sortKey);
";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("media store: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("media store schema {found} is newer than this build ({SCHEMA_VERSION})")]
    NewerSchema { found: i32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub uri: String,
    pub name: String,
    pub alias_name: Option<String>,
    pub excluded: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaItem {
    pub id: i64,
    pub bucket_id: Option<i64>,
    pub uri: String,
    pub title: String,
    pub mime_type: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub track_number: Option<i32>,
    pub year: Option<i32>,
    pub duration_ms: Option<i64>,
    pub audio_codec: Option<String>,
    pub video_codec: Option<String>,
    pub subtitle_codec: Option<String>,
    pub bitrate: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub thumbnail_stream_index: Option<i32>,
    pub youtube_vid: Option<String>,
    pub file_size: i64,
    pub last_modified: i64,
    pub loudness_gain_db: Option<f64>,
}

const ITEM_COLUMNS: &str = "id, bucketId, uri, title, mimeType, artist, album, genre, trackNumber, year, durationMs, \
    audioCodec, videoCodec, subtitleCodec, bitrate, width, height, thumbnailStreamIndex, youtubeVid, fileSize, \
    lastModified, loudnessGainDb";

fn item_from_row(row: &Row<'_>) -> rusqlite::Result<MediaItem> {
    Ok(MediaItem {
        id: row.get(0)?,
        bucket_id: row.get(1)?,
        uri: row.get(2)?,
        title: row.get(3)?,
        mime_type: row.get(4)?,
        artist: row.get(5)?,
        album: row.get(6)?,
        genre: row.get(7)?,
        track_number: row.get(8)?,
        year: row.get(9)?,
        duration_ms: row.get(10)?,
        audio_codec: row.get(11)?,
        video_codec: row.get(12)?,
        subtitle_codec: row.get(13)?,
        bitrate: row.get(14)?,
        width: row.get(15)?,
        height: row.get(16)?,
        thumbnail_stream_index: row.get(17)?,
        youtube_vid: row.get(18)?,
        file_size: row.get(19)?,
        last_modified: row.get(20)?,
        loudness_gain_db: row.get(21)?,
    })
}

fn bucket_from_row(row: &Row<'_>) -> rusqlite::Result<Bucket> {
    Ok(Bucket {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        uri: row.get(2)?,
        name: row.get(3)?,
        alias_name: row.get(4)?,
        excluded: row.get::<_, i64>(5)? != 0,
    })
}

/// A user-made Library playlist; its members are content codes (a YouTube id, or size and length).
#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
}

const BUCKET_COLUMNS: &str = "id, parentId, uri, name, aliasName, excluded";

fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// Every call blocks on SQLite: call off the UI and engine threads.
pub struct MediaStore {
    db: Mutex<Connection>,
}

impl MediaStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let db = Connection::open(path)?;
        db.execute_batch("PRAGMA foreign_keys = ON;")?;
        let version: i32 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema { found: version });
        }
        db.execute_batch(SCHEMA)?;
        if version < SCHEMA_VERSION {
            db.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        }
        Ok(Self { db: Mutex::new(db) })
    }

    /// The bucket for `uri`, created (or revived from a tombstone) under `parent_id`.
    pub fn upsert_bucket(&self, uri: &str, name: &str, parent_id: Option<i64>) -> Result<i64, StoreError> {
        let db = self.db.lock().unwrap();
        let now = now_millis();
        if let Some(id) =
            db.query_row("SELECT id FROM bucket WHERE uri = ?1", params![uri], |row| row.get::<_, i64>(0)).optional()?
        {
            db.execute(
                "UPDATE bucket SET parentId = ?1, name = ?2, deleted = 0, updatedAt = ?3 WHERE id = ?4",
                params![parent_id, name, now, id],
            )?;
            return Ok(id);
        }
        db.execute(
            "INSERT INTO bucket(parentId, uri, name, createdAt, updatedAt) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![parent_id, uri, name, now],
        )?;
        Ok(db.last_insert_rowid())
    }

    pub fn root_buckets(&self) -> Result<Vec<Bucket>, StoreError> {
        self.buckets("SELECT {} FROM bucket WHERE parentId IS NULL AND deleted = 0 ORDER BY name COLLATE NOCASE", [])
    }

    pub fn child_buckets(&self, parent_id: i64) -> Result<Vec<Bucket>, StoreError> {
        self.buckets(
            "SELECT {} FROM bucket WHERE parentId = ?1 AND deleted = 0 ORDER BY name COLLATE NOCASE",
            [parent_id],
        )
    }

    fn buckets<P: rusqlite::Params>(&self, sql: &str, args: P) -> Result<Vec<Bucket>, StoreError> {
        let db = self.db.lock().unwrap();
        let mut statement = db.prepare(&sql.replace("{}", BUCKET_COLUMNS))?;
        let rows = statement.query_map(args, bucket_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_bucket_deleted(&self, bucket_id: i64, deleted: bool) -> Result<(), StoreError> {
        let db = self.db.lock().unwrap();
        db.execute(
            "UPDATE bucket SET deleted = ?1, updatedAt = ?2 WHERE id = ?3",
            params![i64::from(deleted), now_millis(), bucket_id],
        )?;
        Ok(())
    }

    /// Including soft-deleted rows, so a returning file revives its item.
    pub fn find_item_by_uri(&self, uri: &str) -> Result<Option<(MediaItem, bool)>, StoreError> {
        let db = self.db.lock().unwrap();
        let sql = format!("SELECT {ITEM_COLUMNS}, deleted FROM mediaItem WHERE uri = ?1");
        Ok(db.query_row(&sql, params![uri], |row| Ok((item_from_row(row)?, row.get::<_, i64>(22)? != 0))).optional()?)
    }

    /// Inserts `item` (its `id` ignored) or rewrites the row with its `uri`, reviving it.
    pub fn save_item(&self, item: &MediaItem) -> Result<i64, StoreError> {
        let db = self.db.lock().unwrap();
        let now = now_millis();
        let changed = db.execute(
            "UPDATE mediaItem SET bucketId = ?1, title = ?2, mimeType = ?3, artist = ?4, album = ?5, genre = ?6, \
             trackNumber = ?7, year = ?8, durationMs = ?9, audioCodec = ?10, videoCodec = ?11, subtitleCodec = ?12, \
             bitrate = ?13, width = ?14, height = ?15, thumbnailStreamIndex = ?16, fileSize = ?17, \
             lastModified = ?18, youtubeVid = ?21, loudnessGainDb = ?22, deleted = 0, updatedAt = ?19 WHERE uri = ?20",
            params![
                item.bucket_id,
                item.title,
                item.mime_type,
                item.artist,
                item.album,
                item.genre,
                item.track_number,
                item.year,
                item.duration_ms,
                item.audio_codec,
                item.video_codec,
                item.subtitle_codec,
                item.bitrate,
                item.width,
                item.height,
                item.thumbnail_stream_index,
                item.file_size,
                item.last_modified,
                now,
                item.uri,
                item.youtube_vid,
                item.loudness_gain_db
            ],
        )?;
        if changed > 0 {
            return Ok(db.query_row("SELECT id FROM mediaItem WHERE uri = ?1", params![item.uri], |row| row.get(0))?);
        }
        db.execute(
            "INSERT INTO mediaItem(bucketId, uri, title, mimeType, artist, album, genre, trackNumber, year, \
             durationMs, audioCodec, videoCodec, subtitleCodec, bitrate, width, height, thumbnailStreamIndex, \
             fileSize, lastModified, createdAt, updatedAt, youtubeVid, loudnessGainDb) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?20, ?21, ?22)",
            params![
                item.bucket_id,
                item.uri,
                item.title,
                item.mime_type,
                item.artist,
                item.album,
                item.genre,
                item.track_number,
                item.year,
                item.duration_ms,
                item.audio_codec,
                item.video_codec,
                item.subtitle_codec,
                item.bitrate,
                item.width,
                item.height,
                item.thumbnail_stream_index,
                item.file_size,
                item.last_modified,
                now,
                item.youtube_vid,
                item.loudness_gain_db
            ],
        )?;
        Ok(db.last_insert_rowid())
    }

    /// Soft-deletes the bucket's items whose uri is not in `present`. Returns how many.
    pub fn delete_missing_items(&self, bucket_id: i64, present: &[String]) -> Result<usize, StoreError> {
        let db = self.db.lock().unwrap();
        let mut statement = db.prepare("SELECT id, uri FROM mediaItem WHERE bucketId = ?1 AND deleted = 0")?;
        let missing: Vec<i64> = statement
            .query_map(params![bucket_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|(_, uri)| !present.contains(uri))
            .map(|(id, _)| id)
            .collect();
        let now = now_millis();
        for id in &missing {
            db.execute("UPDATE mediaItem SET deleted = 1, updatedAt = ?1 WHERE id = ?2", params![now, id])?;
        }
        Ok(missing.len())
    }

    pub fn items_in_bucket(&self, bucket_id: i64) -> Result<Vec<MediaItem>, StoreError> {
        let db = self.db.lock().unwrap();
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM mediaItem WHERE bucketId = ?1 AND deleted = 0 \
             ORDER BY trackNumber IS NULL, trackNumber, title COLLATE NOCASE"
        );
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map(params![bucket_id], item_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn all_items(&self) -> Result<Vec<MediaItem>, StoreError> {
        let db = self.db.lock().unwrap();
        let sql = format!("SELECT {ITEM_COLUMNS} FROM mediaItem WHERE deleted = 0 ORDER BY title COLLATE NOCASE");
        let mut statement = db.prepare(&sql)?;
        let rows = statement.query_map([], item_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The YouTube video ids the Library holds a file of.
    pub fn youtube_video_ids(&self) -> Result<Vec<String>, StoreError> {
        let db = self.db.lock().unwrap();
        let mut statement =
            db.prepare("SELECT DISTINCT youtubeVid FROM mediaItem WHERE youtubeVid IS NOT NULL AND deleted = 0")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn playlists(&self) -> Result<Vec<Playlist>, StoreError> {
        let db = self.db.lock().unwrap();
        let mut statement = db.prepare("SELECT id, name FROM playlist ORDER BY id")?;
        let rows = statement.query_map([], |row| Ok(Playlist { id: row.get(0)?, name: row.get(1)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn create_playlist(&self, name: &str) -> Result<Playlist, StoreError> {
        let db = self.db.lock().unwrap();
        let now = now_millis();
        db.execute("INSERT INTO playlist(name, createdAt, updatedAt) VALUES (?1, ?2, ?2)", params![name, now])?;
        Ok(Playlist { id: db.last_insert_rowid(), name: name.to_owned() })
    }

    /// Appends `codes` in order; members already in the playlist keep their place. A YouTube id
    /// joins before its file exists and shows up once a Library item carries it.
    pub fn add_codes_to_playlist(&self, playlist_id: i64, codes: &[String]) -> Result<(), StoreError> {
        let mut db = self.db.lock().unwrap();
        let transaction = db.transaction()?;
        let now = now_millis();
        let last: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sortKey), 0) FROM playlistItem WHERE playlistId = ?1",
            params![playlist_id],
            |row| row.get(0),
        )?;
        let mut sort_key = last.max(now);
        for code in codes {
            sort_key += 1;
            transaction.execute(
                "INSERT OR IGNORE INTO playlistItem(playlistId, code, sortKey, addedAt) VALUES (?1, ?2, ?3, ?4)",
                params![playlist_id, code, sort_key, now],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}
