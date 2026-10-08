#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app_state;
mod mem;
mod player;
mod system_player;
mod thumbs;
mod ui;

// Core modules, reachable as `crate::config` etc. inside this crate.
use yt_lite_core::{auth, config, db, feed, net, youtube};

use std::sync::Arc;

use anyhow::Context as _;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

use auth::Auth;
use config::{Config, Paths};
use db::Db;
use feed::pipeline::Services;
use net::Http;
use player::Player;
use ui::app_view::AppView;
use yt_lite_core::resolve::StreamResolver;
use yt_lite_core::resolve::innertube::InnertubeResolver;

actions!(
    yt_lite,
    [
        Quit,
        CloseWindow,
        Refresh,
        ToggleHideWatched,
        OpenConfigFolder
    ]
);

/// Menu bar (macOS) and shortcuts. `secondary` is Cmd on macOS, Ctrl elsewhere.
fn init_menus(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(w) = cx.active_window() {
            w.update(cx, |_, window, _| window.remove_window()).ok();
        }
    });
    cx.bind_keys([
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-w", CloseWindow, None),
        KeyBinding::new("secondary-r", Refresh, None),
        KeyBinding::new("f5", Refresh, None),
        KeyBinding::new("secondary-shift-h", ToggleHideWatched, None),
        KeyBinding::new("secondary-,", OpenConfigFolder, None),
    ]);
    cx.set_menus([
        Menu::new("yt-lite").items([
            MenuItem::action("Settings…", OpenConfigFolder),
            MenuItem::separator(),
            MenuItem::action("Quit yt-lite", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Close Window", CloseWindow)]),
        Menu::new("View").items([
            MenuItem::action("Refresh", Refresh),
            MenuItem::action("Hide/Show Watched", ToggleHideWatched),
        ]),
    ]);
    // Single-window app: closing it quits (macOS would otherwise keep running).
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}

fn init_logging(paths: &Paths) {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,naga=warn,wgpu=warn"),
    );
    // Windows release builds have no console, and a macOS .app launched from
    // Finder has no visible stderr: log to a file instead.
    let in_app_bundle =
        std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"));
    if cfg!(all(windows, not(debug_assertions))) || in_app_bundle {
        let _ = std::fs::create_dir_all(&paths.data_dir);
        if let Ok(file) = std::fs::File::create(paths.data_dir.join("yt-lite.log")) {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
        }
    }
    builder.init();
}

fn main() -> anyhow::Result<()> {
    let paths = Paths::resolve()?;
    init_logging(&paths);
    let config = Config::load_or_init(&paths.config_file)?;
    log::info!("config: {}", paths.config_file.display());

    let http = Http::new();
    let db = Db::open(&paths.db_file()).context("opening cache database")?;
    let native: Option<Arc<dyn StreamResolver>> = (config.player.resolver != "yt-dlp")
        .then(|| Arc::new(InnertubeResolver::new(http.clone(), Some(db.clone()))) as _);
    let player = Player::detect(&config.player, &paths, native);
    let dark = config.ui.dark;
    let services = Arc::new(Services {
        auth: Auth::new(config.google.clone(), http.clone()),
        http,
        db,
        config: config.clone(),
    });
    mem::log_now("startup");

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::change(
                if dark {
                    ThemeMode::Dark
                } else {
                    ThemeMode::Light
                },
                None,
                cx,
            );
            cx.set_global(app_state::AppConfig { config, paths });
            cx.set_global(app_state::AppServices(services));
            cx.set_global(app_state::Account::default());
            init_menus(cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1440.), px(920.)), cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some("yt-lite".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| AppView::new(player, window, cx))
            })
            .expect("failed to open window");
            cx.activate(true);
        });
    Ok(())
}
