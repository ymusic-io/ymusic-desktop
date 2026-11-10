mod assets;
mod audio_effects;
mod download_dialog;
mod downloads;
mod downloads_view;
mod feed;
mod feedback_view;
mod graph;
mod http_client;
mod library;
mod local_engine;
mod media_session;
mod mini_player;
mod now_playing;
mod play_again;
mod play_history;
mod playback;
mod queue_nav;
mod search_view;
mod settings_view;
mod shell;
mod shortcuts;
mod signin;
mod signin_dialog;
mod sound_panel;
mod sponsor_block;
mod telemetry;
mod video_layer;
mod youtube_session;

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
    let tokio =
        tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("ym-core").build().expect("tokio runtime");
    let http = HttpClients::new().expect("HTTP client");
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

    application().with_assets(crate::assets::AppAssets).with_http_client(images).run(move |cx| {
        init(cx);
        crate::shortcuts::bind(cx);
        // No enter/exit animations anywhere: dialogs, popovers, menus and tabs change in one frame.
        cx.set_reduce_motion(true);
        let theme = graph.settings.get_raw(crate::settings_view::THEME_KEY).ok().flatten().unwrap_or_default();
        crate::settings_view::apply_theme(&theme, cx);
        cx.set_global(graph);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(dev_window_size().unwrap_or(size(px(1180.), px(780.))), cx)),
            titlebar: Some(TitlebarOptions { title: Some(t(Str::AppName).into()), ..Default::default() }),
            // Debug dev-route runs float, so a window launched from a terminal still draws frames
            // (a covered window gets none) for screenshots.
            kind: if cfg!(debug_assertions) && std::env::var_os("YMUSIC_DEV_ROUTE").is_some() {
                WindowKind::PopUp
            } else {
                WindowKind::Normal
            },
            // The video view sits behind gpui's, so the window and Root stay unpainted and the
            // Shell paints everything but the video box.
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
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
