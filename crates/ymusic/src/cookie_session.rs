//! What one launch runs on: the cookie jar, and the engine cache that goes with whoever owns it.
//! A guest's cache is its own and is removed at quit, so a guest and the signed-in user never read
//! each other's cached responses. The JVM app's `CookieSession` / `openCookieStore`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ym_common::log::Logger;
use ym_engine::{CookieJar, MemoryCookieJar, PersistentCookieJar};
use ym_settings::AppDb;
use ym_signin::{CookieVault, PasswordSeal, VaultError};

/// The sealed jar's row: the JVM app's name for it.
const COOKIE_JAR_SECRET: &str = "api-cookies.json";
/// Where builds before the vault kept the jar in the clear; removed at boot.
const PLAIN_COOKIES_SECRET: &str = "cookies";
/// This app's own Keychain item for the cookie key.
const KEYCHAIN_SERVICE: &str = "io.ymusic.desktop.rust";
const KEYCHAIN_ACCOUNT: &str = "cookie-key";
/// Where the key lives when no OS key store holds it (the JVM app's Linux fallback name).
const FALLBACK_KEY_SECRET: &str = "cookie-key.bin";
pub const ENGINE_CACHE_DIR: &str = "engine-cache";
/// The macOS Keychain item is bound to this app's signature, so a password adds nothing there.
const KEY_IN_KEYCHAIN: bool = cfg!(target_os = "macos") && !cfg!(debug_assertions);

#[derive(Debug, thiserror::Error)]
pub enum CookieStoreError {
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("app.db: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("guest cache: {0}")]
    Io(#[from] std::io::Error),
    #[error("no sealed cookie jar to unlock")]
    Missing,
}

/// The jar's row in app.db and the vault that seals it. Writes and [`SealedJar::protect`] share one
/// lock, so a write that raced a protect never leaves the jar under the weaker key.
pub struct SealedJar {
    app_db: Arc<AppDb>,
    vault: CookieVault,
    lock: Mutex<()>,
}

impl SealedJar {
    fn new(app_db: Arc<AppDb>, vault: CookieVault) -> Self {
        Self { app_db, vault, lock: Mutex::new(()) }
    }

    fn write(&self, document: &[u8]) -> rusqlite::Result<()> {
        let _guard = self.lock.lock().unwrap();
        self.app_db.put_secret(COOKIE_JAR_SECRET, &self.vault.seal(document))
    }

    pub fn is_password_protected(&self) -> bool {
        self.vault.is_password_protected()
    }

    /// Every later write, and the stored jar now, goes under `password`. Slow (PBKDF2) and blocking.
    pub fn protect(&self, password: &str) -> Result<(), CookieStoreError> {
        let seal = PasswordSeal::new(password);
        let _guard = self.lock.lock().unwrap();
        let document = match self.app_db.secret(COOKIE_JAR_SECRET)? {
            Some(sealed) => Some(self.vault.open(&sealed)?),
            None => None,
        };
        self.vault.protect(seal);
        if let Some(document) = document {
            self.app_db.put_secret(COOKIE_JAR_SECRET, &self.vault.seal(&document))?;
        }
        Ok(())
    }
}

pub struct CookieSession {
    pub jar: Arc<dyn CookieJar>,
    /// `None` for a guest: nothing of it is kept, so there is nothing to protect.
    pub sealed: Option<Arc<SealedJar>>,
    /// Root of the session's engine cache.
    pub cache_root: PathBuf,
}

impl CookieSession {
    pub fn is_guest(&self) -> bool {
        self.sealed.is_none()
    }

    /// The "protect with a password" step is offered where the default key is one every app of
    /// this user can recover.
    pub fn can_protect(&self) -> bool {
        !KEY_IN_KEYCHAIN && self.sealed.as_ref().is_some_and(|sealed| !sealed.is_password_protected())
    }

    fn user(app_db: Arc<AppDb>, vault: CookieVault, document: Option<&[u8]>, data_dir: &Path) -> Self {
        let sealed = Arc::new(SealedJar::new(app_db, vault));
        let writer = sealed.clone();
        let jar = PersistentCookieJar::new(document, Box::new(move |bytes| Ok(writer.write(bytes)?)));
        Self { jar: Arc::new(jar), sealed: Some(sealed), cache_root: data_dir.to_owned() }
    }

    fn guest() -> Result<Self, CookieStoreError> {
        let root = std::env::temp_dir().join(format!("ymusic-guest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root)?;
        Ok(Self { jar: Arc::new(MemoryCookieJar::default()), sealed: None, cache_root: root })
    }

    /// A guest's cache dir, removed at quit.
    pub fn guest_cache(&self) -> Option<PathBuf> {
        self.is_guest().then(|| self.cache_root.clone())
    }
}

/// How the cookie store opened at launch.
pub enum CookieOpen {
    Ready(CookieSession),
    /// Password-sealed: the launch waits for [`LockedJar::unlock`], [`LockedJar::as_guest`] or
    /// [`LockedJar::sign_out`].
    Locked(Arc<LockedJar>),
}

pub struct LockedJar {
    sealed: Arc<SealedJar>,
    data_dir: PathBuf,
    user_key: Option<[u8; 32]>,
}

impl LockedJar {
    /// `None` when `password` does not open the jar. Slow (PBKDF2) and blocking.
    pub fn unlock(&self, password: &str) -> Result<Option<CookieSession>, CookieStoreError> {
        let stored = self.sealed.app_db.secret(COOKIE_JAR_SECRET)?.ok_or(CookieStoreError::Missing)?;
        if !self.sealed.vault.unlock(&stored, password)? {
            return Ok(None);
        }
        let document = self.sealed.vault.open(&stored)?;
        let writer = self.sealed.clone();
        let jar = PersistentCookieJar::new(Some(&document), Box::new(move |bytes| Ok(writer.write(bytes)?)));
        Ok(Some(CookieSession {
            jar: Arc::new(jar),
            sealed: Some(self.sealed.clone()),
            cache_root: self.data_dir.clone(),
        }))
    }

    pub fn as_guest(&self) -> Result<CookieSession, CookieStoreError> {
        CookieSession::guest()
    }

    /// Forgets the sealed sign-in and the cache made under it, then starts signed out. Blocking.
    pub fn sign_out(&self) -> Result<CookieSession, CookieStoreError> {
        self.sealed.app_db.delete_secret(COOKIE_JAR_SECRET)?;
        match std::fs::remove_dir_all(self.data_dir.join(ENGINE_CACHE_DIR)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        match self.user_key {
            Some(key) => {
                Ok(CookieSession::user(self.sealed.app_db.clone(), CookieVault::new(key), None, &self.data_dir))
            }
            None => CookieSession::guest(),
        }
    }
}

/// The OS user's key in release builds. Debug builds are re-signed by every build, which macOS
/// treats as another app: a Keychain read would stop each launch on an access prompt, so they keep
/// the key in app.db, which only keeps other OS users out (the JVM app's Linux fallback too).
fn cookie_key(app_db: &AppDb) -> Result<[u8; 32], VaultError> {
    if !cfg!(debug_assertions) && !cfg!(target_os = "linux") {
        return ym_signin::os_cookie_key(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT);
    }
    if let Some(key) =
        app_db.secret(FALLBACK_KEY_SECRET).ok().flatten().and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
    {
        return Ok(key);
    }
    let key = ym_signin::random_key();
    app_db.put_secret(FALLBACK_KEY_SECRET, &key).map_err(|error| VaultError::KeyStore(error.to_string()))?;
    Ok(key)
}

/// The persistent jar sealed in app.db under the OS user's key. Without that key the launch runs
/// as a guest: signing in is off and nothing is written that a later launch could read. A
/// password-sealed jar never needs the OS key: every write after unlock is password-sealed.
/// Blocking.
pub fn open(app_db: Arc<AppDb>, data_dir: &Path, log: &Logger) -> Result<CookieOpen, CookieStoreError> {
    let log = log.fork("cookies");
    let _ = app_db.delete_secret(PLAIN_COOKIES_SECRET);
    let user_key = match cookie_key(&app_db) {
        Ok(key) => Some(key),
        Err(error) => {
            log.warn("R90013 OS cookie key unavailable, this launch runs as a guest", Some(&error));
            None
        }
    };
    let stored = app_db.secret(COOKIE_JAR_SECRET)?;
    if stored.as_deref().is_some_and(CookieVault::is_password_sealed) {
        let vault = CookieVault::new(user_key.unwrap_or_else(ym_signin::random_key));
        return Ok(CookieOpen::Locked(Arc::new(LockedJar {
            sealed: Arc::new(SealedJar::new(app_db, vault)),
            data_dir: data_dir.to_owned(),
            user_key,
        })));
    }
    let Some(key) = user_key else { return Ok(CookieOpen::Ready(CookieSession::guest()?)) };
    let vault = CookieVault::new(key);
    let document = match stored.as_deref().map(|sealed| vault.open(sealed)) {
        Some(Ok(document)) => Some(document),
        Some(Err(error)) => {
            log.warn("R90014 sealed cookie jar unreadable, starting signed out", Some(&error));
            None
        }
        None => None,
    };
    Ok(CookieOpen::Ready(CookieSession::user(app_db, vault, document.as_deref(), data_dir)))
}
