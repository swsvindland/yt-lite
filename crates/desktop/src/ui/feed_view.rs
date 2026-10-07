//! The subscriptions grid.
//!
//! Virtualized with GPUI's `uniform_list`: each list item is one row of cards,
//! and the number of columns follows the window width. Only visible rows are
//! built, and only visible cards request thumbnails.

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use futures::future::{Either, select};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use smol::lock::Semaphore;

use super::status::MemoryIndicator;
use super::{format_age, now_unix};
use crate::auth::NotSignedIn;
use crate::config::Paths;
use crate::db::{FeedQuery, VideoRow};
use crate::feed::FeedSource;
use crate::feed::pipeline::{self, Services};
use crate::player::{Method, Player};
use crate::thumbs::{self, Thumbnails};
use crate::youtube::duration::format_clock;
use crate::{OpenConfigFolder, Refresh, ToggleHideWatched};

const CARD_W: f32 = 320.;
const THUMB_H: f32 = 180.;
const GAP: f32 = 16.;
const SIDE_PAD: f32 = 16.;
const MAX_PARALLEL_THUMBS: usize = 8;
/// Decoded thumbnails are dropped after the window has been inactive this long.
const IDLE_TRIM_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

struct Card {
    id: SharedString,
    title: SharedString,
    channel: SharedString,
    published: i64,
    duration: Option<SharedString>,
    live: bool,
    watched: bool,
}

impl From<VideoRow> for Card {
    fn from(v: VideoRow) -> Self {
        Self {
            duration: v.duration_secs.filter(|s| *s > 0).map(|s| format_clock(s).into()),
            id: v.id.into(),
            title: v.title.into(),
            channel: v.channel_title.into(),
            published: v.published,
            live: v.live,
            watched: v.watched,
        }
    }
}

pub struct FeedView {
    services: Arc<Services>,
    paths: Paths,
    player: Arc<Player>,
    source: Arc<dyn FeedSource>,
    cards: Rc<Vec<Card>>,
    thumbs: Thumbnails,
    thumb_permits: Arc<Semaphore>,
    status: SharedString,
    refreshing: bool,
    signing_in: bool,
    signed_in: bool,
    hide_watched: bool,
    loaded_once: bool,
    scroll_test_started: bool,
    scroll: UniformListScrollHandle,
    memory: Option<Entity<MemoryIndicator>>,
    idle_trim: Option<Task<()>>,
    _activation: Subscription,
    focus: FocusHandle,
    _timer: Task<()>,
}

impl FeedView {
    pub fn new(
        services: Arc<Services>,
        paths: Paths,
        player: Player,
        source: Arc<dyn FeedSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let interval = services.config.feed.refresh_interval();
        let timer = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                if this.update_in(cx, |v, window, cx| v.refresh(window, cx)).is_err() {
                    break;
                }
            }
        });
        let memory = services
            .config
            .ui
            .show_memory
            .then(|| cx.new(MemoryIndicator::new));
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.idle_trim = None;
            } else {
                this.idle_trim = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(IDLE_TRIM_AFTER).await;
                    this.update(cx, |v, cx| v.trim_thumbnails(cx)).ok();
                }));
            }
        });
        let thumbs = Thumbnails::new(
            paths.thumbs_dir(),
            services.config.cache.thumb_memory_mb * 1024 * 1024,
        );
        let mut this = Self {
            hide_watched: services.config.feed.hide_watched,
            signed_in: false,
            services,
            paths,
            player: Arc::new(player),
            source,
            cards: Rc::new(Vec::new()),
            thumbs,
            thumb_permits: Arc::new(Semaphore::new(MAX_PARALLEL_THUMBS)),
            status: "".into(),
            refreshing: false,
            signing_in: false,
            loaded_once: false,
            scroll_test_started: false,
            scroll: UniformListScrollHandle::new(),
            memory,
            idle_trim: None,
            _activation: activation,
            focus: cx.focus_handle(),
            _timer: timer,
        };
        this.focus.focus(window, cx);
        this.startup(window, cx);
        this
    }

    /// Shows the cached feed immediately, then refreshes if it's stale.
    fn startup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let services = self.services.clone();
        let kind = self.source.kind();
        let thumbs_dir = self.thumbs.dir().to_path_buf();
        let disk_cap = services.config.cache.thumb_disk_mb * 1024 * 1024;
        cx.spawn_in(window, async move |this, cx| {
            let (signed_in, last) = smol::unblock(move || {
                match yt_lite_core::thumbcache::prune_disk(&thumbs_dir, disk_cap) {
                    Ok(n) if n > 0 => log::info!("pruned {n} cached thumbnails"),
                    Err(e) => log::warn!("thumbnail prune failed: {e:#}"),
                    _ => {}
                }
                let last = services
                    .db
                    .meta_get(&format!("last_refresh.{}", kind.as_str()))
                    .ok()
                    .flatten()
                    .and_then(|s| s.parse::<i64>().ok());
                (services.auth.is_signed_in(), last)
            })
            .await;
            let stale = last.is_none_or(|t| {
                now_unix() - t
                    >= this
                        .read_with(cx, |v, _| v.services.config.feed.refresh_interval().as_secs() as i64)
                        .unwrap_or(0)
            });
            this.update_in(cx, |v, window, cx| {
                v.signed_in = signed_in;
                v.reload(cx);
                if stale {
                    v.refresh(window, cx);
                } else {
                    v.maybe_scroll_test(window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn query(&self) -> FeedQuery {
        let f = &self.services.config.feed;
        FeedQuery {
            kind: self.source.kind(),
            max_age_days: f.max_age_days,
            limit: f.max_items,
            hide_watched: self.hide_watched,
        }
    }

    /// Re-reads the feed from SQLite (off the UI thread).
    fn reload(&mut self, cx: &mut Context<Self>) {
        let db = self.services.db.clone();
        let q = self.query();
        cx.spawn(async move |this, cx| {
            let rows = smol::unblock(move || db.feed(q)).await;
            this.update(cx, |v, cx| {
                match rows {
                    Ok(rows) => {
                        v.cards = Rc::new(rows.into_iter().map(Card::from).collect());
                        v.loaded_once = true;
                    }
                    Err(e) => v.status = format!("Database error: {e:#}").into(),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refreshing || self.signing_in {
            return;
        }
        self.refreshing = true;
        self.status = "Refreshing…".into();
        self.thumbs.clear_failures();
        cx.notify();

        let services = self.services.clone();
        let source = self.source.clone();
        let (tx, rx) = smol::channel::unbounded::<String>();
        cx.spawn_in(window, async move |this, cx| {
            let job = smol::unblock(move || {
                let progress = move |msg: String| {
                    let _ = tx.try_send(msg);
                };
                pipeline::refresh(&*source, &services, &progress)
            });
            let mut job = std::pin::pin!(job);
            // Forward progress messages until the job finishes.
            let result = loop {
                let next = std::pin::pin!(rx.recv());
                match select(job.as_mut(), next).await {
                    Either::Left((result, _)) => break result,
                    Either::Right((Ok(msg), _)) => {
                        this.update(cx, |v, cx| {
                            v.status = msg.into();
                            cx.notify();
                        })
                        .ok();
                    }
                    Either::Right((Err(_), _)) => break job.await,
                }
            };
            crate::mem::log_now("after refresh");
            this.update_in(cx, |v, window, cx| {
                v.refreshing = false;
                match result {
                    Ok(stats) => {
                        v.signed_in = v.services.auth.is_signed_in();
                        v.status = format!(
                            "Updated {} · {stats}",
                            chrono::Local::now().format("%H:%M")
                        )
                        .into();
                    }
                    Err(e) if e.downcast_ref::<NotSignedIn>().is_some() => {
                        v.signed_in = false;
                        v.status = "Sign in with Google to load your subscriptions.".into();
                    }
                    Err(e) => {
                        log::error!("refresh failed: {e:#}");
                        v.status = "Refresh failed".into();
                        window.push_notification(
                            Notification::error(format!("{e:#}")).title("Refresh failed"),
                            cx,
                        );
                    }
                }
                v.reload(cx);
                v.maybe_scroll_test(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// `YT_LITE_SCROLL_TEST=1`: after the first refresh, scroll through the
    /// whole grid twice and log memory, to verify the memory budget with a
    /// full thumbnail cache.
    fn maybe_scroll_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scroll_test_started || std::env::var_os("YT_LITE_SCROLL_TEST").is_none() {
            return;
        }
        self.scroll_test_started = true;
        let scroll = self.scroll.clone();
        cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let wait = |ms| executor.timer(std::time::Duration::from_millis(ms));
            wait(2000).await;
            crate::mem::log_now("scroll test start");
            for pass in 1..=2 {
                let Ok(rows) =
                    this.update_in(cx, |v, window, _| v.cards.len().div_ceil(v.columns(window)))
                else {
                    return;
                };
                for row in 0..rows {
                    scroll.scroll_to_item(row, ScrollStrategy::Top);
                    this.update(cx, |_, cx| cx.notify()).ok();
                    wait(250).await;
                }
                scroll.scroll_to_item(0, ScrollStrategy::Top);
                this.update(cx, |_, cx| cx.notify()).ok();
                wait(2000).await;
                let stats = this
                    .read_with(cx, |v, _| (v.cards.len(), v.thumbs.len(), v.thumbs.memory_bytes()))
                    .unwrap_or_default();
                log::info!(
                    "scroll test pass {pass}: {rows} rows, {} cards, {} thumbs cached ({:.1} MB)",
                    stats.0,
                    stats.1,
                    crate::mem::mb(stats.2)
                );
                crate::mem::log_now(&format!("scroll test pass {pass}"));
            }
        })
        .detach();
    }

    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.services.config.has_google_client() {
            window.push_notification(
                Notification::warning(format!(
                    "Add your Google OAuth client_id and client_secret to {} and restart.",
                    self.paths.config_file.display()
                ))
                .title("Google client not configured"),
                cx,
            );
            return;
        }
        if self.signing_in {
            return;
        }
        self.signing_in = true;
        self.status = "Waiting for sign-in in your browser…".into();
        cx.notify();
        let services = self.services.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = smol::unblock(move || services.auth.sign_in()).await;
            this.update_in(cx, |v, window, cx| {
                v.signing_in = false;
                match result {
                    Ok(()) => {
                        v.signed_in = true;
                        v.refresh(window, cx);
                    }
                    Err(e) => {
                        v.status = "Sign-in failed".into();
                        window.push_notification(
                            Notification::error(format!("{e:#}")).title("Sign-in failed"),
                            cx,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = self.services.auth.sign_out() {
            window.push_notification(Notification::error(format!("{e:#}")), cx);
        }
        self.signed_in = false;
        self.status = "Signed out".into();
        cx.notify();
    }

    fn play(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = self.cards.get(ix) else { return };
        let (id, title) = (card.id.clone(), card.title.clone());
        let player = self.player.clone();
        self.status = format!("Opening “{title}”…").into();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let video_id = id.to_string();
            let result = smol::unblock(move || player.play(&video_id, &title)).await;
            this.update_in(cx, |v, window, cx| {
                match result {
                    Ok(method) => {
                        v.status = match method {
                            Method::Native => "Playing in mpv".into(),
                            Method::YtDlp => "Playing in mpv (via yt-dlp)".into(),
                        };
                        // The list may have been reloaded meanwhile; find by id.
                        if let Some(ix) = v.cards.iter().position(|c| c.id == id) {
                            v.set_watched(ix, true, cx);
                        }
                    }
                    Err(e) => {
                        v.status = "".into();
                        window.push_notification(
                            Notification::error(format!("{e:#}")).title("Can't play video"),
                            cx,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_watched(&mut self, ix: usize, watched: bool, cx: &mut Context<Self>) {
        let Some(cards) = Rc::get_mut(&mut self.cards) else {
            return;
        };
        let Some(card) = cards.get_mut(ix) else { return };
        card.watched = watched;
        let id = card.id.to_string();
        let db = self.services.db.clone();
        cx.background_spawn(async move {
            if let Err(e) = db.set_watched(&id, watched) {
                log::error!("set_watched {id}: {e:#}");
            }
        })
        .detach();
        cx.notify();
    }

    fn toggle_hide_watched(&mut self, cx: &mut Context<Self>) {
        self.hide_watched = !self.hide_watched;
        self.reload(cx);
    }

    fn request_thumb(&mut self, id: SharedString, window: &Window, cx: &mut Context<Self>) {
        if !self.thumbs.begin_load(&id) {
            return;
        }
        let http = self.services.http.clone();
        let dir = self.thumbs.dir().to_path_buf();
        let permits = self.thumb_permits.clone();
        // Decode at the physical display size.
        let scale = window.scale_factor();
        let (w, h) = ((CARD_W * scale) as u32, (THUMB_H * scale) as u32);
        cx.spawn(async move |this, cx| {
            let _permit = permits.acquire_arc().await;
            let id_for_load = id.clone();
            let result = smol::unblock(move || thumbs::load(&http, &dir, &id_for_load, w, h)).await;
            this.update(cx, |v, cx| {
                let evicted = v.thumbs.finish(id, result);
                for img in evicted {
                    cx.drop_image(img, None);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Releases all decoded thumbnails (CPU buffers and GPU atlas tiles).
    /// They reload from the disk cache when next shown.
    fn trim_thumbnails(&mut self, cx: &mut Context<Self>) {
        let images = self.thumbs.clear();
        let n = images.len();
        for img in images {
            cx.drop_image(img, None);
        }
        log::info!("idle: released {n} thumbnails");
        cx.notify();
        // Measure after the allocator has had a moment.
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            crate::mem::log_now("after idle trim");
        })
        .detach();
    }

    fn open_config_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.paths.config_file.parent().map(|p| p.to_path_buf());
        if let Some(dir) = dir
            && let Err(e) = open::that_detached(&dir)
        {
            window.push_notification(Notification::error(e.to_string()), cx);
        }
    }

    fn columns(&self, window: &Window) -> usize {
        let width = f32::from(window.viewport_size().width) - SIDE_PAD * 2.;
        (((width + GAP) / (CARD_W + GAP)).floor() as usize).max(1)
    }

    fn render_row(
        &mut self,
        row: usize,
        cols: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let start = row * cols;
        let end = (start + cols).min(self.cards.len());
        let now = now_unix();
        let cards = (start..end)
            .map(|ix| self.render_card(ix, now, window, cx))
            .collect::<Vec<_>>();
        h_flex()
            .id(("row", row))
            .w_full()
            .px(px(SIDE_PAD))
            .pt(px(GAP))
            .gap(px(GAP))
            .items_start()
            .children(cards)
            .into_any_element()
    }

    fn render_card(
        &mut self,
        ix: usize,
        now: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cards = self.cards.clone();
        let card = &cards[ix];
        let image = self.thumbs.get(&card.id);
        if image.is_none() {
            self.request_thumb(card.id.clone(), window, cx);
        }
        let theme = cx.theme();
        let watched = card.watched;

        let badge = card
            .duration
            .clone()
            .or_else(|| card.live.then(|| SharedString::from("LIVE")));
        let thumb = div()
            .id("thumb")
            .relative()
            .w(px(CARD_W))
            .h(px(THUMB_H))
            .rounded(theme.radius_lg)
            .overflow_hidden()
            .bg(theme.muted)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| this.play(ix, window, cx)))
            .when_some(image, |d, image| d.child(img(image).size_full()))
            .when_some(badge, |d, badge| {
                d.child(
                    div()
                        .absolute()
                        .bottom_1()
                        .right_1()
                        .px_1()
                        .rounded_sm()
                        .bg(black().opacity(0.8))
                        .text_color(white())
                        .text_xs()
                        .child(badge),
                )
            })
            .when(watched, |d| {
                d.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(px(4.))
                        .bg(theme.danger),
                )
            });

        v_flex()
            .id(card.id.clone())
            .w(px(CARD_W))
            .gap_1()
            .when(watched, |d| d.opacity(0.6))
            .child(thumb)
            .child(
                h_flex()
                    .items_start()
                    .gap_1()
                    .child(
                        div()
                            .id("title")
                            .flex_1()
                            .h(px(40.))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_clamp(2)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| this.play(ix, window, cx)))
                            .child(card.title.clone()),
                    )
                    .child(
                        Button::new("watched")
                            .ghost()
                            .xsmall()
                            .icon(if watched { IconName::EyeOff } else { IconName::Eye })
                            .tooltip(if watched { "Mark as unwatched" } else { "Mark as watched" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_watched(ix, !watched, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(format!("{} · {}", card.channel, format_age(card.published, now))),
            )
            .into_any_element()
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let border = cx.theme().border;
        let muted_fg = cx.theme().muted_foreground;
        let auth_button = if self.signed_in {
            Button::new("sign-out")
                .ghost()
                .small()
                .label("Sign out")
                .on_click(cx.listener(|this, _, window, cx| this.sign_out(window, cx)))
        } else {
            Button::new("sign-in")
                .primary()
                .small()
                .icon(IconName::CircleUser)
                .label("Sign in with Google")
                .loading(self.signing_in)
                .on_click(cx.listener(|this, _, window, cx| this.sign_in(window, cx)))
        };
        h_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_3()
            .border_b_1()
            .border_color(border)
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.source.display_name()),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(muted_fg)
                    .truncate()
                    .child(self.status.clone()),
            )
            .child(
                Button::new("hide-watched")
                    .ghost()
                    .small()
                    .icon(if self.hide_watched { IconName::EyeOff } else { IconName::Eye })
                    .label(if self.hide_watched { "Show watched" } else { "Hide watched" })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_hide_watched(cx))),
            )
            .child(
                Button::new("refresh")
                    .ghost()
                    .small()
                    .icon(IconName::RefreshCw)
                    .label("Refresh")
                    .loading(self.refreshing)
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
            .child(
                Button::new("settings")
                    .ghost()
                    .small()
                    .icon(IconName::Settings)
                    .tooltip("Open config folder")
                    .on_click(cx.listener(|this, _, window, cx| this.open_config_folder(window, cx))),
            )
            .child(auth_button)
            .into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let msg = if !self.loaded_once || self.refreshing {
            "Loading…"
        } else if !self.signed_in && self.services.config.feed.extra_channels.is_empty() {
            "Sign in with Google to see your subscriptions."
        } else {
            "No videos yet."
        };
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(msg)
            .into_any_element()
    }
}

impl Render for FeedView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cols = self.columns(window);
        let rows = self.cards.len().div_ceil(cols);
        let count_label = format!("{} videos", self.cards.len());

        let body = if self.cards.is_empty() {
            self.render_empty(cx).into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    uniform_list(
                        "feed",
                        rows,
                        cx.processor(move |this, range: Range<usize>, window, cx| {
                            range
                                .map(|row| this.render_row(row, cols, window, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full()
                    .pb(px(GAP)),
                )
                .vertical_scrollbar(&self.scroll)
                .into_any_element()
        };

        let toolbar = self.render_toolbar(cx);
        let theme = cx.theme();
        v_flex()
            .track_focus(&self.focus)
            .key_context("FeedView")
            .on_action(cx.listener(|this, _: &Refresh, window, cx| this.refresh(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleHideWatched, _, cx| this.toggle_hide_watched(cx)))
            .on_action(cx.listener(|this, _: &OpenConfigFolder, window, cx| {
                this.open_config_folder(window, cx)
            }))
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(toolbar)
            .child(body)
            .child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py_1()
                    .gap_4()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(count_label)
                    .child(format!(
                        "thumbs {} ({:.0} MB)",
                        self.thumbs.len(),
                        crate::mem::mb(self.thumbs.memory_bytes())
                    ))
                    .child(div().flex_1())
                    .children(self.memory.clone()),
            )
    }
}
