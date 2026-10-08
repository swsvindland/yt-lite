//! A video grid for one feed source (Subscriptions or For you).
//!
//! Virtualized with GPUI's `uniform_list`: each list item is one row of cards.
//! Card width adapts to the window (at least `MIN_CARD_W`, filling the row),
//! only visible rows are built, and only visible cards request thumbnails.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use futures::future::{Either, select};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::thumb_store::ThumbStore;
use super::{format_age, now_unix};
use crate::app_state::{Account, AppConfig, AppServices};
use crate::auth::NotSignedIn;
use crate::db::{FeedQuery, VideoRow};
use crate::feed::pipeline;
use crate::feed::query::{EXPLORE_TOPICS, QuerySource};
use crate::feed::{FeedKind, FeedSource};
use crate::player::{Method, Player, Prepared};
use crate::system_player::SystemPlayer;
use crate::youtube::duration::format_clock;

pub const SIDEBAR_W: f32 = 232.;
const MIN_CARD_W: f32 = 280.;
const GAP: f32 = 20.;
const PAD_X: f32 = 28.;

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
            duration: v
                .duration_secs
                .filter(|s| *s > 0)
                .map(|s| format_clock(s).into()),
            id: v.id.into(),
            title: v.title.into(),
            channel: v.channel_title.into(),
            published: v.published,
            live: v.live,
            watched: v.watched,
        }
    }
}

pub enum FeedEvent {
    OpenSettings,
    /// A video started playing (For you uses this to know it's stale).
    Watched,
}

impl EventEmitter<FeedEvent> for FeedView {}

/// Shared between the feed views.
#[derive(Clone)]
pub struct Shared {
    pub player: Arc<Player>,
    pub system_player: Rc<RefCell<SystemPlayer>>,
    pub thumbs: Entity<ThumbStore>,
}

pub struct FeedView {
    source: Arc<dyn FeedSource>,
    shared: Shared,
    cards: Rc<Vec<Card>>,
    status: SharedString,
    refreshing: bool,
    loaded_once: bool,
    /// For you: something was watched since the last refresh.
    pub stale: bool,
    last_refresh: Option<i64>,
    scroll_test_started: bool,
    scroll: UniformListScrollHandle,
    /// Search and Explore: the source whose query we set.
    query: Option<Arc<QuerySource>>,
    search_input: Option<Entity<InputState>>,
    topic: usize,
    _subscriptions: Vec<Subscription>,
    _timer: Option<Task<()>>,
}

impl FeedView {
    /// Search or Explore, backed by a [`QuerySource`].
    pub fn new_query(
        source: Arc<QuerySource>,
        shared: Shared,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new(source.clone(), shared, window, cx);
        if source.kind() == FeedKind::Search {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search YouTube"));
            this._subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        let query = input.read(cx).value().to_string();
                        this.run_query(&query, window, cx);
                    }
                },
            ));
            this.search_input = Some(input);
        }
        this.query = Some(source);
        this
    }

    pub fn new(
        source: Arc<dyn FeedSource>,
        shared: Shared,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let interval = AppServices::get(cx).config.feed.refresh_interval();
        // Search results are only fetched on demand.
        let timer = (source.kind() != FeedKind::Search).then(|| {
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(interval).await;
                    if this
                        .update_in(cx, |v, window, cx| v.refresh(window, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });
        let mut was_signed_in = Account::get(cx).signed_in;
        let subscriptions = vec![
            cx.observe(&shared.thumbs, |_, _, cx| cx.notify()),
            // Settings like "hide watched" or max age change the query.
            cx.observe_global::<AppConfig>(|v, cx| v.reload(cx)),
            cx.observe_global_in::<Account>(window, move |v, window, cx| {
                let signed_in = Account::get(cx).signed_in;
                if signed_in && !was_signed_in && v.source.kind() == FeedKind::Subscriptions {
                    v.refresh(window, cx);
                }
                was_signed_in = signed_in;
                cx.notify();
            }),
        ];
        let mut this = Self {
            source,
            shared,
            cards: Rc::new(Vec::new()),
            status: "".into(),
            refreshing: false,
            loaded_once: false,
            stale: false,
            last_refresh: None,
            scroll_test_started: false,
            scroll: UniformListScrollHandle::new(),
            query: None,
            search_input: None,
            topic: 0,
            _subscriptions: subscriptions,
            _timer: timer,
        };
        this.startup(window, cx);
        this
    }

    /// Shows the cached feed immediately, then refreshes if it's stale.
    fn startup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source.kind() == FeedKind::Search {
            self.loaded_once = true;
            return;
        }
        let services = AppServices::get(cx);
        let kind = self.source.kind();
        cx.spawn_in(window, async move |this, cx| {
            let last = smol::unblock(move || {
                services
                    .db
                    .meta_get(&format!("last_refresh.{}", kind.as_str()))
                    .ok()
                    .flatten()
                    .and_then(|s| s.parse::<i64>().ok())
            })
            .await;
            this.update_in(cx, |v, window, cx| {
                v.last_refresh = last;
                v.reload(cx);
                if v.is_stale(cx) {
                    v.refresh(window, cx);
                } else {
                    v.maybe_scroll_test(window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Runs a search (or switches the Explore topic).
    fn run_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = &self.query else { return };
        if query.trim().is_empty() {
            return;
        }
        source.set_query(query);
        self.cards = Rc::new(Vec::new());
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.refresh(window, cx);
    }

    /// Puts `query` in the search box and runs it.
    pub fn search_for(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = &self.search_input {
            let q = query.to_string();
            input.update(cx, |s, cx| s.set_value(q, window, cx));
        }
        self.run_query(query, window, cx);
    }

    /// Focuses the search box (Search page).
    pub fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = &self.search_input {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    fn is_stale(&self, cx: &App) -> bool {
        if self.source.kind() == FeedKind::Search {
            return false;
        }
        let interval = AppConfig::get(cx).feed.refresh_interval().as_secs() as i64;
        self.stale || self.last_refresh.is_none_or(|t| now_unix() - t >= interval)
    }

    /// Called when the tab is shown.
    pub fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_stale(cx) {
            self.refresh(window, cx);
        }
    }

    fn query(&self, cx: &App) -> FeedQuery {
        let f = &AppConfig::get(cx).feed;
        FeedQuery {
            kind: self.source.kind(),
            max_age_days: f.max_age_days,
            limit: f.max_items,
            hide_watched: f.hide_watched,
        }
    }

    /// Re-reads the feed from SQLite (off the UI thread).
    fn reload(&mut self, cx: &mut Context<Self>) {
        let db = AppServices::get(cx).db.clone();
        let q = self.query(cx);
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

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        self.status = "Refreshing…".into();
        self.shared.thumbs.update(cx, |t, _| t.clear_failures());
        cx.notify();

        let services = AppServices::get(cx);
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
                        v.stale = false;
                        v.last_refresh = Some(now_unix());
                        let signed_in = AppServices::get(cx).auth.is_signed_in();
                        cx.update_global::<Account, _>(|a, _| a.signed_in = signed_in);
                        v.status =
                            format!("Updated {} · {stats}", chrono::Local::now().format("%H:%M"))
                                .into();
                    }
                    Err(e) if e.downcast_ref::<NotSignedIn>().is_some() => {
                        cx.update_global::<Account, _>(|a, _| a.signed_in = false);
                        v.status = "".into();
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

    /// `YT_LITE_SCROLL_TEST=1`: scroll through the whole grid twice and log
    /// memory, to verify the memory budget with a full thumbnail cache.
    fn maybe_scroll_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scroll_test_started
            || self.source.kind() != FeedKind::Subscriptions
            || std::env::var_os("YT_LITE_SCROLL_TEST").is_none()
        {
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
                let Ok(rows) = this.update_in(cx, |v, window, _| {
                    v.cards.len().div_ceil(layout(window).cols)
                }) else {
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
                    .read_with(cx, |v, cx| {
                        let t = v.shared.thumbs.read(cx);
                        (v.cards.len(), t.len(), t.memory_bytes())
                    })
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

    fn play(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(card) = self.cards.get(ix) else {
            return;
        };
        let (id, title) = (card.id.clone(), card.title.clone());
        self.play_video(id, title, window, cx);
    }

    pub fn play_video(
        &mut self,
        id: SharedString,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let player = self.shared.player.clone();
        let max_tier = AppConfig::get(cx).player.max_height;
        self.status = format!("Opening “{title}”…").into();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let video_id = id.to_string();
            let result = smol::unblock(move || player.prepare(&video_id, &title, max_tier)).await;
            this.update_in(cx, |v, window, cx| {
                // The system player must be created on the UI thread.
                let result = result.and_then(|prepared| match prepared {
                    Prepared::Started(method) => Ok(method),
                    Prepared::System(req, method) => v
                        .shared
                        .system_player
                        .borrow_mut()
                        .open(&req)
                        .map(|()| method),
                });
                match result {
                    Ok(method) => {
                        v.status = match method {
                            Method::Native => "Playing".into(),
                            Method::YtDlp => "Playing (via yt-dlp)".into(),
                        };
                        if let Some(ix) = v.cards.iter().position(|c| c.id == id) {
                            v.set_watched(ix, true, cx);
                        }
                        cx.emit(FeedEvent::Watched);
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
        let Some(card) = cards.get_mut(ix) else {
            return;
        };
        card.watched = watched;
        let id = card.id.to_string();
        let db = AppServices::get(cx).db.clone();
        cx.background_spawn(async move {
            if let Err(e) = db.set_watched(&id, watched) {
                log::error!("set_watched {id}: {e:#}");
            }
        })
        .detach();
        cx.notify();
    }

    fn render_row(
        &mut self,
        row: usize,
        l: Layout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let start = row * l.cols;
        let end = (start + l.cols).min(self.cards.len());
        let now = now_unix();
        let cards = (start..end)
            .map(|ix| self.render_card(ix, l, now, window, cx))
            .collect::<Vec<_>>();
        h_flex()
            .id(("row", row))
            .w_full()
            .px(px(PAD_X))
            .pb(px(GAP))
            .gap(px(GAP))
            .items_start()
            .children(cards)
            .into_any_element()
    }

    fn render_card(
        &mut self,
        ix: usize,
        l: Layout,
        now: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cards = self.cards.clone();
        let card = &cards[ix];
        let scale = window.scale_factor();
        let image = self.shared.thumbs.update(cx, |t, cx| {
            t.get(&card.id, (l.card_w, l.thumb_h), scale, cx)
        });
        let theme = cx.theme();
        let watched = card.watched;

        let badge = match (&card.duration, card.live) {
            (_, true) => Some(
                div()
                    .px_1p5()
                    .rounded_md()
                    .bg(rgb(0xcc0000))
                    .text_color(white())
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("LIVE"),
            ),
            (Some(d), false) => Some(
                div()
                    .px_1p5()
                    .rounded_md()
                    .bg(black().opacity(0.78))
                    .text_color(white())
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .child(d.clone()),
            ),
            _ => None,
        };

        let thumb = div()
            .relative()
            .w(px(l.card_w))
            .h(px(l.thumb_h))
            .rounded(px(12.))
            .overflow_hidden()
            .bg(theme.muted)
            .when_some(image, |d, image| {
                d.child(img(image).size_full().object_fit(ObjectFit::Cover))
            })
            .when_some(badge, |d, badge| {
                d.child(div().absolute().bottom_2().right_2().child(badge))
            })
            .when(watched, |d| {
                d.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(px(4.))
                        .bg(rgb(0xff0033)),
                )
            });

        let eye = Button::new("watched")
            .ghost()
            .xsmall()
            .icon(if watched {
                IconName::EyeOff
            } else {
                IconName::Eye
            })
            .tooltip(if watched {
                "Mark as unwatched"
            } else {
                "Mark as watched"
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.set_watched(ix, !watched, cx)
            }));

        v_flex()
            .id(card.id.clone())
            .w(px(l.card_w))
            .gap_2()
            .cursor_pointer()
            .group("card")
            .when(watched, |d| d.opacity(0.55))
            .on_click(cx.listener(move |this, _, window, cx| this.play(ix, window, cx)))
            .child(thumb)
            .child(
                h_flex()
                    .items_start()
                    .gap_1()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .h(px(40.))
                                    .text_sm()
                                    .line_height(px(20.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .line_clamp(2)
                                    .group_hover("card", |s| s.text_color(theme.primary))
                                    .child(card.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(format!(
                                        "{} · {}",
                                        card.channel,
                                        format_age(card.published, now)
                                    )),
                            ),
                    )
                    .child(eye),
            )
            .into_any_element()
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(input) = &self.search_input {
            return h_flex()
                .gap_2()
                .items_center()
                .child(
                    div().w(px(420.)).child(
                        Input::new(input)
                            .prefix(Icon::new(IconName::Search).small())
                            .cleanable(true),
                    ),
                )
                .child(
                    Button::new("search")
                        .primary()
                        .small()
                        .label("Search")
                        .loading(self.refreshing)
                        .on_click(cx.listener(|this, _, window, cx| {
                            let query = this
                                .search_input
                                .as_ref()
                                .map(|i| i.read(cx).value().to_string())
                                .unwrap_or_default();
                            this.run_query(&query, window, cx);
                        })),
                )
                .into_any_element();
        }
        let hide_watched = AppConfig::get(cx).feed.hide_watched;
        h_flex()
            .gap_4()
            .items_center()
            .child(
                Switch::new("hide-watched")
                    .checked(hide_watched)
                    .label("Hide watched")
                    .small()
                    .on_click(|checked, _, cx| {
                        let checked = *checked;
                        AppConfig::update(cx, |c| c.feed.hide_watched = checked);
                    }),
            )
            .child(
                Button::new("refresh")
                    .outline()
                    .small()
                    .icon(IconName::RefreshCw)
                    .label("Refresh")
                    .loading(self.refreshing)
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
            .into_any_element()
    }

    /// Explore: one chip per topic.
    fn render_topics(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.source.kind() != FeedKind::Explore {
            return None;
        }
        let chips = EXPLORE_TOPICS
            .iter()
            .enumerate()
            .map(|(ix, (label, query))| {
                let selected = ix == self.topic;
                let button = Button::new(("topic", ix)).small().label(*label);
                let button = if selected {
                    button.primary()
                } else {
                    button.outline()
                };
                button.on_click(cx.listener(move |this, _, window, cx| {
                    this.topic = ix;
                    this.run_query(query, window, cx);
                }))
            });
        Some(
            h_flex()
                .px(px(PAD_X))
                .pb_4()
                .gap_2()
                .flex_wrap()
                .children(chips)
                .into_any_element(),
        )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let controls = self.render_controls(cx);
        let topics = self.render_topics(cx);
        let theme = cx.theme();
        let subtitle: SharedString = if !self.status.is_empty() {
            self.status.clone()
        } else if self.source.kind() == FeedKind::Search && self.cards.is_empty() {
            "Results never include Shorts.".into()
        } else {
            format!("{} videos", self.cards.len()).into()
        };
        v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .px(px(PAD_X))
                    .pt_6()
                    .pb_4()
                    .gap_4()
                    .items_end()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child(self.source.display_name()),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .when(self.refreshing, |d| d.child(Spinner::new().xsmall()))
                                    .child(div().truncate().child(subtitle)),
                            ),
                    )
                    .child(controls),
            )
            .children(topics)
            .into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let account = Account::get(cx);
        let (icon, title, body, action): (IconName, &str, &str, Option<AnyElement>) = if !self
            .loaded_once
            || (self.refreshing && self.cards.is_empty())
        {
            (
                IconName::LoaderCircle,
                "Loading…",
                "Fetching the latest videos.",
                None,
            )
        } else if self.source.kind() == FeedKind::Subscriptions
            && !account.signed_in
            && AppConfig::get(cx).feed.extra_channels.is_empty()
        {
            let has_client = AppConfig::get(cx).has_google_client();
            let button = if has_client {
                Button::new("sign-in")
                    .primary()
                    .icon(IconName::CircleUser)
                    .label("Sign in with Google")
                    .loading(account.signing_in)
                    .on_click(|_, window, cx| Account::sign_in(window, cx))
            } else {
                Button::new("open-settings")
                    .primary()
                    .icon(IconName::Settings)
                    .label("Open Settings")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FeedEvent::OpenSettings)))
            };
            (
                IconName::CircleUser,
                "Connect your YouTube account",
                if has_client {
                    "Sign in to load the channels you subscribe to. Only read access is requested."
                } else {
                    "Add your Google OAuth client in Settings to load your subscriptions."
                },
                Some(button.into_any_element()),
            )
        } else if self.source.kind() == FeedKind::Search {
            (
                IconName::Search,
                "Search YouTube",
                "Type a query and press Enter. Works without signing in.",
                None,
            )
        } else if self.source.kind() == FeedKind::Home {
            (
                IconName::Star,
                "Nothing here yet",
                "Watch a few videos and yt-lite will recommend more like them.",
                None,
            )
        } else {
            (
                IconName::Inbox,
                "No videos yet",
                "New uploads from your subscriptions show up here.",
                None,
            )
        };
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .p_8()
            .child(
                div()
                    .size(px(56.))
                    .rounded_full()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(icon).size_6().text_color(theme.muted_foreground)),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(380.))
                    .text_sm()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(body),
            )
            .children(action.map(|a| div().pt_2().child(a)))
            .into_any_element()
    }
}

#[derive(Clone, Copy)]
struct Layout {
    cols: usize,
    card_w: f32,
    thumb_h: f32,
}

fn layout(window: &Window) -> Layout {
    let avail = (f32::from(window.viewport_size().width) - SIDEBAR_W - PAD_X * 2.).max(MIN_CARD_W);
    let cols = (((avail + GAP) / (MIN_CARD_W + GAP)).floor() as usize).max(1);
    let card_w = ((avail - GAP * (cols as f32 - 1.)) / cols as f32).floor();
    Layout {
        cols,
        card_w,
        thumb_h: (card_w * 9. / 16.).round(),
    }
}

impl Render for FeedView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let l = layout(window);
        let rows = self.cards.len().div_ceil(l.cols);
        let header = self.render_header(cx);
        let body = if self.cards.is_empty() {
            self.render_empty(cx)
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
                                .map(|row| this.render_row(row, l, window, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .size_full(),
                )
                .vertical_scrollbar(&self.scroll)
                .into_any_element()
        };
        v_flex().size_full().child(header).child(body)
    }
}
