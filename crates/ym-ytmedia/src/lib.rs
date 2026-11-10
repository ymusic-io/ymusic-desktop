//! googlevideo SABR/UMP: the protocol, and the download session built on it. Knows nothing about
//! the engine or the player: everything per video arrives as plain data ([`sabr::SabrStreamSpec`]).
//! Protocol notes: YMusicV4 `yt-media/SABR.md`.

pub mod proto;
pub mod sabr;
pub mod segment_index;
pub mod ump;
