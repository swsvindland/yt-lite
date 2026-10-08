//! Settings page. Every field reads and writes [`AppConfig`], which saves
//! `config.toml` (keeping its comments). Fields marked "after restart" are
//! read once at launch.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::group_box::GroupBoxVariant;
use gpui_kit::component::setting::{
    SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, Theme, ThemeMode, h_flex};
use gpui_kit::*;

use crate::app_state::{Account, AppConfig};

pub struct SettingsView;

fn choices(items: &[(&str, &str)]) -> Vec<(SharedString, SharedString)> {
    items
        .iter()
        .map(|(v, l)| {
            (
                SharedString::from(v.to_string()),
                SharedString::from(l.to_string()),
            )
        })
        .collect()
}

fn general() -> SettingPage {
    SettingPage::new("General")
        .icon(IconName::Settings)
        .default_open(true)
        .groups([
            SettingGroup::new()
                .title("Appearance")
                .items([SettingItem::new(
                    "Dark mode",
                    SettingField::switch(
                        |cx: &App| AppConfig::get(cx).ui.dark,
                        |dark: bool, cx: &mut App| {
                            AppConfig::update(cx, |c| c.ui.dark = dark);
                            Theme::change(
                                if dark {
                                    ThemeMode::Dark
                                } else {
                                    ThemeMode::Light
                                },
                                None,
                                cx,
                            );
                            cx.refresh_windows();
                        },
                    ),
                )]),
            SettingGroup::new().title("Feeds").items([
                SettingItem::new(
                    "Hide watched videos",
                    SettingField::switch(
                        |cx: &App| AppConfig::get(cx).feed.hide_watched,
                        |v: bool, cx: &mut App| AppConfig::update(cx, |c| c.feed.hide_watched = v),
                    ),
                ),
                SettingItem::new(
                    "Show uploads from the last",
                    SettingField::dropdown(
                        choices(&[
                            ("7", "Week"),
                            ("14", "2 weeks"),
                            ("30", "Month"),
                            ("60", "2 months"),
                            ("180", "6 months"),
                            ("365", "Year"),
                        ]),
                        |cx: &App| AppConfig::get(cx).feed.max_age_days.to_string().into(),
                        |v: SharedString, cx: &mut App| {
                            if let Ok(days) = v.parse() {
                                AppConfig::update(cx, |c| c.feed.max_age_days = days);
                            }
                        },
                    ),
                )
                .description("Subscriptions only; For you isn't limited by age."),
                SettingItem::new(
                    "Refresh every",
                    SettingField::dropdown(
                        choices(&[
                            ("15", "15 minutes"),
                            ("30", "30 minutes"),
                            ("60", "Hour"),
                            ("180", "3 hours"),
                        ]),
                        |cx: &App| {
                            AppConfig::get(cx)
                                .feed
                                .refresh_interval_minutes
                                .to_string()
                                .into()
                        },
                        |v: SharedString, cx: &mut App| {
                            if let Ok(m) = v.parse() {
                                AppConfig::update(cx, |c| c.feed.refresh_interval_minutes = m);
                            }
                        },
                    ),
                )
                .description("Applies after restart."),
            ]),
        ])
}

fn playback() -> SettingPage {
    SettingPage::new("Playback").icon(IconName::Play).groups([SettingGroup::new()
        .title("Player")
        .items([
            SettingItem::new(
                "Maximum quality",
                SettingField::dropdown(
                    choices(&[("720", "720p"), ("1080", "1080p"), ("1440", "1440p"), ("2160", "4K")]),
                    |cx: &App| AppConfig::get(cx).player.max_height.to_string().into(),
                    |v: SharedString, cx: &mut App| {
                        if let Ok(h) = v.parse() {
                            AppConfig::update(cx, |c| c.player.max_height = h);
                        }
                    },
                ),
            ),
            SettingItem::new(
                "Player",
                SettingField::dropdown(
                    choices(&[("system", "Built-in (AVPlayer / Windows)"), ("mpv", "mpv")]),
                    |cx: &App| AppConfig::get(cx).player.backend.clone().into(),
                    |v: SharedString, cx: &mut App| AppConfig::update(cx, |c| c.player.backend = v.to_string()),
                ),
            )
            .description("The built-in player needs nothing installed. mpv supports SponsorBlock. Applies after restart."),
            SettingItem::new(
                "SponsorBlock",
                SettingField::switch(
                    |cx: &App| AppConfig::get(cx).player.sponsorblock,
                    |v: bool, cx: &mut App| AppConfig::update(cx, |c| c.player.sponsorblock = v),
                ),
            )
            .description("mpv only: loads sponsorblock.lua from the config folder. Applies after restart."),
        ])])
}

fn account() -> SettingPage {
    SettingPage::new("Account")
        .icon(IconName::CircleUser)
        .groups([
        SettingGroup::new()
            .title("YouTube")
            .items([SettingItem::render(|options, _, cx| {
                let account = Account::get(cx);
                let (label, button) = if account.signed_in {
                    (
                        "Signed in. Subscriptions load from your account.",
                        Button::new("sign-out")
                            .outline()
                            .label("Sign out")
                            .with_size(options.size())
                            .on_click(|_, window, cx| Account::sign_out(window, cx)),
                    )
                } else {
                    (
                        "Not signed in.",
                        Button::new("sign-in")
                            .primary()
                            .icon(IconName::CircleUser)
                            .label("Sign in with Google")
                            .loading(account.signing_in)
                            .with_size(options.size())
                            .on_click(|_, window, cx| Account::sign_in(window, cx)),
                    )
                };
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_4()
                    .child(div().text_sm().child(label))
                    .child(button)
                    .into_any_element()
            })]),
        SettingGroup::new()
            .title("Google OAuth client")
            .items([
                SettingItem::new(
                    "Client ID",
                    SettingField::input(
                        |cx: &App| AppConfig::get(cx).google.client_id.clone().into(),
                        |v: SharedString, cx: &mut App| {
                            AppConfig::update(cx, |c| c.google.client_id = v.trim().to_string())
                        },
                    ),
                )
                .layout(Axis::Vertical),
                SettingItem::new(
                    "Client secret",
                    SettingField::input(
                        |cx: &App| AppConfig::get(cx).google.client_secret.clone().into(),
                        |v: SharedString, cx: &mut App| {
                            AppConfig::update(cx, |c| c.google.client_secret = v.trim().to_string())
                        },
                    ),
                )
                .layout(Axis::Vertical),
            ])
            .footer(
                |_, _| "A \"Desktop app\" client from Google Cloud Console. Applies after restart.",
            ),
    ])
}

fn storage() -> SettingPage {
    SettingPage::new("Storage")
        .icon(IconName::HardDrive)
        .groups([SettingGroup::new().title("Cache and memory").items([
            SettingItem::new(
                "Thumbnail memory",
                SettingField::dropdown(
                    choices(&[("24", "24 MB"), ("50", "50 MB"), ("100", "100 MB")]),
                    |cx: &App| AppConfig::get(cx).cache.thumb_memory_mb.to_string().into(),
                    |v: SharedString, cx: &mut App| {
                        if let Ok(mb) = v.parse() {
                            AppConfig::update(cx, |c| c.cache.thumb_memory_mb = mb);
                        }
                    },
                ),
            )
            .description("Decoded thumbnails kept in RAM. Applies after restart."),
            SettingItem::new(
                "Thumbnail disk cache",
                SettingField::dropdown(
                    choices(&[("100", "100 MB"), ("300", "300 MB"), ("1000", "1 GB")]),
                    |cx: &App| AppConfig::get(cx).cache.thumb_disk_mb.to_string().into(),
                    |v: SharedString, cx: &mut App| {
                        if let Ok(mb) = v.parse() {
                            AppConfig::update(cx, |c| c.cache.thumb_disk_mb = mb);
                        }
                    },
                ),
            ),
            SettingItem::new(
                "Show memory usage",
                SettingField::switch(
                    |cx: &App| AppConfig::get(cx).ui.show_memory,
                    |v: bool, cx: &mut App| AppConfig::update(cx, |c| c.ui.show_memory = v),
                ),
            )
            .description("In the sidebar."),
            SettingItem::new(
                "Config folder",
                SettingField::render(|options, _, _| {
                    Button::new("open-config")
                        .outline()
                        .icon(IconName::FolderOpen)
                        .label("Open")
                        .with_size(options.size())
                        .on_click(|_, _, cx| {
                            let dir = cx.global::<AppConfig>().paths.config_dir.clone();
                            if let Err(e) = open::that_detached(&dir) {
                                log::error!("opening {}: {e}", dir.display());
                            }
                        })
                }),
            ),
        ])])
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(cx.theme().background).child(
            Settings::new("settings")
                .with_group_variant(GroupBoxVariant::Outline)
                .pages([general(), playback(), account(), storage()]),
        )
    }
}
