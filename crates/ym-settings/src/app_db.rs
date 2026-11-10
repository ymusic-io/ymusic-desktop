use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{SettingsError, SettingsStore};

/// The same tables as the JVM desktop app's `app.db`, so its file can be read on migration.
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
        let db = Connection::open(path)?;
        db.execute_batch(SCHEMA)?;
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
