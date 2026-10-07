#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod lru;
mod player;
mod shorts;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, notification::Notification, v_flex};
use gpui_kit::*;

use config::{Config, Paths};
use player::Player;

const DEMO: &[(&str, &str)] = &[
    ("3iRUwVzRDZQ", "Xiaomi 18 Fold: How Does This Happen?"),
    ("e1q-TuHdc4Y", "Dear YouTube!"),
    ("dQw4w9WgXcQ", "Rick Astley - Never Gonna Give You Up"),
];

struct DemoView {
    player: Player,
}

impl DemoView {
    fn play(&mut self, id: &str, title: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = format!("https://www.youtube.com/watch?v={id}");
        if let Err(e) = self.player.play(&url, title) {
            window.push_notification(Notification::error(e.to_string()), cx);
        }
    }
}

impl Render for DemoView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = DEMO.iter().map(|&(id, title)| {
            h_flex()
                .gap_3()
                .p_2()
                .child(
                    Button::new(SharedString::from(id))
                        .primary()
                        .label("Play")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.play(id, title, window, cx)
                        })),
                )
                .child(title)
        });
        v_flex()
            .size_full()
            .p_4()
            .gap_2()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .children(rows)
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let paths = Paths::resolve()?;
    let config = Config::load_or_init(&paths.config_file)?;
    let player = Player::detect(&config.player, &paths);

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
                cx.new(|_| DemoView { player })
            })
            .expect("failed to open window");
            cx.activate(true);
        });
    Ok(())
}
