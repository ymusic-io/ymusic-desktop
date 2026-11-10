//! The app log: every line from every layer in one SQLite table (`logs.db`, the JVM app's
//! schema), the source of the feedback report and of the lines an error event takes to Sentry.

use std::collections::VecDeque;
use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use rusqlite::{Connection, params};
use ym_common::log::{Crumb, Level, LogSink, now_millis};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS log_line (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    level INTEGER NOT NULL,
    launch TEXT NOT NULL,
    trail TEXT NOT NULL,
    message TEXT NOT NULL,
    state INTEGER NOT NULL DEFAULT 0
);
";
const QUEUE_CAPACITY: usize = 4_096;
const MIN_LINES: i64 = 100;
const STATE_SENT: i64 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct LogRecord {
    pub seq: i64,
    pub epoch_ms: i64,
    pub level: i64,
    pub launch: String,
    pub trail: String,
    pub message: String,
}

struct Line {
    ts: i64,
    level: i64,
    trail: String,
    message: String,
}

struct Queue {
    lines: VecDeque<Line>,
    closed: bool,
}

/// Lines are queued without blocking (the oldest queued line drops under a burst) and written one
/// at a time by a writer thread; queries take the same connection, after the queued writes.
pub struct LogStore {
    /// This app open's id on each of its lines: 32 hex chars, so Sentry takes it as a trace id.
    pub launch: String,
    db: Arc<Mutex<Connection>>,
    queue: Arc<(Mutex<Queue>, Condvar)>,
    max_lines: Arc<AtomicI64>,
}

impl LogStore {
    /// Blocking.
    pub fn open(path: &Path) -> rusqlite::Result<Arc<Self>> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let db = Connection::open(path)?;
        db.execute_batch(SCHEMA)?;
        let launch = {
            let seed = now_millis() as u128 ^ (std::process::id() as u128) << 64;
            let hashed = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15_f39c_c060_5ced_c835);
            format!("{hashed:032x}")
        };
        let store = Arc::new(Self {
            launch,
            db: Arc::new(Mutex::new(db)),
            queue: Arc::new((Mutex::new(Queue { lines: VecDeque::new(), closed: false }), Condvar::new())),
            max_lines: Arc::new(AtomicI64::new(0)),
        });
        let (db, queue, max_lines, launch) =
            (store.db.clone(), store.queue.clone(), store.max_lines.clone(), store.launch.clone());
        std::thread::Builder::new()
            .name("log-store".into())
            .spawn(move || writer(db, queue, max_lines, launch))
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(store)
    }

    /// Keep the newest `lines`, trimmed once the table holds 10% more. Nothing trims until set.
    pub fn set_max_lines(&self, lines: i64) {
        self.max_lines.store(lines.max(MIN_LINES), Ordering::Relaxed);
    }

    /// Never blocks or fails.
    pub fn enqueue(&self, level: Level, trail: String, message: String) {
        let (queue, wake) = &*self.queue;
        let mut queue = queue.lock().unwrap();
        if queue.lines.len() >= QUEUE_CAPACITY {
            queue.lines.pop_front();
        }
        queue.lines.push_back(Line { ts: now_millis(), level: level_code(level), trail, message });
        wake.notify_one();
    }

    /// Blocking: lines after `after_seq`, oldest first, at most `limit`.
    pub fn lines_after(&self, after_seq: i64, limit: i64) -> rusqlite::Result<Vec<LogRecord>> {
        let db = self.db.lock().unwrap();
        let mut statement = db.prepare(
            "SELECT seq, ts, level, launch, trail, message FROM log_line WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after_seq, limit], record)?;
        rows.collect()
    }

    /// Blocking: this launch's newest `limit` lines not yet sent with an error event, marked sent.
    pub fn claim_unsent(&self, limit: i64) -> rusqlite::Result<Vec<LogRecord>> {
        let db = self.db.lock().unwrap();
        let mut statement = db.prepare(
            "SELECT seq, ts, level, launch, trail, message FROM log_line WHERE state = 0 AND seq IN \
             (SELECT seq FROM log_line WHERE launch = ?1 ORDER BY seq DESC LIMIT ?2) ORDER BY seq",
        )?;
        let lines: Vec<LogRecord> =
            statement.query_map(params![self.launch, limit], record)?.collect::<Result<_, _>>()?;
        for line in &lines {
            db.execute("UPDATE log_line SET state = ?1 WHERE seq = ?2", params![STATE_SENT, line.seq])?;
        }
        Ok(lines)
    }

    /// Blocking: the whole log, gzipped text, for the feedback report.
    pub fn export_gzip(&self, out: &mut impl Write) -> std::io::Result<()> {
        let mut encoder = flate2::write::GzEncoder::new(out, flate2::Compression::default());
        let mut after = 0;
        loop {
            let page = self.lines_after(after, 1_000).map_err(std::io::Error::other)?;
            let Some(last) = page.last() else { break };
            after = last.seq;
            for line in &page {
                writeln!(
                    encoder,
                    "{} {} {} [{}] {}",
                    line.epoch_ms, line.launch, line.level, line.trail, line.message
                )?;
            }
        }
        encoder.finish()?;
        Ok(())
    }
}

impl Drop for LogStore {
    fn drop(&mut self) {
        let (queue, wake) = &*self.queue;
        queue.lock().unwrap().closed = true;
        wake.notify_one();
    }
}

fn record(row: &rusqlite::Row<'_>) -> rusqlite::Result<LogRecord> {
    Ok(LogRecord {
        seq: row.get(0)?,
        epoch_ms: row.get(1)?,
        level: row.get(2)?,
        launch: row.get(3)?,
        trail: row.get(4)?,
        message: row.get(5)?,
    })
}

/// The JVM app's level ordinals: DEBUG 0, INFO 1, WARN 2, ERROR 3.
fn level_code(level: Level) -> i64 {
    match level {
        Level::Debug => 0,
        Level::Info => 1,
        Level::Warn => 2,
        Level::Error => 3,
    }
}

fn writer(db: Arc<Mutex<Connection>>, queue: Arc<(Mutex<Queue>, Condvar)>, max_lines: Arc<AtomicI64>, launch: String) {
    let (lock, wake) = &*queue;
    let mut rows: i64 =
        db.lock().unwrap().query_row("SELECT COUNT(*) FROM log_line", [], |row| row.get(0)).unwrap_or(0);
    loop {
        let batch: Vec<Line> = {
            let mut queue = lock.lock().unwrap();
            while queue.lines.is_empty() && !queue.closed {
                queue = wake.wait(queue).unwrap();
            }
            if queue.lines.is_empty() && queue.closed {
                return;
            }
            queue.lines.drain(..).collect()
        };
        let db = db.lock().unwrap();
        // A failed write loses its lines; the log is the last place to report that.
        for line in batch {
            if db
                .execute(
                    "INSERT INTO log_line (ts, level, launch, trail, message) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![line.ts, line.level, launch, line.trail, line.message],
                )
                .is_ok()
            {
                rows += 1;
            }
        }
        let cap = max_lines.load(Ordering::Relaxed);
        if cap > 0 && rows > cap + cap / 10 {
            let trimmed =
                db.execute("DELETE FROM log_line WHERE seq <= (SELECT MAX(seq) FROM log_line) - ?1", params![cap]);
            if trimmed.is_ok() {
                rows = cap;
            }
        }
    }
}

/// Writes every line to the app log.
pub struct LogStoreSink {
    pub store: Arc<LogStore>,
}

impl LogSink for LogStoreSink {
    fn log(&self, level: Level, trail: &[Crumb], message: &str, error: Option<&(dyn Error + 'static)>) {
        let path = trail.iter().map(|crumb| crumb.tag.as_str()).collect::<Vec<_>>().join("/");
        let text = match error {
            Some(error) => format!("{message}: {error}"),
            None => message.to_owned(),
        };
        self.store.enqueue(level, path, text);
    }
}
