mod assets;
mod feed;
mod graph;
mod http_client;
mod mini_player;
mod playback;
mod shell;

use std::sync::Arc;

use gpui_kit::*;
use ym_common::http::HttpClients;
use ym_common::log::{Level, Logger, StderrSink};
use ym_i18n::{Str, t};

use crate::graph::AppGraph;
use crate::http_client::ReqwestHttpClient;
use crate::shell::Shell;

fn main() {
    ym_i18n::use_system_locale();
    let log = Logger::new(Arc::new(StderrSink { min_level: Level::Info }));
    let tokio =
        tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("ym-core").build().expect("tokio runtime");
    let http = HttpClients::new().expect("HTTP client");
    let data_dir = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("YMusicRust");
    let graph = match tokio.block_on(AppGraph::boot(tokio.handle().clone(), log.clone(), http.clone(), data_dir)) {
        Ok(graph) => graph,
        Err(error) => {
            log.capture_error("engine boot failed", &error);
            eprintln!("{}", crate::feed::error_text(&error));
            std::process::exit(1);
        }
    };
    let images = ReqwestHttpClient::new(http.following.clone(), tokio.handle().clone());

    application().with_assets(crate::assets::AppAssets).with_http_client(images).run(move |cx| {
        init(cx);
        cx.set_global(graph);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1180.), px(780.)), cx)),
            titlebar: Some(TitlebarOptions { title: Some(t(Str::AppName).into()), ..Default::default() }),
            // Debug dev-route runs float, so a window launched from a terminal still draws frames
            // (a covered window gets none) for screenshots.
            kind: if cfg!(debug_assertions) && std::env::var_os("YMUSIC_DEV_ROUTE").is_some() {
                WindowKind::PopUp
            } else {
                WindowKind::Normal
            },
            ..Default::default()
        };
        open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                let mut shell = Shell::new(window, cx);
                shell.apply_dev_route(window, cx);
                shell
            })
        })
        .expect("open main window");
        cx.activate(true);
    });
    drop(tokio);
}
