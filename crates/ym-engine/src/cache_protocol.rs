//! `cache/get|put|delete|list` (HOST_PROTOCOLS §3) and `log/capture` on [`DiskCacheStore`].

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt;
use ym_quickjs::{PIPE_CAP, Pipe, PipeError, QuickJsRuntime};

use crate::cache::{CacheError, CacheMeta, DiskCacheStore, PutError};
use crate::error::WireError;
use crate::logs::ParkedLoggers;
use crate::protocol::{Codes, Request, ServeError, serve};

const CODES: Codes = Codes { request: "R20008", failure: "R20009" };
const CAPTURE: Codes = Codes { request: "R20016", failure: "R20016" };
const PUT_IDLE_TIMEOUT_MS: u64 = 30_000;

pub(crate) fn install(runtime: &QuickJsRuntime, store: Arc<DiskCacheStore>, parked: Arc<ParkedLoggers>) {
    let (get_store, get_parked) = (store.clone(), parked.clone());
    runtime.on_accept("cache/get", move |pipe| {
        let (store, parked) = (get_store.clone(), get_parked.clone());
        async move {
            serve(pipe, parked, "cache/get", CODES, &["scope", "key"], async |request: &Request<'_>| {
                let (scope, key) = (request.string("scope")?, request.string("key")?);
                let entry = tokio::task::spawn_blocking(move || store.get(&scope, &key)).await?.map_err(failure)?;
                let Some(entry) = entry else {
                    return request.send_json(&json!({ "hit": false })).await;
                };
                request.send_json(&meta_json(&entry.meta, Some(true))).await?;
                let mut file = tokio::fs::File::open(&entry.path).await.map_err(|error| failure(error.into()))?;
                let mut buffer = BytesMut::with_capacity(PIPE_CAP);
                loop {
                    buffer.clear();
                    let read = file.read_buf(&mut buffer).await.map_err(|error| failure(error.into()))?;
                    if read == 0 {
                        return Ok(());
                    }
                    request.pipe.send(buffer.split().freeze()).await?;
                }
            })
            .await
        }
    });

    let (put_store, put_parked) = (store.clone(), parked.clone());
    runtime.on_accept("cache/put", move |pipe| {
        let (store, parked) = (put_store.clone(), put_parked.clone());
        async move {
            serve(pipe, parked, "cache/put", CODES, &["scope", "key"], async |request: &Request<'_>| {
                let (scope, key) = (request.string("scope")?, request.string("key")?);
                let custom: BTreeMap<String, String> = request
                    .head
                    .get("meta")
                    .and_then(Value::as_object)
                    .map(|meta| meta.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
                    .unwrap_or_default();
                let idle_ms = request.head.get("idleTimeoutMs").and_then(Value::as_u64).unwrap_or(PUT_IDLE_TIMEOUT_MS);
                let body = body_stream(request.pipe, request.trailing.clone().freeze(), idle_ms);
                let meta = store.put(&scope, &key, custom, body).await.map_err(|error| match error {
                    PutError::Body(error) => error,
                    PutError::Cache(error) => failure(error),
                })?;
                request.send_json(&meta_json(&meta, None)).await
            })
            .await
        }
    });

    let (delete_store, delete_parked) = (store.clone(), parked.clone());
    runtime.on_accept("cache/delete", move |pipe| {
        let (store, parked) = (delete_store.clone(), delete_parked.clone());
        async move {
            serve(pipe, parked, "cache/delete", CODES, &["scope", "key"], async |request: &Request<'_>| {
                let (scope, key) = (request.string("scope")?, request.string("key")?);
                tokio::task::spawn_blocking(move || store.delete(&scope, &key)).await?.map_err(failure)
            })
            .await
        }
    });

    let (list_store, list_parked) = (store.clone(), parked.clone());
    runtime.on_accept("cache/list", move |pipe| {
        let (store, parked) = (list_store.clone(), list_parked.clone());
        async move {
            serve(pipe, parked, "cache/list", CODES, &["scope"], async |request: &Request<'_>| {
                let scope = request.string("scope")?;
                let listed = tokio::task::spawn_blocking(move || store.list(&scope)).await?.map_err(failure)?;
                request.send_json(&Value::Array(listed.iter().map(|meta| meta_json(meta, None)).collect())).await
            })
            .await
        }
    });
}

/// The put body: bytes already read past the head, then the pipe to its END. A writer RESET drops
/// the staged value; a body silent for `idle_ms` (0 = never) fails `R20010`.
fn body_stream(
    pipe: &Pipe,
    already: Bytes,
    idle_ms: u64,
) -> impl futures::Stream<Item = Result<Bytes, ServeError>> + '_ {
    let first = (!already.is_empty()).then_some(already);
    futures::stream::unfold((pipe, first, false), move |(pipe, first, done)| async move {
        if done {
            return None;
        }
        if let Some(chunk) = first {
            return Some((Ok(chunk), (pipe, None, false)));
        }
        let received = if idle_ms == 0 {
            pipe.receive(PIPE_CAP).await
        } else {
            match tokio::time::timeout(Duration::from_millis(idle_ms), pipe.receive(PIPE_CAP)).await {
                Ok(received) => received,
                Err(_) => {
                    let wire = WireError::new("R20010", format!("cache put body idle for {idle_ms}ms"));
                    return Some((Err(ServeError::Coded(Box::new(wire))), (pipe, None, true)));
                }
            }
        };
        match received {
            Ok(Some(chunk)) => Some((Ok(chunk), (pipe, None, false))),
            Ok(None) => None,
            Err(PipeError::Reset(_)) => Some((Err(ServeError::Hangup), (pipe, None, true))),
            Err(error) => Some((Err(ServeError::from(error)), (pipe, None, true))),
        }
    })
}

fn failure(error: CacheError) -> ServeError {
    match error {
        CacheError::Superseded(_) => ServeError::Coded(Box::new(WireError::new("R20011", error.to_string()))),
        other => ServeError::Failure(other.into()),
    }
}

fn meta_json(meta: &CacheMeta, hit: Option<bool>) -> Value {
    let mut body = Map::new();
    if let Some(hit) = hit {
        body.insert("hit".into(), json!(hit));
    }
    body.insert("key".into(), json!(meta.key));
    body.insert("size".into(), json!(meta.size));
    body.insert("updatedAt".into(), json!(meta.updated_at));
    body.insert("lastUsed".into(), json!(meta.last_used));
    body.insert("meta".into(), json!(meta.custom));
    Value::Object(body)
}

#[derive(Debug)]
struct CapturedFailure {
    code: String,
    message: String,
}

impl fmt::Display for CapturedFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for CapturedFailure {}

/// The bundle reporting a coded failure it declared dead (e.g. a PO token rejection) as the job's
/// one crash record.
pub(crate) fn install_capture(runtime: &QuickJsRuntime, parked: Arc<ParkedLoggers>) {
    runtime.on_accept("log/capture", move |pipe| {
        let parked = parked.clone();
        async move {
            let root = parked.root().clone();
            serve(pipe, parked, "log/capture", CAPTURE, &["scope", "key"], async move |request: &Request<'_>| {
                let field = |name: &str| request.head.get(name).and_then(Value::as_str).filter(|v| !v.is_empty());
                let failure = CapturedFailure {
                    code: field("code").unwrap_or("R20015").to_owned(),
                    message: field("message").unwrap_or("PO rejected").to_owned(),
                };
                root.capture_error(&failure.message, &failure);
                Ok(())
            })
            .await
        }
    });
}
