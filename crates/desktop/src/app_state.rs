//! App-wide state shared by the views, as GPUI globals.

use std::sync::Arc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::*;

use crate::config::{Config, Paths};
use crate::feed::pipeline::Services;

/// The live configuration. Edits go through [`AppConfig::update`], which saves
/// `config.toml` and notifies observers.
pub struct AppConfig {
    pub config: Config,
    pub paths: Paths,
}

impl Global for AppConfig {}

impl AppConfig {
    pub fn get(cx: &App) -> &Config {
        &cx.global::<AppConfig>().config
    }

    pub fn update(cx: &mut App, f: impl FnOnce(&mut Config)) {
        cx.update_global::<AppConfig, _>(|g, _| {
            f(&mut g.config);
            if let Err(e) = g.config.save(&g.paths.config_file) {
                log::error!("saving config: {e:#}");
            }
        });
    }
}

/// Network, database and auth handles (started with the config at launch).
pub struct AppServices(pub Arc<Services>);

impl Global for AppServices {}

impl AppServices {
    pub fn get(cx: &App) -> Arc<Services> {
        cx.global::<AppServices>().0.clone()
    }
}

#[derive(Default)]
pub struct Account {
    pub signed_in: bool,
    pub signing_in: bool,
}

impl Global for Account {}

impl Account {
    pub fn get(cx: &App) -> &Account {
        cx.global::<Account>()
    }

    /// Opens the browser for Google sign-in and waits for it off the UI thread.
    pub fn sign_in(window: &mut Window, cx: &mut App) {
        if !AppConfig::get(cx).has_google_client() {
            window.push_notification(
                Notification::warning(
                    "Enter your Google OAuth client ID and secret above, then restart yt-lite.",
                )
                .title("Google client not configured"),
                cx,
            );
            return;
        }
        if Account::get(cx).signing_in {
            return;
        }
        cx.update_global::<Account, _>(|a, _| a.signing_in = true);
        let services = AppServices::get(cx);
        let handle = window.window_handle();
        cx.spawn(async move |cx| {
            let result = smol::unblock(move || services.auth.sign_in()).await;
            cx.update(|cx| {
                cx.update_global::<Account, _>(|a, _| {
                    a.signing_in = false;
                    a.signed_in = result.is_ok();
                });
                if let Err(e) = result {
                    handle
                        .update(cx, |_, window, cx| {
                            window.push_notification(
                                Notification::error(format!("{e:#}")).title("Sign-in failed"),
                                cx,
                            );
                        })
                        .ok();
                }
            });
        })
        .detach();
    }

    pub fn sign_out(window: &mut Window, cx: &mut App) {
        if let Err(e) = AppServices::get(cx).auth.sign_out() {
            window.push_notification(Notification::error(format!("{e:#}")), cx);
        }
        cx.update_global::<Account, _>(|a, _| a.signed_in = false);
    }
}
