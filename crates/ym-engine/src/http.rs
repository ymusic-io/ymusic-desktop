//! The `"http"` host protocol (HOST_PROTOCOLS §1): one control pipe per exchange, plus a body
//! pipe the host opens lazily on the transport's first pull when the request has a body.
//!
//! Expected failures RESET the control pipe with an origin-classified `WireError` reason:
//! `R20001` for a request the host cannot build (caller bug), `network` for transport failures
//! before or after the head. Protocol violations error out of the handler (runtime backstop).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use ym_common::http::{DEFAULT_EXECUTE_TIMEOUT, HttpClients};
use ym_common::log::Logger;
use ym_quickjs::{BoxError, PIPE_CAP, Pipe, PipeError, QuickJsRuntime, WeakRuntime};

use crate::error::WireError;
use crate::head::{HeadError, read_json_head};
use crate::logs::{ParkedLoggers, prop_of, protocol_logger};

pub(crate) fn install(runtime: &QuickJsRuntime, clients: HttpClients, parked: Arc<ParkedLoggers>) {
    let weak = runtime.downgrade();
    runtime.on_accept("http", move |pipe| serve(weak.clone(), pipe, clients.clone(), parked.clone()));
}

const SABR_FORMATS_HEADER: &str = "X-Ym-Af";

struct Prepared {
    client: reqwest::Client,
    request: reqwest::Request,
    timeout: Duration,
}

enum Watch {
    Ended,
    Cancelled,
    Data,
    Failed(PipeError),
}

async fn serve(
    runtime: WeakRuntime,
    pipe: Pipe,
    clients: HttpClients,
    parked: Arc<ParkedLoggers>,
) -> Result<(), BoxError> {
    let (head, trailing) = match read_json_head(&pipe).await {
        Ok(parsed) => parsed,
        // The client abandoned the exchange before it started.
        Err(HeadError::Pipe(PipeError::Reset(_))) => return finish(&pipe).await,
        Err(error) => {
            let log = parked.root().fork("http");
            return reject(&pipe, &log, &format!("invalid request: {error}")).await;
        }
    };
    if !head.is_object() {
        return reject(&pipe, parked.root(), "invalid request: head is not a JSON object").await;
    }
    if !trailing.is_empty() {
        return Err("R20002 unexpected data on the control pipe".into());
    }
    let log = protocol_logger(&parked, &head, "http", prop_of(&head, "url"));
    let prepared = match prepare(&head, &clients, runtime) {
        Ok(prepared) => prepared,
        Err(error) => return reject(&pipe, &log, &format!("invalid request: {error}")).await,
    };

    // Armed right after the head: the control pipe carries nothing more, so a RESET (the client's
    // cancel) is observable at every stage, including a transport parked before the body pull.
    let watcher = async {
        match pipe.receive(1).await {
            Ok(None) => Watch::Ended,
            Ok(Some(_)) => Watch::Data,
            Err(PipeError::Reset(_)) => Watch::Cancelled,
            Err(error) => Watch::Failed(error),
        }
    };
    let exchange = exchange(&pipe, prepared, &log);
    tokio::pin!(watcher, exchange);
    let mut watching = true;
    loop {
        tokio::select! {
            result = &mut exchange => {
                result?;
                break;
            }
            event = &mut watcher, if watching => match event {
                Watch::Ended => watching = false,
                Watch::Cancelled => break,
                Watch::Data => return Err("R20002 unexpected data on the control pipe".into()),
                Watch::Failed(error) => return Err(error.into()),
            },
        }
    }
    finish(&pipe).await
}

async fn finish(pipe: &Pipe) -> Result<(), BoxError> {
    pipe.close_write(None).await?;
    pipe.close_read().await?;
    Ok(())
}

async fn reject(pipe: &Pipe, log: &Logger, message: &str) -> Result<(), BoxError> {
    log.warn(&format!("R20001 {message}"), None);
    reset(pipe, "R20001", message).await
}

async fn reset(pipe: &Pipe, code: &str, message: &str) -> Result<(), BoxError> {
    pipe.close_write(Some(WireError::new(code, message).to_reason())).await?;
    pipe.close_read().await?;
    Ok(())
}

fn prepare(head: &Value, clients: &HttpClients, runtime: WeakRuntime) -> Result<Prepared, BoxError> {
    let method = head.get("method").and_then(Value::as_str).unwrap_or("GET");
    let url = head.get("url").and_then(Value::as_str).unwrap_or_default();
    let mut headers = HeaderMap::new();
    if let Some(pairs) = head.get("headers").and_then(Value::as_array) {
        for pair in pairs {
            let name = pair.get(0).and_then(Value::as_str).ok_or("header entry without a name")?;
            let value = pair.get(1).and_then(Value::as_str).ok_or("header entry without a value")?;
            // The bundle tags cacheable SABR requests for the host's media proxy; never on the wire.
            if name.eq_ignore_ascii_case(SABR_FORMATS_HEADER) {
                continue;
            }
            headers.append(HeaderName::from_bytes(name.as_bytes())?, HeaderValue::from_str(value)?);
        }
    }
    let opts = head.get("opts");
    let follow_redirects = opts.and_then(|opts| opts.get("followRedirects")).and_then(Value::as_bool).unwrap_or(true);
    let timeout = opts
        .and_then(|opts| opts.get("timeoutMs"))
        .and_then(Value::as_u64)
        .map_or(DEFAULT_EXECUTE_TIMEOUT, Duration::from_millis);
    let client = clients.pick(follow_redirects).clone();
    let mut builder = client.request(reqwest::Method::from_bytes(method.as_bytes())?, url).headers(headers);
    if let Some(body_command) = head.get("bodyCmd").and_then(Value::as_str) {
        builder = builder.body(reqwest::Body::wrap_stream(body_stream(runtime, body_command.to_owned())));
    }
    Ok(Prepared { client, request: builder.build()?, timeout })
}

enum BodyState {
    Unopened(WeakRuntime, String),
    Open(Pipe),
}

/// The request body, read from its own pipe, opened on the transport's first pull: a response
/// that never needs the body (early error, redirect) never transfers it.
fn body_stream(runtime: WeakRuntime, command: String) -> impl Stream<Item = Result<Bytes, BoxError>> + Send + 'static {
    futures::stream::try_unfold(Some(BodyState::Unopened(runtime, command)), |state| async move {
        let pipe = match state {
            None => return Ok(None),
            Some(BodyState::Open(pipe)) => pipe,
            Some(BodyState::Unopened(runtime, command)) => {
                runtime.upgrade().ok_or(PipeError::RuntimeClosed)?.open_pipe(&command).await?
            }
        };
        match pipe.receive(PIPE_CAP).await? {
            Some(chunk) => Ok(Some((chunk, Some(BodyState::Open(pipe))))),
            None => {
                finish(&pipe).await?;
                Ok(None)
            }
        }
    })
}

async fn exchange(pipe: &Pipe, prepared: Prepared, log: &Logger) -> Result<(), BoxError> {
    let request_url = prepared.request.url().clone();
    log.info(&format!(
        "{} {}{}",
        prepared.request.method(),
        request_url.host_str().unwrap_or_default(),
        request_url.path()
    ));
    log.debug(|| format!("cookies={}", cookie_names(prepared.request.headers())));

    let response = match tokio::time::timeout(prepared.timeout, prepared.client.execute(prepared.request)).await {
        Err(_) => {
            log.warn("request failed: timeout", None);
            return reset(pipe, "network", "request failed: timeout").await;
        }
        Ok(Err(error)) => {
            let error = error.without_url();
            let message = format!("request failed: {}", error_chain(&error));
            log.warn(&message, Some(&error));
            return reset(pipe, "network", &message).await;
        }
        Ok(Ok(response)) => response,
    };

    let final_url = response.url().clone();
    log.info(&format!(
        "HTTP {} {}{}",
        response.status().as_u16(),
        final_url.host_str().unwrap_or_default(),
        final_url.path()
    ));
    let set_cookie_names: Vec<&str> = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok()?.split('=').next())
        .collect();
    if !set_cookie_names.is_empty() {
        log.debug(|| format!("set-cookie={}", set_cookie_names.join(",")));
    }
    let headers: Vec<Value> = response
        .headers()
        .iter()
        .map(|(name, value)| json!([name.as_str(), String::from_utf8_lossy(value.as_bytes())]))
        .collect();
    let head = json!({
        "status": response.status().as_u16(),
        "message": response.status().canonical_reason().unwrap_or_default(),
        "url": final_url.as_str(),
        "headers": headers,
    });
    if delivery_ended(pipe.send(head.to_string()).await)? {
        return Ok(());
    }
    let mut response = response;
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if delivery_ended(pipe.send(chunk).await)? {
                    return Ok(());
                }
            }
            Ok(None) => break,
            Err(error) => {
                // The head is out: a clean END now would read as a complete, truncated body.
                let error = error.without_url();
                let message = format!("response body failed: {}", error_chain(&error));
                log.warn(&message, Some(&error));
                return reset(pipe, "network", &message).await;
            }
        }
    }
    pipe.close_write(None).await?;
    Ok(())
}

/// `true` when the client stopped reading (cancelled or torn down): no one is left to tell.
fn delivery_ended(sent: Result<(), PipeError>) -> Result<bool, PipeError> {
    match sent {
        Ok(()) => Ok(false),
        Err(PipeError::BrokenPipe | PipeError::SendClosed(_)) => Ok(true),
        Err(error) => Err(error),
    }
}

/// Cookie names only: values are session credentials and never enter a log line.
fn cookie_names(headers: &HeaderMap) -> String {
    headers
        .get_all(reqwest::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| Some(pair.split('=').next()?.trim()).filter(|name| !name.is_empty()))
        .collect::<Vec<_>>()
        .join(",")
}

fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
