mod a11y;
mod accent;
mod account_view;
mod assets;
mod audio_effects;
mod caption_style;
mod cookie_guide;
mod cookie_lock;
mod cookie_session;
mod desktop;
mod download_dialog;
mod downloads;
mod downloads_view;
mod feed;
mod feedback_view;
mod file_rename;
mod fps_badge;
mod graph;
mod http_client;
mod instance;
mod launch;
mod library;
mod library_playlist;
mod library_search;
mod local_art;
mod local_engine;
mod local_playlist;
mod media_session;
mod mini_player;
mod now_playing;
mod offline;
mod play_again;
mod play_history;
mod playback;
mod playlist_download;
mod queue_nav;
mod quick_download;
mod search_filter;
mod search_view;
mod settings_lists;
mod settings_view;
mod shell;
mod shortcuts;
mod shortcuts_help;
mod signin;
mod signin_dialog;
mod sleep_timer;
mod sound_panel;
mod spinner;
mod sponsor_block;
mod tab_row;
mod telemetry;
mod tray_popup;
mod video_details;
mod video_layer;
mod youtube_session;
mod yt_playlist;

use gpui_kit::*;
use ym_common::http::HttpClients;
use ym_i18n::{Str, t};

use crate::cookie_session::{CookieOpen, LockedJar};
use crate::graph::AppGraph;
use crate::http_client::ReqwestHttpClient;
use crate::launch::{BootInputs, Launch};
use crate::telemetry::LogSetup;

enum Start {
    Ready(Box<AppGraph>, Option<ym_telemetry::Telemetry>),
    Locked(std::sync::Arc<LockedJar>, BootInputs, LogSetup),
}

/// A launch that cannot boot ends here, its error captured.
pub fn boot_failed(log: &ym_common::log::Logger, error: &ym_engine::EngineError) -> ! {
    log.capture_error("engine boot failed", error);
    eprintln!("{}", crate::feed::error_text(error));
    std::process::exit(1);
}

fn main() {
    ym_i18n::use_system_locale();
    // Debug runs can be pointed at a throwaway data dir for automated checks.
    let data_dir = std::env::var_os("YMUSIC_DATA_DIR")
        .filter(|_| cfg!(debug_assertions))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("YMusicRust"));
    let log_setup = crate::telemetry::logger(&data_dir);
    let log = log_setup.log.clone();
    let launch = crate::instance::Request::from_args(std::env::args_os().skip(1));
    let (inbox_sender, inbox) = futures::channel::mpsc::unbounded();
    let instance_log = log.fork("instance");
    match crate::instance::claim(&data_dir, &launch, inbox_sender.clone(), &instance_log) {
        Ok(crate::instance::Claim::HandedOver) => return,
        // A player task (Jump List) with no app running has nothing to control.
        Ok(crate::instance::Claim::Owner) if launch.command != crate::instance::Command::Show => return,
        Ok(crate::instance::Claim::Owner) => {}
        Err(error @ crate::instance::ClaimError::NoAnswer(_)) => {
            instance_log.capture_error("another instance holds the lock but did not answer", &error);
            std::process::exit(1);
        }
        Err(error) => instance_log.warn("single-instance setup failed, starting without it", Some(&error)),
    }
    if !launch.is_empty() {
        let _ = inbox_sender.unbounded_send(launch);
    }
    let tokio =
        tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("ym-core").build().expect("tokio runtime");
    let http = HttpClients::new().expect("HTTP client");
    let local_art = std::sync::Arc::new(crate::local_art::LocalArt::new(data_dir.join("art-cache"), log.fork("art")));
    let app_db = graph::open_app_db(&data_dir).unwrap_or_else(|error| boot_failed(&log, &error));
    let opened = crate::cookie_session::open(app_db.clone(), &data_dir, &log).unwrap_or_else(|error| {
        boot_failed(&log, &ym_engine::EngineError::site("R98000", format!("cookie store: {error}")))
    });
    crate::settings_view::apply_language(app_db.as_ref());
    let inputs = crate::launch::BootInputs {
        tokio: tokio.handle().clone(),
        log: log.clone(),
        http: http.clone(),
        data_dir,
        app_db: app_db.clone(),
    };
    let app_logs = crate::telemetry::AppLogs(log_setup.logs.clone());
    let start = match opened {
        CookieOpen::Ready(session) => {
            let graph = tokio.block_on(inputs.boot(session)).unwrap_or_else(|error| boot_failed(&log, &error));
            let reporting = crate::telemetry::start_reporting(&log_setup, &graph);
            Start::Ready(Box::new(graph), reporting)
        }
        CookieOpen::Locked(locked) => {
            log.info("cookie jar is password-sealed: waiting for the unlock screen");
            Start::Locked(locked, inputs, log_setup)
        }
    };
    let images = ReqwestHttpClient::new(http.following.clone(), tokio.handle().clone());
    #[cfg(target_os = "linux")]
    let tokio_handle = tokio.handle().clone();

    crate::a11y::install_debug_log();
    let assets = crate::assets::AppAssets { local_art: local_art.clone() };
    let app = application().with_assets(assets).with_http_client(images);
    // Opening the app again (Finder, Spotlight, the Dock) brings a window closed to the tray back.
    app.on_reopen(|cx| {
        if let Some(shell) = cx.try_global::<crate::shell::ShellHandle>().and_then(|handle| handle.0.upgrade()) {
            shell.update(cx, |shell, cx| shell.show_window(cx));
        }
    });
    // macOS delivers ymusic:// links and Finder's Open With as open-URL events, a cold launch's
    // included, before the window exists.
    app.on_open_urls(move |urls| {
        let _ = inbox_sender.unbounded_send(crate::instance::Request::from_urls(urls));
    });
    app.run(move |cx| {
        init(cx);
        cx.set_global(crate::instance::Inbox(inbox));
        cx.on_app_quit(|_| {
            crate::instance::stop_accepting();
            async {}
        })
        .detach();
        cx.set_global(crate::local_art::SharedLocalArt(local_art));
        crate::shortcuts::bind(cx);
        crate::a11y::init(cx);
        // No enter/exit animations anywhere: dialogs, popovers, menus and tabs change in one frame.
        cx.set_reduce_motion(true);
        // A guest's cache is fire-and-forget at quit: nothing else reads it.
        cx.on_app_quit(|cx| {
            if let Some(dir) = cx.try_global::<AppGraph>().and_then(|graph| graph.guest_cache.clone()) {
                let _ = std::fs::remove_dir_all(dir);
            }
            async {}
        })
        .detach();
        crate::settings_view::apply_saved_theme(app_db.as_ref(), cx);
        crate::desktop::set_dock_icon(&log);
        #[cfg(target_os = "linux")]
        {
            let log = log.fork("desktop-entry");
            tokio_handle.spawn_blocking(move || {
                if let Err(error) = crate::desktop::sync_desktop_entry() {
                    log.warn("desktop entry install failed", Some(error.as_ref()));
                }
            });
        }
        let window_icon = crate::desktop::window_icon(&log);
        let window_bounds = match dev_window_size() {
            Some(dev) => WindowBounds::centered(dev, cx),
            None => crate::desktop::initial_bounds(app_db.as_ref(), size(px(1180.), px(780.)), cx),
        };
        let locked = match start {
            Start::Ready(graph, reporting) => {
                cx.set_global(*graph);
                cx.set_global(crate::telemetry::Reporting { _guard: reporting });
                None
            }
            Start::Locked(locked, inputs, log_setup) => Some((locked, inputs, log_setup)),
        };
        cx.set_global(app_logs);
        let options = WindowOptions {
            window_bounds: Some(window_bounds),
            titlebar: Some(TitlebarOptions { title: Some(t(Str::AppName).into()), ..Default::default() }),
            // Debug dev-route runs float, so a window launched from a terminal still draws frames
            // (a covered window gets none) for screenshots.
            kind: if cfg!(debug_assertions)
                && std::env::var_os("YMUSIC_DEV_ROUTE").is_some()
                && std::env::var_os("YMUSIC_DEV_A11Y_DUMP").is_none()
            {
                WindowKind::PopUp
            } else {
                WindowKind::Normal
            },
            // The video view sits behind gpui's, so the window and Root stay unpainted and the
            // Shell paints everything but the video box.
            window_background: WindowBackgroundAppearance::Transparent,
            app_id: Some(crate::desktop::APP_ID.into()),
            icon: window_icon,
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            crate::desktop::install_pin_button(window, &log.fork("pin"));
            crate::desktop::remember_main_window(window);
            let launch = cx.new(|cx| match locked {
                Some((locked, inputs, log_setup)) => Launch::locked(locked, inputs, log_setup, window, cx),
                None => Launch::shell(window, cx),
            });
            cx.new(|cx| gpui_kit::base::Root::new(launch, window, cx).bg(transparent_black()))
        })
        .expect("open main window");
        cx.activate(true);
    });
    drop(tokio);
}

/// Debug: `YMUSIC_DEV_WINDOW=WxH` opens the window at that size, for checking breakpoints.
fn dev_window_size() -> Option<Size<Pixels>> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let spec = std::env::var("YMUSIC_DEV_WINDOW").ok()?;
    let (width, height) = spec.split_once('x')?;
    Some(size(px(width.parse().ok()?), px(height.parse().ok()?)))
}
