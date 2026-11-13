use crate::{MediaControlEvent, MediaMetadata, MediaPlayback, PlatformConfig};

/// A platform-specific error.
#[derive(Debug)]
pub struct Error;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "Error")
    }
}

impl std::error::Error for Error {}

/// A handle to OS media controls.
pub struct MediaControls;

impl MediaControls {
    /// Create media controls with the specified config.
    pub fn new(_config: PlatformConfig) -> Result<Self, Error> {
        Ok(Self)
    }

    /// Attach the media control events to a handler.
    pub fn attach<F>(&mut self, _event_handler: F) -> Result<(), Error>
    where
        F: Fn(MediaControlEvent) + Send + 'static,
    {
        Ok(())
    }

    /// Detach the event handler.
    pub fn detach(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// Set the current playback status.
    pub fn set_playback(&mut self, _playback: MediaPlayback) -> Result<(), Error> {
        Ok(())
    }

    /// Set the metadata of the currently playing media item.
    pub fn set_metadata(&mut self, _metadata: MediaMetadata) -> Result<(), Error> {
        Ok(())
    }

    /// Set the playback rate the OS extrapolates the position at (1.0 is normal speed).
    pub fn set_rate(&mut self, _rate: f64) -> Result<(), Error> {
        Ok(())
    }
}
