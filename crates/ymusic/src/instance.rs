//! One app process per data dir: the first launch holds `instance.lock` and listens on a loopback
//! port; a later launch hands its request (files, links, a player command) over and exits. The
//! port file carries a random token, so only a process that can read the data dir is listened to.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use gpui_kit::Global;
use serde_json::{Value, json};
use ym_common::log::Logger;

/// ~9 s: outlasts a quitting instance's wind-down, which holds the lock but takes nothing.
const HANDOFF_ATTEMPTS: u32 = 60;
const HANDOFF_RETRY: Duration = Duration::from_millis(150);
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_REQUEST_BYTES: u64 = 1 << 20;
const ACK: &[u8] = b"ok\n";

static ACCEPTING: AtomicBool = AtomicBool::new(true);

/// What a launch asks of the app: bring the window back, or a player task (the Windows Jump List).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Show,
    TogglePlay,
    Stop,
    Quit,
}

impl Command {
    const ALL: [(Command, &'static str); 4] =
        [(Command::Show, "show"), (Command::TogglePlay, "toggle"), (Command::Stop, "stop"), (Command::Quit, "quit")];

    fn name(self) -> &'static str {
        Self::ALL.iter().find(|(command, _)| *command == self).map_or("show", |(_, name)| name)
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, known)| *known == name).map(|(command, _)| *command)
    }
}

/// A launch's or an OS open event's command, media files and outside links.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub command: Command,
    pub files: Vec<PathBuf>,
    pub links: Vec<String>,
}

impl Request {
    /// From the process arguments (without the program): `--player=toggle|stop|quit`, media file
    /// paths or `file://` URLs, and app / web links. Other flags (macOS's `-psn_…`) are ignored.
    pub fn from_args(args: impl IntoIterator<Item = OsString>) -> Self {
        let mut request = Self { command: Command::Show, files: Vec::new(), links: Vec::new() };
        for arg in args {
            let arg = arg.to_string_lossy().into_owned();
            if let Some(name) = arg.strip_prefix("--player=") {
                if let Some(command) = Command::from_name(name) {
                    request.command = command;
                }
            } else if arg.starts_with('-') {
                continue;
            } else if is_link(&arg) {
                request.add_url(arg);
            } else if let Ok(path) = std::path::absolute(&arg)
                && path.is_file()
            {
                request.files.push(path);
            }
        }
        request
    }

    /// From the URLs an OS open event carries (macOS `application:openURLs:`): files and links.
    pub fn from_urls(urls: Vec<String>) -> Self {
        let mut request = Self { command: Command::Show, files: Vec::new(), links: Vec::new() };
        for url in urls {
            request.add_url(url);
        }
        request
    }

    fn add_url(&mut self, url: String) {
        match url::Url::parse(&url) {
            Ok(parsed) if parsed.scheme() == "file" => {
                if let Ok(path) = parsed.to_file_path() {
                    self.files.push(path);
                }
            }
            Ok(_) => self.links.push(url),
            Err(_) => {}
        }
    }

    pub fn is_empty(&self) -> bool {
        self.command == Command::Show && self.files.is_empty() && self.links.is_empty()
    }

    fn to_wire(&self, token: &str) -> Vec<u8> {
        let files: Vec<String> = self.files.iter().map(|path| path.to_string_lossy().into_owned()).collect();
        let wire = json!({ "token": token, "command": self.command.name(), "files": files, "links": self.links });
        wire.to_string().into_bytes()
    }

    fn from_wire(bytes: &[u8], token: &str) -> Option<Self> {
        let wire: Value = serde_json::from_slice(bytes).ok()?;
        if wire.get("token")?.as_str()? != token {
            return None;
        }
        let strings = |key: &str| -> Vec<String> {
            wire.get(key)
                .and_then(Value::as_array)
                .map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default()
        };
        Some(Self {
            command: Command::from_name(wire.get("command")?.as_str()?)?,
            files: strings("files").into_iter().map(PathBuf::from).collect(),
            links: strings("links"),
        })
    }
}

/// An app link or a web link (what the engine's `resolve-link` takes); never a bare path.
fn is_link(arg: &str) -> bool {
    url::Url::parse(arg).is_ok_and(|url| matches!(url.scheme(), "ymusic" | "http" | "https" | "file"))
}

/// Requests from later launches and OS open events, held until the shell takes them.
pub struct Inbox(pub UnboundedReceiver<Request>);

impl Global for Inbox {}

pub enum Claim {
    /// This process is the app instance; later launches' requests arrive on the channel given.
    Owner,
    /// The request went to the running instance; this process has nothing left to do.
    HandedOver,
}

#[derive(Debug, thiserror::Error)]
pub enum ClaimError {
    #[error("R96000 another process holds the instance lock but took no request")]
    NoAnswer(#[source] Option<std::io::Error>),
    #[error("R96001 the instance claim could not be set up")]
    Setup(#[source] std::io::Error),
}

/// Claims this process as the app instance for `dir`, or hands `request` to the running one.
pub fn claim(
    dir: &Path,
    request: &Request,
    sender: UnboundedSender<Request>,
    log: &Logger,
) -> Result<Claim, ClaimError> {
    std::fs::create_dir_all(dir).map_err(ClaimError::Setup)?;
    let lock_path = dir.join("instance.lock");
    let port_path = dir.join("instance.port");
    let mut last_error = None;
    for attempt in 0..HANDOFF_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(HANDOFF_RETRY);
        }
        let lock =
            OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).map_err(ClaimError::Setup)?;
        match lock.try_lock() {
            Ok(()) => {
                listen(lock, &port_path, sender, log.clone()).map_err(ClaimError::Setup)?;
                return Ok(Claim::Owner);
            }
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => return Err(ClaimError::Setup(error)),
        }
        drop(lock);
        if attempt == 0 && request.command == Command::Show {
            share_foreground(log);
        }
        match hand_off(&port_path, request) {
            Ok(()) => return Ok(Claim::HandedOver),
            Err(error) => last_error = Some(error),
        }
    }
    Err(ClaimError::NoAnswer(last_error))
}

/// Stops taking requests while the lock stays held until exit: a launch during the quit wind-down
/// gets no answer and retries until this process is gone, instead of being dropped.
pub fn stop_accepting() {
    ACCEPTING.store(false, Ordering::SeqCst);
}

fn listen(lock: File, port_path: &Path, sender: UnboundedSender<Request>, log: Logger) -> std::io::Result<()> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let token: String = rand::random::<[u8; 16]>().iter().map(|byte| format!("{byte:02x}")).collect();
    let staged = port_path.with_extension("port.tmp");
    std::fs::write(&staged, format!("{} {token}", listener.local_addr()?.port()))?;
    std::fs::rename(&staged, port_path)?;
    std::thread::Builder::new().name("single-instance".into()).spawn(move || {
        // The lock lives as long as the listener: until the process exits.
        let _lock = lock;
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(stream) => stream,
                Err(error) => {
                    log.warn("instance hand-off: accept failed", Some(&error));
                    continue;
                }
            };
            if !ACCEPTING.load(Ordering::SeqCst) {
                continue;
            }
            match receive(stream, &token) {
                Ok(Some(request)) => {
                    if sender.unbounded_send(request).is_err() {
                        return;
                    }
                }
                Ok(None) => log.warn("instance hand-off: a request without the token was ignored", None),
                Err(error) => log.warn("instance hand-off failed", Some(&error)),
            }
        }
    })?;
    Ok(())
}

fn receive(mut stream: TcpStream, token: &str) -> std::io::Result<Option<Request>> {
    stream.set_read_timeout(Some(HANDOFF_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDOFF_TIMEOUT))?;
    let mut bytes = Vec::new();
    (&mut stream).take(MAX_REQUEST_BYTES).read_to_end(&mut bytes)?;
    let Some(request) = Request::from_wire(&bytes, token) else { return Ok(None) };
    stream.write_all(ACK)?;
    Ok(Some(request))
}

fn hand_off(port_path: &Path, request: &Request) -> std::io::Result<()> {
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "bad instance.port");
    let contents = std::fs::read_to_string(port_path)?;
    let (port, token) = contents.trim().split_once(' ').ok_or_else(invalid)?;
    let port: u16 = port.parse().map_err(|_| invalid())?;
    let mut stream = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), HANDOFF_TIMEOUT)?;
    stream.set_read_timeout(Some(HANDOFF_TIMEOUT))?;
    stream.write_all(&request.to_wire(token))?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut ack = Vec::new();
    stream.take(ACK.len() as u64).read_to_end(&mut ack)?;
    if ack != ACK {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "the instance did not take the request",
        ));
    }
    Ok(())
}

/// Windows lets only the process the user just launched take the foreground: pass that on.
#[cfg(target_os = "windows")]
fn share_foreground(log: &Logger) {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn AllowSetForegroundWindow(process_id: u32) -> i32;
    }
    const ASFW_ANY: u32 = u32::MAX;
    // SAFETY: a plain Win32 call without pointers.
    if unsafe { AllowSetForegroundWindow(ASFW_ANY) } == 0 {
        log.info("AllowSetForegroundWindow refused");
    }
}

#[cfg(not(target_os = "windows"))]
fn share_foreground(_log: &Logger) {}
