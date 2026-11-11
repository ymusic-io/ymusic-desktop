//! The desktop shell around the window: its remembered size and position, the Dock icon, and the
//! tray icon that controls playback and brings a window closed to the tray back.

use std::time::Duration;

use gpui_kit::*;
use ym_common::log::Logger;
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

#[cfg_attr(target_os = "windows", allow(dead_code))]
const LOGO_PNG: &[u8] = include_bytes!("../assets/ymusic-512.png");
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
pub fn set_dock_icon(_log: &Logger) {}

/// The Wayland app_id and X11 WM_CLASS of the window; GNOME takes its icon from the desktop entry
/// of the same name.
pub const APP_ID: &str = "io.ymusic.YMusic";

/// The logo as the X11 `_NET_WM_ICON` (Wayland has no window icon: the desktop entry gives it).
#[cfg(target_os = "linux")]
pub fn window_icon(log: &Logger) -> Option<std::sync::Arc<image::RgbaImage>> {
    match image::load_from_memory(LOGO_PNG) {
        Ok(logo) => {
            Some(std::sync::Arc::new(logo.resize(128, 128, image::imageops::FilterType::Triangle).into_rgba8()))
        }
        Err(error) => {
            log.warn("window icon: the logo did not decode", Some(&error));
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn window_icon(_log: &Logger) -> Option<std::sync::Arc<image::RgbaImage>> {
    None
}

/// GNOME Shell takes window icons only from a desktop entry matched by app_id / StartupWMClass, so
/// a hidden (NoDisplay) entry in the user's data dir maps the window to the logo. Rewritten only
/// when its contents change. An install under /opt or /usr ships its own entry, which a second one
/// with the same StartupWMClass would compete with, so there this one is removed.
#[cfg(target_os = "linux")]
pub fn sync_desktop_entry() -> anyhow::Result<()> {
    use std::path::PathBuf;
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let entry_file = data_home.join("applications").join(format!("{APP_ID}.desktop"));
    let icon_file = data_home.join("icons/hicolor/512x512/apps").join(format!("{APP_ID}.png"));
    let exe = std::env::current_exe()?;
    if exe.starts_with("/opt") || exe.starts_with("/usr") {
        for file in [&entry_file, &icon_file] {
            match std::fs::remove_file(file) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
                _ => {}
            }
        }
        return Ok(());
    }
    write_if_changed(&icon_file, LOGO_PNG)?;
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName={}\nExec={}\nIcon={}\nStartupWMClass={APP_ID}\nNoDisplay=true\n",
        t(Str::AppName),
        desktop_exec_quoted(&exe.to_string_lossy()),
        icon_file.display(),
    );
    write_if_changed(&entry_file, entry.as_bytes())
}

#[cfg(target_os = "linux")]
fn write_if_changed(file: &std::path::Path, contents: &[u8]) -> anyhow::Result<()> {
    if std::fs::read(file).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, contents)?;
    Ok(())
}

/// An Exec program path per the Desktop Entry spec: quoted, `"` `` ` `` `$` `\` backslash-escaped,
/// then string-escaped (each backslash doubled), and `%` doubled.
#[cfg(target_os = "linux")]
fn desktop_exec_quoted(path: &str) -> String {
    let mut quoted = String::from("\"");
    for character in path.chars() {
        match character {
            '\\' => quoted.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(character);
            }
            '%' => quoted.push_str("%%"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
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

gpui_kit::actions!(ymusic_dock, [DockTogglePlay, DockStop]);

/// The Dock icon's menu (JVM `DockMenu`): Pause/Resume and Stop while there is a queue; the Dock
/// adds its own Quit. Its entries arrive as tray commands.
#[derive(Default)]
pub struct DockMenu {
    shown: Option<(bool, bool)>,
}

impl DockMenu {
    pub fn new(commands: futures::channel::mpsc::UnboundedSender<TrayCommand>, cx: &mut App) -> Self {
        let toggle = commands.clone();
        cx.on_action(move |_: &DockTogglePlay, _| {
            let _ = toggle.unbounded_send(TrayCommand::TogglePlay);
        });
        cx.on_action(move |_: &DockStop, _| {
            let _ = commands.unbounded_send(TrayCommand::Stop);
        });
        Self::default()
    }

    #[cfg(target_os = "macos")]
    pub fn update(&mut self, has_queue: bool, playing: bool, cx: &App) {
        if self.shown == Some((has_queue, playing)) {
            return;
        }
        self.shown = Some((has_queue, playing));
        let items = if has_queue {
            vec![
                MenuItem::action(t(if playing { Str::TrayPause } else { Str::TrayResume }), DockTogglePlay),
                MenuItem::action(t(Str::TrayStop), DockStop),
            ]
        } else {
            Vec::new()
        };
        cx.set_dock_menu(items);
    }

    /// gpui turns a dock menu into Jump List tasks on Windows, which this app does not handle.
    #[cfg(not(target_os = "macos"))]
    pub fn update(&mut self, has_queue: bool, playing: bool, _cx: &App) {
        self.shown = Some((has_queue, playing));
    }
}

/// What a tray click or menu entry asks for.
pub enum TrayCommand {
    Open,
    TogglePlay,
    Stop,
    Quit,
    /// The tray host went away: a window hidden to it has no way back but this.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Lost,
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
    pub fn new(
        _tokio: &tokio::runtime::Handle,
        _log: Logger,
    ) -> anyhow::Result<(Self, futures::channel::mpsc::UnboundedReceiver<TrayCommand>)> {
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

    /// Whether a window hidden now could be brought back from the tray.
    pub fn available(&self) -> bool {
        true
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Default)]
struct TrayState {
    has_queue: bool,
    playing: bool,
    window_hidden: bool,
}

/// The StatusNotifierItem; its properties read the shared state, so a poke re-reads the newest.
#[cfg(target_os = "linux")]
struct SniTray {
    state: std::sync::Arc<std::sync::Mutex<TrayState>>,
    commands: futures::channel::mpsc::UnboundedSender<TrayCommand>,
    available: std::sync::Arc<std::sync::atomic::AtomicBool>,
    icons: Vec<ksni::Icon>,
}

#[cfg(target_os = "linux")]
impl SniTray {
    fn state(&self) -> TrayState {
        self.state.lock().map(|state| *state).unwrap_or_default()
    }

    fn send(&self, command: TrayCommand) {
        let _ = self.commands.unbounded_send(command);
    }
}

#[cfg(target_os = "linux")]
impl ksni::Tray for SniTray {
    fn id(&self) -> String {
        "ymusic".into()
    }

    fn title(&self) -> String {
        t(Str::AppName).into()
    }

    fn status(&self) -> ksni::Status {
        let state = self.state();
        if state.has_queue || state.window_hidden { ksni::Status::Active } else { ksni::Status::Passive }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: t(Str::AppName).into(), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCommand::Open);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        let state = self.state();
        let item = |label: Str, enabled: bool, command: fn() -> TrayCommand| -> MenuItem<Self> {
            StandardItem {
                label: t(label).into(),
                enabled,
                activate: Box::new(move |tray: &mut Self| tray.send(command())),
                ..Default::default()
            }
            .into()
        };
        vec![
            item(Str::CdOpenApp, true, || TrayCommand::Open),
            MenuItem::Separator,
            item(if state.playing { Str::TrayPause } else { Str::TrayResume }, state.has_queue, || {
                TrayCommand::TogglePlay
            }),
            item(Str::TrayStop, state.has_queue, || TrayCommand::Stop),
            MenuItem::Separator,
            item(Str::TrayQuit, true, || TrayCommand::Quit),
        ]
    }

    fn watcher_online(&self) {
        self.available.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        self.available.store(false, std::sync::atomic::Ordering::Relaxed);
        self.send(TrayCommand::Lost);
        true
    }
}

/// A StatusNotifierItem over D-Bus (GNOME needs the AppIndicator extension, KDE has it built in).
#[cfg(target_os = "linux")]
pub struct Tray {
    state: std::sync::Arc<std::sync::Mutex<TrayState>>,
    handle: std::sync::Arc<std::sync::Mutex<Option<ksni::Handle<SniTray>>>>,
    available: std::sync::Arc<std::sync::atomic::AtomicBool>,
    tokio: tokio::runtime::Handle,
}

#[cfg(target_os = "linux")]
impl Tray {
    /// Registers with the StatusNotifierWatcher in the background; until that succeeds the tray is
    /// not [`available`](Self::available). Commands arrive on the returned channel.
    pub fn new(
        tokio: &tokio::runtime::Handle,
        log: Logger,
    ) -> anyhow::Result<(Self, futures::channel::mpsc::UnboundedReceiver<TrayCommand>)> {
        use ksni::TrayMethods;
        let image = image::load_from_memory(TRAY_PNG)?.into_rgba8();
        let (width, height) = image.dimensions();
        let argb = image.pixels().flat_map(|pixel| {
            let [red, green, blue, alpha] = pixel.0;
            [alpha, red, green, blue]
        });
        let icon = ksni::Icon { width: width as i32, height: height as i32, data: argb.collect() };
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        let state = std::sync::Arc::new(std::sync::Mutex::new(TrayState::default()));
        let available = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sni = SniTray { state: state.clone(), commands: sender, available: available.clone(), icons: vec![icon] };
        let (slot, registered) = (handle.clone(), available.clone());
        tokio.spawn(async move {
            match sni.spawn().await {
                Ok(spawned) => {
                    if let Ok(mut slot) = slot.lock() {
                        *slot = Some(spawned);
                    }
                    registered.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                Err(error) => log.warn("tray icon could not be registered", Some(&error)),
            }
        });
        Ok((Self { state, handle, available, tokio: tokio.clone() }, receiver))
    }

    /// Shown while there is a queue to control or the window is hidden (the only way back then).
    pub fn update(&mut self, has_queue: bool, playing: bool, window_hidden: bool) -> anyhow::Result<()> {
        if let Ok(mut state) = self.state.lock() {
            *state = TrayState { has_queue, playing, window_hidden };
        }
        let handle = self.handle.lock().ok().and_then(|handle| handle.clone());
        if let Some(handle) = handle {
            self.tokio.spawn(async move { handle.update(|_| ()).await });
        }
        Ok(())
    }

    /// Whether a window hidden now could be brought back from the tray.
    pub fn available(&self) -> bool {
        self.available.load(std::sync::atomic::Ordering::Relaxed)
    }
}
