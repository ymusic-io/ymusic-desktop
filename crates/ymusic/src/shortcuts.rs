//! The JVM desktop app's keyboard shortcuts, bound in the Shell's key context so a focused text
//! field or an open dialog gets keys first.

use gpui_kit::{App, KeyBinding, actions};

pub const CONTEXT: &str = "Shell";

actions!(
    ymusic,
    [
        TogglePlay,
        SeekBackward,
        SeekForward,
        PreviousTrack,
        NextTrack,
        VolumeUp,
        VolumeDown,
        ToggleNowPlaying,
        ToggleDrawer,
        OpenSearch,
        ToggleFullscreen,
        Escape,
        SelectTab1,
        SelectTab2,
        SelectTab3,
        SelectTab4,
        SelectTab5,
        SelectTab6,
        /// Open the sign-in screen (dispatched by the sign-in buttons, no key).
        OpenSignIn,
    ]
);

/// Cmd on macOS, Ctrl elsewhere.
#[cfg(target_os = "macos")]
macro_rules! secondary {
    ($key:literal) => {
        concat!("cmd-", $key)
    };
}
#[cfg(not(target_os = "macos"))]
macro_rules! secondary {
    ($key:literal) => {
        concat!("ctrl-", $key)
    };
}

pub fn bind(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("f8", TogglePlay, context),
        KeyBinding::new("f7", SeekBackward, context),
        KeyBinding::new("f9", SeekForward, context),
        KeyBinding::new("f11", ToggleFullscreen, context),
        // macOS's standard full screen.
        #[cfg(target_os = "macos")]
        KeyBinding::new("ctrl-cmd-f", ToggleFullscreen, context),
        KeyBinding::new(secondary!("n"), ToggleNowPlaying, context),
        KeyBinding::new(secondary!("m"), ToggleDrawer, context),
        KeyBinding::new(secondary!("f"), OpenSearch, context),
        KeyBinding::new(secondary!("left"), PreviousTrack, context),
        KeyBinding::new(secondary!("right"), NextTrack, context),
        KeyBinding::new(secondary!("shift-left"), SeekBackward, context),
        KeyBinding::new(secondary!("shift-right"), SeekForward, context),
        KeyBinding::new(secondary!("up"), VolumeUp, context),
        KeyBinding::new(secondary!("down"), VolumeDown, context),
        KeyBinding::new("escape", Escape, context),
        KeyBinding::new(secondary!("1"), SelectTab1, context),
        KeyBinding::new(secondary!("2"), SelectTab2, context),
        KeyBinding::new(secondary!("3"), SelectTab3, context),
        KeyBinding::new(secondary!("4"), SelectTab4, context),
        KeyBinding::new(secondary!("5"), SelectTab5, context),
        KeyBinding::new(secondary!("6"), SelectTab6, context),
    ]);
}
