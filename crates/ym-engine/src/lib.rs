//! The engine host: boots the remotely-updatable `engine.js` on QuickJS and serves its host
//! protocols (quickjs/HOST_PROTOCOLS.md in YMusicV4), wire-identical to the Kotlin host.

mod engine;
mod error;
mod head;
mod http;
mod logs;

pub use engine::{DynamicEngine, EngineConfig};
pub use error::{EngineError, ErrorDetails, ErrorKind, WireError};
pub use logs::ParkedLoggers;
