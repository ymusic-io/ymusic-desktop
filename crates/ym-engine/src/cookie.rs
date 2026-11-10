//! `cookie/get` / `cookie/set` (HOST_PROTOCOLS §2). The bundle owns cookie policy and cooks every
//! request's `Cookie` header; the host owns parsing and storage.

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use url::Url;
use ym_quickjs::{BoxError, QuickJsRuntime};

use crate::logs::ParkedLoggers;
use crate::protocol::{Codes, Request, ServeError, serve};

const CODES: Codes = Codes { request: "R20006", failure: "R20007" };

/// The app's cookie store. Blocking: called off the engine thread.
pub trait CookieJar: Send + Sync {
    /// name → value pairs the store would send to `url`.
    fn load_for_request(&self, url: &Url) -> Result<Vec<(String, String)>, BoxError>;
    /// Parses and stores `Set-Cookie` values received from `url`; unparseable ones are dropped.
    fn save_from_response(&self, url: &Url, set_cookies: &[String]) -> Result<(), BoxError>;
}

/// RFC 6265 domain / path / expiry / secure matching, in memory.
#[derive(Default)]
pub struct MemoryCookieJar {
    store: Mutex<cookie_store::CookieStore>,
}

impl CookieJar for MemoryCookieJar {
    fn load_for_request(&self, url: &Url) -> Result<Vec<(String, String)>, BoxError> {
        let store = self.store.lock().unwrap();
        Ok(store.get_request_values(url).map(|(name, value)| (name.to_owned(), value.to_owned())).collect())
    }

    fn save_from_response(&self, url: &Url, set_cookies: &[String]) -> Result<(), BoxError> {
        let mut store = self.store.lock().unwrap();
        for set_cookie in set_cookies {
            let _ = store.parse(set_cookie, url);
        }
        Ok(())
    }
}

/// Writes the jar's persistent cookies somewhere durable. Blocking.
pub type CookiePersist = Box<dyn Fn(&[u8]) -> Result<(), BoxError> + Send + Sync>;

/// [`MemoryCookieJar`] semantics, saved after every change and restored at start. Session
/// cookies (no expiry) are not saved, as a browser drops them at exit.
pub struct PersistentCookieJar {
    store: Mutex<cookie_store::CookieStore>,
    persist: CookiePersist,
}

impl PersistentCookieJar {
    /// `saved` is what `persist` last wrote; unreadable bytes start an empty jar.
    pub fn new(saved: Option<&[u8]>, persist: CookiePersist) -> Self {
        let store = saved.and_then(|bytes| cookie_store::serde::json::load(bytes).ok()).unwrap_or_default();
        Self { store: Mutex::new(store), persist }
    }
}

impl CookieJar for PersistentCookieJar {
    fn load_for_request(&self, url: &Url) -> Result<Vec<(String, String)>, BoxError> {
        let store = self.store.lock().unwrap();
        Ok(store.get_request_values(url).map(|(name, value)| (name.to_owned(), value.to_owned())).collect())
    }

    fn save_from_response(&self, url: &Url, set_cookies: &[String]) -> Result<(), BoxError> {
        let mut bytes = Vec::new();
        {
            let mut store = self.store.lock().unwrap();
            for set_cookie in set_cookies {
                let _ = store.parse(set_cookie, url);
            }
            cookie_store::serde::json::save(&store, &mut bytes)?;
        }
        (self.persist)(&bytes)
    }
}

pub(crate) fn install(runtime: &QuickJsRuntime, jar: Arc<dyn CookieJar>, parked: Arc<ParkedLoggers>) {
    let (get_jar, get_parked) = (jar.clone(), parked.clone());
    runtime.on_accept("cookie/get", move |pipe| {
        let (jar, parked) = (get_jar.clone(), get_parked.clone());
        async move {
            serve(pipe, parked, "cookie/get", CODES, &["url"], async |request: &Request<'_>| {
                let url = http_url(request)?;
                let cookies = tokio::task::spawn_blocking(move || jar.load_for_request(&url))
                    .await?
                    .map_err(ServeError::Failure)?;
                let body: Map<String, Value> =
                    cookies.into_iter().map(|(name, value)| (name, Value::String(value))).collect();
                request.send_json(&Value::Object(body)).await
            })
            .await
        }
    });
    runtime.on_accept("cookie/set", move |pipe| {
        let (jar, parked) = (jar.clone(), parked.clone());
        async move {
            serve(pipe, parked, "cookie/set", CODES, &["url"], async |request: &Request<'_>| {
                let url = http_url(request)?;
                let set_cookies: Vec<String> = request
                    .head
                    .get("setCookie")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values.iter().filter_map(Value::as_str).filter(|v| !v.is_empty()).map(str::to_owned).collect()
                    })
                    .unwrap_or_default();
                if set_cookies.is_empty() {
                    return Err(ServeError::Request("cookie request missing 'setCookie'".into()));
                }
                // Names only: values are session credentials and never enter a log line.
                request.log.debug(|| {
                    let names: Vec<&str> = set_cookies.iter().filter_map(|c| c.split('=').next()).collect();
                    format!("{} set-cookie={}", url.host_str().unwrap_or_default(), names.join(","))
                });
                tokio::task::spawn_blocking(move || jar.save_from_response(&url, &set_cookies))
                    .await?
                    .map_err(ServeError::Failure)
            })
            .await
        }
    });
}

fn http_url(request: &Request<'_>) -> Result<Url, ServeError> {
    let raw = request.string("url").map_err(|_| ServeError::Request("cookie request missing 'url'".into()))?;
    Url::parse(&raw)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        .ok_or_else(|| ServeError::Request(format!("cookie request has an invalid 'url': {raw}")))
}
