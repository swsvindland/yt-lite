#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod auth;
mod config;
mod db;
mod feed;
mod lru;
mod mem;
mod net;
mod player;
mod shorts;
mod thumbs;
mod ui;
mod youtube;

use std::sync::Arc;

use anyhow::Context as _;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

use auth::Auth;
use config::{Config, Paths};
use db::Db;
use feed::pipeline::Services;
use feed::subscriptions::SubscriptionsSource;
use net::Http;
use player::Player;
use ui::feed_view::FeedView;

fn init_logging(paths: &Paths) {
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,naga=warn,wgpu=warn"),
    );
    // Release builds on Windows have no console; log to a file instead.
    if cfg!(all(windows, not(debug_assertions))) {
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
    let player = Player::detect(&config.player, &paths);
    let dark = config.ui.dark;
    let services = Arc::new(Services {
        auth: Auth::new(config.google.clone(), http.clone()),
        http,
        db,
        config,
    });
    mem::log_now("startup");

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1400.), px(900.)), cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some("yt-lite".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| {
                    FeedView::new(
                        services,
                        paths,
                        player,
                        Arc::new(SubscriptionsSource),
                        window,
                        cx,
                    )
                })
            })
            .expect("failed to open window");
            cx.activate(true);
        });
    Ok(())
}
