//! The data dir holds the JVM app's files under the JVM app's names, so a release build takes over
//! `~/.ymusic` in place and the JVM app still opens it after a rollback. Databases this app's
//! earlier builds kept under other names are renamed once at boot, before anything opens them.

use std::path::{Path, PathBuf};

use ym_common::log::Logger;
use ym_settings::{AppDb, SettingsStore};

/// The JVM app's data dir, which a release build uses as its own.
pub fn release_data_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(std::env::temp_dir).join(".ymusic")
}

const RENAMED: [(&str, &str); 4] = [
    ("media_store.db", "media-store.db"),
    ("play_history.db", "play-history.db"),
    ("sponsor_block.db", "sponsor-block.db"),
    ("engine-cache/index.db", "engine-cache/engine-cache.db"),
];
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];
/// Caches earlier builds kept where the JVM app keeps nothing; refilled under the JVM app's names.
const OBSOLETE_DIRS: [&str; 1] = ["bundles"];

/// Blocking. Runs only in the instance that holds the data dir, so nothing has the files open.
pub fn adopt_jvm_names(data_dir: &Path, log: &Logger) {
    let log = log.fork("data-files");
    for (old_name, new_name) in RENAMED {
        let (old, new) = (data_dir.join(old_name), data_dir.join(new_name));
        if !old.exists() {
            continue;
        }
        let outcome = if new.exists() {
            log.info(&format!("{new_name} already exists, removing the older {old_name}"));
            remove_database(&old)
        } else {
            rename_database(&old, &new)
        };
        match outcome {
            Ok(()) => log.info(&format!("{old_name} is now {new_name}")),
            Err(error) => log.warn(&format!("R90200 {old_name} could not become {new_name}"), Some(&error)),
        }
    }
    for name in OBSOLETE_DIRS {
        match std::fs::remove_dir_all(data_dir.join(name)) {
            Ok(()) => log.info(&format!("removed {name}, a cache the JVM app keeps elsewhere")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log.warn(&format!("R90404 the obsolete {name} folder could not be removed"), Some(&error)),
        }
    }
}

/// Folds the write-ahead log into the file first, so the file moves alone and no frame is left
/// behind under the old name.
fn rename_database(old: &Path, new: &Path) -> std::io::Result<()> {
    rusqlite::Connection::open(old)
        .and_then(|connection| {
            connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            connection.close().map_err(|(_, error)| error)
        })
        .map_err(std::io::Error::other)?;
    for suffix in SIDECARS {
        remove_if_exists(&with_suffix(new, suffix))?;
    }
    std::fs::rename(old, new)?;
    for suffix in SIDECARS {
        remove_if_exists(&with_suffix(old, suffix))?;
    }
    Ok(())
}

fn remove_database(path: &Path) -> std::io::Result<()> {
    remove_if_exists(path)?;
    for suffix in SIDECARS {
        remove_if_exists(&with_suffix(path, suffix))?;
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Settings earlier builds kept under their own keys, moved to the JVM app's: `(old, new)`.
const RENAMED_SETTINGS: [(&str, &str); 1] = [("playback.volume", "player.volume")];

/// Blocking.
pub fn adopt_jvm_settings(app_db: &AppDb, log: &Logger) {
    for (old, new) in RENAMED_SETTINGS {
        let Ok(Some(value)) = app_db.get_raw(old) else { continue };
        let moved = match app_db.get_raw(new) {
            Ok(Some(_)) => Ok(()),
            _ => app_db.put_raw(new, value),
        }
        .and_then(|()| Ok(app_db.delete_setting(old)?));
        if let Err(error) = moved {
            log.warn(&format!("R90203 setting {old} could not move to {new}"), Some(error.as_ref()));
        }
    }
}
