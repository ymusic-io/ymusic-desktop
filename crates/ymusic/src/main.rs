mod a11y;
mod accent;
mod account_view;
mod assets;
mod audio_effects;
mod caption_style;
mod desktop;
mod download_dialog;
mod downloads;
mod downloads_view;
mod feed;
mod feedback_view;
mod fps_badge;
mod graph;
mod http_client;
mod instance;
mod library;
mod library_search;
mod local_art;
mod local_engine;
mod local_playlist;
mod media_session;
mod mini_player;
mod now_playing;
mod play_again;
mod play_history;
mod playback;
mod playlist_download;
mod queue_nav;
mod quick_download;
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
mod video_details;
mod video_layer;
mod youtube_session;
mod yt_playlist;

use gpui_kit::*;
use ym_common::http::HttpClients;
use ym_i18n::{Str, t};

use crate::graph::AppGraph;
use crate::http_client::ReqwestHttpClient;
use crate::shell::Shell;

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
    let graph = match tokio.block_on(AppGraph::boot(tokio.handle().clone(), log.clone(), http.clone(), data_dir)) {
        Ok(graph) => graph,
        Err(error) => {
            log.capture_error("engine boot failed", &error);
            eprintln!("{}", crate::feed::error_text(&error));
            std::process::exit(1);
        }
    };
    crate::settings_view::apply_language(graph.settings.as_ref());
    let _reporting = crate::telemetry::start_reporting(&log_setup, &graph);
    if graph.cookies_locked {
        log.warn("cookie jar is password-sealed: running signed out until it is unlocked", None);
    }
    let images = ReqwestHttpClient::new(http.following.clone(), tokio.handle().clone());

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
        crate::settings_view::apply_saved_theme(graph.settings.as_ref(), cx);
        crate::desktop::set_dock_icon(&graph.log);
        #[cfg(target_os = "linux")]
        {
            let log = graph.log.fork("desktop-entry");
            graph.tokio.spawn_blocking(move || {
                if let Err(error) = crate::desktop::sync_desktop_entry() {
                    log.warn("desktop entry install failed", Some(error.as_ref()));
                }
            });
        }
        let window_icon = crate::desktop::window_icon(&graph.log);
        let window_bounds = match dev_window_size() {
            Some(dev) => WindowBounds::centered(dev, cx),
            None => crate::desktop::initial_bounds(graph.settings.as_ref(), size(px(1180.), px(780.)), cx),
        };
        cx.set_global(graph);
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
            crate::desktop::install_pin_button(window, &cx.global::<AppGraph>().log.fork("pin"));
            let shell = cx.new(|cx| {
                let mut shell = Shell::new(window, cx);
                shell.apply_dev_route(window, cx);
                shell
            });
            cx.new(|cx| gpui_kit::base::Root::new(shell, window, cx).bg(transparent_black()))
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
