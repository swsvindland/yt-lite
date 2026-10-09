//! Window root: sidebar navigation plus the active page.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::sidebar::{
    Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::feed_view::{FeedEvent, FeedView, SIDEBAR_W, Shared};
use super::settings_view::SettingsView;
use super::status::MemoryIndicator;
use super::thumb_store::ThumbStore;
use crate::app_state::{Account, AppConfig, AppServices};
use crate::feed::foryou::ForYouSource;
use crate::feed::query::QuerySource;
use crate::feed::subscriptions::SubscriptionsSource;
use crate::player::Player;
use crate::system_player::SystemPlayer;
use crate::thumbs::Thumbnails;
use crate::{FocusSearch, OpenConfigFolder, Refresh, ToggleHideWatched};
use yt_lite_core::progress::Progress;

/// How often playback positions are collected from the players and saved.
const SAVE_PROGRESS_EVERY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Subscriptions,
    ForYou,
    Explore,
    Search,
    Settings,
}

pub struct AppView {
    page: Page,
    subscriptions: Entity<FeedView>,
    for_you: Entity<FeedView>,
    explore: Entity<FeedView>,
    search: Entity<FeedView>,
    settings: Entity<SettingsView>,
    memory: Entity<MemoryIndicator>,
    thumbs: Entity<ThumbStore>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _save_progress: Task<()>,
}

impl AppView {
    pub fn new(player: Player, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let services = AppServices::get(cx);
        let paths = cx.global::<AppConfig>().paths.clone();
        let thumbs = cx.new(|cx| {
            ThumbStore::new(
                Thumbnails::new(
                    paths.thumbs_dir(),
                    services.config.cache.thumb_memory_mb * 1024 * 1024,
                ),
                services.http.clone(),
                window,
                cx,
            )
        });
        let shared = Shared {
            player: Arc::new(player),
            system_player: Rc::new(RefCell::new(SystemPlayer::default())),
            thumbs: thumbs.clone(),
        };
        let subscriptions =
            cx.new(|cx| FeedView::new(Arc::new(SubscriptionsSource), shared.clone(), window, cx));
        let for_you =
            cx.new(|cx| FeedView::new(Arc::new(ForYouSource), shared.clone(), window, cx));
        let explore = cx.new(|cx| {
            FeedView::new_query(Arc::new(QuerySource::explore()), shared.clone(), window, cx)
        });
        let save_progress = Self::save_progress(&shared, cx);
        let search =
            cx.new(|cx| FeedView::new_query(Arc::new(QuerySource::search()), shared, window, cx));
        let settings = cx.new(|_| SettingsView);
        let memory = cx.new(MemoryIndicator::new);

        let mut subs = Vec::new();
        for feed in [&subscriptions, &for_you, &explore, &search] {
            subs.push(
                cx.subscribe_in(feed, window, |this, _, event, window, cx| match event {
                    FeedEvent::OpenSettings => this.show(Page::Settings, window, cx),
                    FeedEvent::Played => {
                        this.for_you.update(cx, |v, _| v.stale = true);
                    }
                }),
            );
        }
        subs.push(cx.observe(&thumbs, |_, _, cx| cx.notify()));
        subs.push(cx.observe_global::<Account>(|_, cx| cx.notify()));
        subs.push(cx.observe_global::<AppConfig>(|_, cx| cx.notify()));

        // Prune the thumbnail disk cache and learn the sign-in state off the UI thread.
        let disk_cap = services.config.cache.thumb_disk_mb * 1024 * 1024;
        let thumbs_dir = paths.thumbs_dir();
        cx.spawn(async move |_, cx| {
            let signed_in = smol::unblock(move || {
                match yt_lite_core::thumbcache::prune_disk(&thumbs_dir, disk_cap) {
                    Ok(n) if n > 0 => log::info!("pruned {n} cached thumbnails"),
                    Err(e) => log::warn!("thumbnail prune failed: {e:#}"),
                    _ => {}
                }
                services.auth.is_signed_in()
            })
            .await;
            cx.update(|cx| cx.update_global::<Account, _>(|a, _| a.signed_in = signed_in));
        })
        .detach();

        let focus = cx.focus_handle();
        focus.focus(window, cx);
        // `YT_LITE_PAGE=foryou|settings`: start on that page (testing aid).
        let page = match std::env::var("YT_LITE_PAGE").as_deref() {
            Ok("foryou") => Page::ForYou,
            Ok("settings") => Page::Settings,
            Ok("explore") => Page::Explore,
            Ok("search") => Page::Search,
            _ => Page::Subscriptions,
        };
        let mut this = Self {
            page,
            subscriptions,
            for_you,
            explore,
            search,
            settings,
            memory,
            thumbs,
            focus,
            _subscriptions: subs,
            _save_progress: save_progress,
        };
        // `YT_LITE_SEARCH=<query>`: open Search with this query (testing aid).
        if let Some(q) = std::env::var("YT_LITE_SEARCH").ok().filter(|s| !s.is_empty()) {
            this.page = Page::Search;
            this.search.update(cx, |v, cx| v.search_for(&q, window, cx));
        }
        // `YT_LITE_AUTOPLAY=<video id>`: play on startup (testing aid).
        if let Some(id) = std::env::var("YT_LITE_AUTOPLAY")
            .ok()
            .filter(|s| !s.is_empty())
        {
            this.subscriptions.update(cx, |v, cx| {
                v.play_video(id.into(), "Autoplay".into(), false, window, cx)
            });
        }
        this
    }

    /// Saves where the players got to every few seconds: resume points, and
    /// watched once a video has played to the end. Feeds reload when a video
    /// is closed or finished, so their progress bars catch up.
    fn save_progress(shared: &Shared, cx: &mut Context<Self>) -> Task<()> {
        let system_player = shared.system_player.clone();
        let player = shared.player.clone();
        let db = AppServices::get(cx).db.clone();
        cx.spawn(async move |this, cx| {
            // Last saved position per video, to skip ones that haven't moved.
            let mut saved: HashMap<String, f64> = HashMap::new();
            loop {
                cx.background_executor().timer(SAVE_PROGRESS_EVERY).await;
                let mut positions = system_player.borrow_mut().poll();
                positions.extend(player.take_positions());
                positions.retain(|p| {
                    p.done
                        || saved
                            .get(&p.video_id)
                            .is_none_or(|s| (s - p.secs).abs() >= 1.0)
                });
                if positions.is_empty() {
                    continue;
                }
                for p in &positions {
                    if p.done {
                        saved.remove(&p.video_id);
                    } else {
                        saved.insert(p.video_id.clone(), p.secs);
                    }
                }
                let db = db.clone();
                let changed = smol::unblock(move || {
                    let mut changed = false;
                    for p in positions {
                        match db.save_progress(&p.video_id, p.secs, p.duration) {
                            Ok(progress) => {
                                changed |= p.done || progress == Some(Progress::Finished)
                            }
                            Err(e) => log::error!("saving progress of {}: {e:#}", p.video_id),
                        }
                    }
                    changed
                })
                .await;
                if changed && this.update(cx, |this, cx| this.reload_feeds(cx)).is_err() {
                    break;
                }
            }
        })
    }

    fn reload_feeds(&mut self, cx: &mut Context<Self>) {
        for feed in [
            &self.subscriptions,
            &self.for_you,
            &self.explore,
            &self.search,
        ] {
            feed.update(cx, |v, cx| v.reload(cx));
        }
    }

    fn show(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        if let Some(feed) = self.active_feed() {
            feed.update(cx, |v, cx| v.activate(window, cx));
        }
        if page == Page::Search {
            self.search.update(cx, |v, cx| v.focus_search(window, cx));
        }
        cx.notify();
    }

    fn active_feed(&self) -> Option<Entity<FeedView>> {
        match self.page {
            Page::Subscriptions => Some(self.subscriptions.clone()),
            Page::ForYou => Some(self.for_you.clone()),
            Page::Explore => Some(self.explore.clone()),
            Page::Search => Some(self.search.clone()),
            Page::Settings => None,
        }
    }

    fn nav_item(
        &self,
        label: &'static str,
        icon: IconName,
        page: Page,
        cx: &mut Context<Self>,
    ) -> SidebarMenuItem {
        SidebarMenuItem::new(label)
            .icon(icon)
            .active(self.page == page)
            .on_click(cx.listener(move |this, _, window, cx| this.show(page, window, cx)))
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let nav_subs = self.nav_item("Subscriptions", IconName::Inbox, Page::Subscriptions, cx);
        let nav_for_you = self.nav_item("For you", IconName::Star, Page::ForYou, cx);
        let nav_explore = self.nav_item("Explore", IconName::Globe, Page::Explore, cx);
        let nav_search = self.nav_item("Search", IconName::Search, Page::Search, cx);
        let nav_settings = self.nav_item("Settings", IconName::Settings, Page::Settings, cx);
        let theme = cx.theme();
        let account = Account::get(cx);
        let show_memory = AppConfig::get(cx).ui.show_memory;
        let thumbs = self.thumbs.read(cx);
        let thumb_line = format!(
            "{} thumbs · {:.0} MB",
            thumbs.len(),
            crate::mem::mb(thumbs.memory_bytes())
        );

        let logo = h_flex()
            .gap_2()
            .items_center()
            .child(
                div()
                    .size(px(28.))
                    .rounded_lg()
                    .bg(rgb(0xff0033))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::Play).small().text_color(white())),
            )
            .child(
                div()
                    .text_base()
                    .font_weight(FontWeight::BOLD)
                    .child("yt-lite"),
            );

        let account_line = h_flex()
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().size(px(8.)).rounded_full().bg(if account.signed_in {
                rgb(0x22c55e)
            } else {
                rgb(0x9ca3af)
            }))
            .child(if account.signed_in {
                "Signed in"
            } else {
                "Not signed in"
            });

        Sidebar::new("nav")
            .w(px(SIDEBAR_W))
            .header(SidebarHeader::new().child(logo))
            .child(
                SidebarGroup::new("Watch").child(
                    SidebarMenu::new()
                        .child(nav_subs)
                        .child(nav_for_you)
                        .child(nav_explore)
                        .child(nav_search),
                ),
            )
            .child(SidebarGroup::new("App").child(SidebarMenu::new().child(nav_settings)))
            .footer(
                SidebarFooter::new().child(v_flex().gap_1().child(account_line).when(
                    show_memory,
                    |d| {
                        d.child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(Icon::new(IconName::MemoryStick).xsmall())
                                .child(self.memory.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(thumb_line),
                        )
                    },
                )),
            )
    }
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page: AnyElement = match self.page {
            Page::Subscriptions => self.subscriptions.clone().into_any_element(),
            Page::ForYou => self.for_you.clone().into_any_element(),
            Page::Explore => self.explore.clone().into_any_element(),
            Page::Search => self.search.clone().into_any_element(),
            Page::Settings => self.settings.clone().into_any_element(),
        };
        let theme = cx.theme();
        h_flex()
            .track_focus(&self.focus)
            .key_context("AppView")
            .on_action(cx.listener(|this, _: &Refresh, window, cx| {
                if let Some(feed) = this.active_feed() {
                    feed.update(cx, |v, cx| v.refresh(window, cx));
                }
            }))
            .on_action(cx.listener(|_, _: &ToggleHideWatched, _, cx| {
                AppConfig::update(cx, |c| c.feed.hide_watched = !c.feed.hide_watched);
            }))
            .on_action(
                cx.listener(|this, _: &FocusSearch, window, cx| {
                    this.show(Page::Search, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &OpenConfigFolder, window, cx| {
                this.show(Page::Settings, window, cx)
            }))
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.render_sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().child(page))
    }
}
