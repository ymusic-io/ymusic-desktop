use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{SettingsError, SettingsStore};

/// The JVM desktop app's `app.db` (`AppDatabase`, version 1): either app opens the other's file.
const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS setting (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS secret (
    name  TEXT NOT NULL PRIMARY KEY,
    value BLOB NOT NULL
);
";

/// `app.db`: settings (cached in memory, written through) and named secret blobs.
pub struct AppDb {
    db: Mutex<Connection>,
    settings: Mutex<HashMap<String, String>>,
}

impl AppDb {
    /// Blocking.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        create_owner_only(path);
        let db = Connection::open(path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        ym_common::sqlite::open_schema(&db, SCHEMA_VERSION, |db| db.execute_batch(SCHEMA), |_, _| Ok(()))?;
        let settings = {
            let mut statement = db.prepare("SELECT key, value FROM setting")?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<HashMap<_, _>>>()?
        };
        Ok(Self { db: Mutex::new(db), settings: Mutex::new(settings) })
    }

    /// Blocking.
    pub fn secret(&self, name: &str) -> rusqlite::Result<Option<Vec<u8>>> {
        self.db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM secret WHERE name = ?1", params![name], |row| row.get(0))
            .optional()
    }

    /// Blocking.
    pub fn put_secret(&self, name: &str, value: &[u8]) -> rusqlite::Result<()> {
        self.db
            .lock()
            .unwrap()
            .execute("INSERT OR REPLACE INTO secret(name, value) VALUES (?1, ?2)", params![name, value])?;
        Ok(())
    }
}

impl AppDb {
    /// Blocking.
    pub fn delete_setting(&self, key: &str) -> rusqlite::Result<()> {
        self.db.lock().unwrap().execute("DELETE FROM setting WHERE key = ?1", params![key])?;
        self.settings.lock().unwrap().remove(key);
        Ok(())
    }

    /// Blocking.
    pub fn delete_secret(&self, name: &str) -> rusqlite::Result<()> {
        self.db.lock().unwrap().execute("DELETE FROM secret WHERE name = ?1", params![name])?;
        Ok(())
    }
}

impl SettingsStore for AppDb {
    fn get_raw(&self, key: &str) -> Result<Option<String>, SettingsError> {
        Ok(self.settings.lock().unwrap().get(key).cloned())
    }

    fn put_raw(&self, key: &str, value: String) -> Result<(), SettingsError> {
        self.db
            .lock()
            .unwrap()
            .execute("INSERT OR REPLACE INTO setting(key, value) VALUES (?1, ?2)", params![key, value])?;
        self.settings.lock().unwrap().insert(key.to_owned(), value);
        Ok(())
    }
}

/// Owner read/write only, as the JVM app makes it; SQLite gives its -wal/-shm the same mode. An
/// existing file (or a race with another launch) keeps the mode it has.
fn create_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path);
    }
    #[cfg(not(unix))]
    let _ = path;
}
