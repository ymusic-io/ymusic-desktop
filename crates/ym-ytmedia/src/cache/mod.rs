//! The player media cache, on the JVM app's on-disk format so either app reads and fills the
//! other's: `{root}/cache-index.db` (the `SegmentDatabase` schema) and immutable span files
//! `{root}/{key}/{start}_{len}.span`, `key` = `2_{itag}_{lmt}`. See [`blob::DiskMediaBlobCache`]
//! for the store and [`media::MediaCache`] for the segment layer the SABR proxy plans with.

pub mod blob;
pub mod check;
pub mod media;
pub mod planner;

pub use blob::{BlobWriter, DiskMediaBlobCache, PinnedSource};
pub use media::{MediaCache, SegmentFill};
pub use planner::{SabrServe, serve_sabr};

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// A span file could not be opened, read, written or committed (disk full, directory gone):
    /// the whole on-disk cache is suspect. A deleted span file is only a miss, never this.
    #[error("player cache storage: {op}: {source}")]
    Storage {
        op: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("player cache index: {0}")]
    Index(#[from] rusqlite::Error),
    #[error("player cache: {0}")]
    Invalid(String),
}

impl CacheError {
    pub fn is_storage_fault(&self) -> bool {
        matches!(self, Self::Storage { .. })
    }

    pub fn is_disk_full(&self) -> bool {
        matches!(self, Self::Storage { source, .. } if source.kind() == std::io::ErrorKind::StorageFull)
    }
}

pub(crate) fn storage(op: &'static str) -> impl FnOnce(std::io::Error) -> CacheError {
    move |source| CacheError::Storage { op, source }
}
