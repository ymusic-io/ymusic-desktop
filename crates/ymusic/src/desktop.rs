//! The desktop shell around the window: its remembered size and position, the Dock icon, and the
//! tray icon that controls playback and brings a window closed to the tray back.

use std::time::Duration;

use gpui_kit::*;
use ym_common::log::Logger;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use ym_i18n::{Str, t};
use ym_settings::SettingsStore;

const BOUNDS_KEY: &str = "window.bounds";
const MIN_WIDTH: f32 = 360.;
const MIN_HEIGHT: f32 = 480.;
/// Resizes settle this long before the bounds are written.
pub const BOUNDS_SAVE_DELAY: Duration = Duration::from_millis(500);

/// The saved window bounds when they still fit a connected display, else `fallback` centered.
pub fn initial_bounds(settings: &dyn SettingsStore, fallback: Size<Pixels>, cx: &App) -> WindowBounds {
    let saved = settings.string(BOUNDS_KEY, "").ok().and_then(|raw| {
        let parts: Vec<f32> = raw.split(',').filter_map(|part| part.parse().ok()).collect();
        let [x, y, width, height] = parts[..] else { return None };
        (width >= MIN_WIDTH && height >= MIN_HEIGHT).then(|| bounds(point(px(x), px(y)), size(px(width), px(height))))
    });
    // A title bar that lands on no screen (a monitor unplugged since) could never be grabbed back.
    let on_screen = |saved: &Bounds<Pixels>| {
        let grip = point(saved.origin.x + px(100.), saved.origin.y + px(20.));
        cx.displays().iter().any(|display| display.bounds().contains(&grip))
    };
    match saved {
        Some(saved) if on_screen(&saved) => WindowBounds::Windowed(saved),
        _ => WindowBounds::centered(fallback, cx),
    }
}

/// Writes the window's position and size while it floats; a maximized or fullscreen window keeps
/// the last floating ones.
pub fn save_bounds(window: &Window, settings: std::sync::Arc<dyn SettingsStore>, log: Logger, cx: &App) {
    let WindowBounds::Windowed(frame) = window.window_bounds() else { return };
    // The window opens at a content size; its frame adds the title bar.
    let content = window.viewport_size();
    let raw = format!(
        "{},{},{},{}",
        f32::from(frame.origin.x),
        f32::from(frame.origin.y),
        f32::from(content.width),
        f32::from(content.height)
    );
    cx.global::<crate::graph::AppGraph>().tokio.spawn_blocking(move || {
        if let Err(error) = settings.put_raw(BOUNDS_KEY, raw) {
            log.warn(&format!("window bounds could not be saved: {error}"), None);
        }
    });
}

const LOGO_PNG: &[u8] = include_bytes!("../assets/ymusic-512.png");
#[cfg(any(target_os = "macos", target_os = "windows"))]
const TRAY_PNG: &[u8] = include_bytes!("../assets/ymusic-tray.png");

/// The Dock shows the app's logo even when it runs outside an app bundle.
#[cfg(target_os = "macos")]
pub fn set_dock_icon(log: &Logger) {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    let (Some(app_class), Some(image_class), Some(data_class)) =
        (AnyClass::get(c"NSApplication"), AnyClass::get(c"NSImage"), AnyClass::get(c"NSData"))
    else {
        log.warn("dock icon: AppKit classes missing", None);
        return;
    };
    // SAFETY: plain AppKit calls on the main thread; `init` hands back the one reference `alloc` made.
    unsafe {
        let data: Retained<AnyObject> =
            msg_send![data_class, dataWithBytes: LOGO_PNG.as_ptr().cast::<std::ffi::c_void>(), length: LOGO_PNG.len()];
        let allocated: *mut AnyObject = msg_send![image_class, alloc];
        let image: *mut AnyObject = msg_send![allocated, initWithData: &*data];
        let Some(image) = Retained::from_raw(image) else {
            log.warn("dock icon: the logo did not decode", None);
            return;
        };
        let app: Retained<AnyObject> = msg_send![app_class, sharedApplication];
        let _: () = msg_send![&*app, setApplicationIconImage: &*image];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon(_log: &Logger) {
    let _ = LOGO_PNG;
}

/// Hides the Dock icon while only the tray icon is left (macOS accessory activation).
#[cfg(target_os = "macos")]
pub fn set_dock_visible(visible: bool) {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    let Some(app_class) = AnyClass::get(c"NSApplication") else { return };
    // NSApplicationActivationPolicyRegular = 0, Accessory = 1.
    let policy: isize = if visible { 0 } else { 1 };
    // SAFETY: a plain AppKit call on the main thread.
    unsafe {
        let app: Retained<AnyObject> = msg_send![app_class, sharedApplication];
        let _: bool = msg_send![&*app, setActivationPolicy: policy];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_visible(_visible: bool) {}

/// What a tray click or menu entry asks for.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub enum TrayCommand {
    Open,
    TogglePlay,
    Stop,
    Quit,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub struct Tray {
    icon: tray_icon::TrayIcon,
    open: tray_icon::menu::MenuItem,
    toggle: tray_icon::menu::MenuItem,
    stop: tray_icon::menu::MenuItem,
    quit: tray_icon::menu::MenuItem,
    playing: Option<bool>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl Tray {
    /// A hidden tray icon whose commands arrive on the returned channel.
    pub fn new() -> anyhow::Result<(Self, futures::channel::mpsc::UnboundedReceiver<TrayCommand>)> {
        use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
        use tray_icon::{TrayIconBuilder, TrayIconEvent};
        let image = image::load_from_memory(TRAY_PNG)?.into_rgba8();
        let (width, height) = image.dimensions();
        let icon = tray_icon::Icon::from_rgba(image.into_raw(), width, height)?;
        let open = MenuItem::new(t(Str::CdOpenApp), true, None);
        let toggle = MenuItem::new(t(Str::TrayPause), true, None);
        let stop = MenuItem::new(t(Str::TrayStop), true, None);
        let quit = MenuItem::new(t(Str::TrayQuit), true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &PredefinedMenuItem::separator(), &toggle, &stop, &PredefinedMenuItem::separator(), &quit])?;
        let tray = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip(t(Str::AppName))
            .with_menu(Box::new(menu))
            .build()?;
        tray.set_visible(false)?;
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        let ids = [
            (open.id().clone(), 0u8),
            (toggle.id().clone(), 1),
            (stop.id().clone(), 2),
            (quit.id().clone(), 3),
        ];
        let menu_sender = sender.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let command = match ids.iter().find(|(id, _)| *id == event.id).map(|(_, which)| *which) {
                Some(0) => TrayCommand::Open,
                Some(1) => TrayCommand::TogglePlay,
                Some(2) => TrayCommand::Stop,
                Some(3) => TrayCommand::Quit,
                _ => return,
            };
            let _ = menu_sender.unbounded_send(command);
        }));
        // The menu opens on a click; a double-click on Windows opens the window as well.
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                let _ = sender.unbounded_send(TrayCommand::Open);
            }
        }));
        Ok((Self { icon: tray, open, toggle, stop, quit, playing: None }, receiver))
    }

    /// Shown while there is a queue to control or the window is hidden (the only way back then).
    pub fn update(&mut self, has_queue: bool, playing: bool, window_hidden: bool) -> anyhow::Result<()> {
        self.icon.set_visible(has_queue || window_hidden)?;
        self.toggle.set_enabled(has_queue);
        self.stop.set_enabled(has_queue);
        self.open.set_enabled(true);
        self.quit.set_enabled(true);
        if self.playing != Some(playing) {
            self.toggle.set_text(t(if playing { Str::TrayPause } else { Str::TrayResume }));
            self.playing = Some(playing);
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct Tray;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl Tray {
    pub fn new() -> anyhow::Result<(Self, futures::channel::mpsc::UnboundedReceiver<TrayCommand>)> {
        anyhow::bail!("no tray on this platform yet")
    }

    pub fn update(&mut self, _has_queue: bool, _playing: bool, _window_hidden: bool) -> anyhow::Result<()> {
        Ok(())
    }
}
