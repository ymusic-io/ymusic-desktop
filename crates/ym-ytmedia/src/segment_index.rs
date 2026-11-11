//! Segment ⇄ (time range, byte range) per format stream. A download records each header as it
//! arrives and, on resume, finds the last whole segment its file holds. A hint, not the truth: a row
//! can exist for bytes a crash lost, so callers check against the file length.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::sabr::messages::MediaHeader;

const RETENTION_MILLIS: i64 = 30 * 24 * 60 * 60 * 1000;

/// `:yt-media`'s `SegmentDatabase` (version 3), the JVM app's `download-segments.db`: this index
/// reads only `segment`, the rest is the player cache's and is created so either app opens the file.
const SCHEMA_VERSION: i64 = 3;
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS blob_span (
    key       TEXT    NOT NULL,
    start     INTEGER NOT NULL,
    len       INTEGER NOT NULL,
    last_used INTEGER NOT NULL,
    PRIMARY KEY (key, start, len)
);
CREATE INDEX IF NOT EXISTS blob_span_lru ON blob_span(last_used);
CREATE TABLE IF NOT EXISTS cache_stats (
    id        INTEGER NOT NULL PRIMARY KEY,
    hit_bytes INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS segment (
    itag          INTEGER NOT NULL,
    lmt           INTEGER NOT NULL,
    seg_index     INTEGER NOT NULL,
    xtags         TEXT,
    vid           TEXT    NOT NULL,
    start_ms      INTEGER NOT NULL,
    end_ms        INTEGER NOT NULL,
    r_start       INTEGER NOT NULL,
    r_end         INTEGER NOT NULL,
    download_time INTEGER NOT NULL,
    PRIMARY KEY (itag, lmt, seg_index)
);
CREATE INDEX IF NOT EXISTS segment_start_ms ON segment(itag, lmt, start_ms);
CREATE INDEX IF NOT EXISTS segment_end_ms ON segment(itag, lmt, end_ms);
CREATE INDEX IF NOT EXISTS segment_seg ON segment(itag, lmt, seg_index);
CREATE TABLE IF NOT EXISTS format_init (
    itag  INTEGER NOT NULL,
    lmt   INTEGER NOT NULL,
    bytes BLOB    NOT NULL,
    PRIMARY KEY (itag, lmt)
);
";
const MIGRATIONS: [&str; 2] = [
    "CREATE TABLE format_init (itag INTEGER NOT NULL, lmt INTEGER NOT NULL, bytes BLOB NOT NULL, PRIMARY KEY (itag, lmt));",
    "CREATE TABLE cache_stats (id INTEGER NOT NULL PRIMARY KEY, hit_bytes INTEGER NOT NULL);",
];

/// Blocking SQLite: call from a worker, never the UI or engine thread.
pub struct SegmentIndex {
    connection: Mutex<Connection>,
}

impl SegmentIndex {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::with_connection(Connection::open(path)?)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(connection: Connection) -> rusqlite::Result<Self> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        ym_common::sqlite::open_schema(
            &connection,
            SCHEMA_VERSION,
            |db| db.execute_batch(SCHEMA),
            |db, found| ym_common::sqlite::run_migrations(db, found, &MIGRATIONS),
        )?;
        Ok(Self { connection: Mutex::new(connection) })
    }

    /// The first writer of a `(itag, lmt, seg)` wins.
    pub fn save(&self, header: &MediaHeader) -> rusqlite::Result<()> {
        // The init segment stores a degenerate window [0, -1].
        let start_ms = header.start_ms.unwrap_or(0);
        let end_ms = start_ms + header.duration_ms.unwrap_or(0) - 1;
        self.connection.lock().unwrap().execute(
            "INSERT OR IGNORE INTO segment(itag, lmt, seg_index, xtags, vid, start_ms, end_ms, r_start, r_end, download_time)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                header.itag,
                header.lmt,
                header.segment_index,
                header.xtag,
                header.video_id,
                start_ms,
                end_ms,
                header.start_range,
                header.start_range + header.content_length - 1,
                ym_common::log::now_millis(),
            ],
        )?;
        Ok(())
    }

    /// The furthest segment ending at or before `byte_index`.
    pub fn find_by_byte_offset(&self, itag: i32, lmt: i64, byte_index: i64) -> rusqlite::Result<Option<MediaHeader>> {
        self.connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT seg_index, xtags, vid, start_ms, end_ms, r_start, r_end FROM segment
                 WHERE itag = ? AND lmt = ? AND r_end <= ? ORDER BY seg_index DESC LIMIT 1",
                params![itag, lmt, byte_index],
                |row| {
                    let segment_index: i64 = row.get(0)?;
                    let (start_ms, end_ms): (i64, i64) = (row.get(3)?, row.get(4)?);
                    let (r_start, r_end): (i64, i64) = (row.get(5)?, row.get(6)?);
                    let is_init = segment_index == 0;
                    Ok(MediaHeader {
                        header_id: 0,
                        video_id: row.get(2)?,
                        itag,
                        lmt,
                        xtag: row.get(1)?,
                        start_range: r_start,
                        is_init_segment: is_init,
                        segment_index,
                        start_ms: (!is_init).then_some(start_ms),
                        duration_ms: (!is_init).then_some(end_ms - start_ms + 1),
                        content_length: r_end - r_start + 1,
                        data_offset: 0,
                    })
                },
            )
            .optional()
    }

    /// Protects a stream's rows from [`Self::cleanup`] while it downloads.
    pub fn mark_download_time(&self, itag: i32, lmt: i64) -> rusqlite::Result<()> {
        self.connection.lock().unwrap().execute(
            "UPDATE segment SET download_time = ? WHERE itag = ? AND lmt = ?",
            params![ym_common::log::now_millis(), itag, lmt],
        )?;
        Ok(())
    }

    /// Drops rows untouched for the retention window.
    pub fn cleanup(&self) -> rusqlite::Result<usize> {
        let cutoff = ym_common::log::now_millis() - RETENTION_MILLIS;
        self.connection.lock().unwrap().execute("DELETE FROM segment WHERE download_time < ?", params![cutoff])
    }
}
