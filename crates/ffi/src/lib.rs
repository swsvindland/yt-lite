//! Swift-facing API for the iOS app. Every method is blocking (network or
//! SQLite); Swift calls them from background tasks.

use std::path::PathBuf;
use std::sync::Arc;

use yt_lite_core::auth::{Auth, DEFAULT_KEYRING_SERVICE, LoopbackSignIn, NotSignedIn};
use yt_lite_core::config::{Config, Paths};
use yt_lite_core::db::{Db, FeedQuery, VideoRow};
use yt_lite_core::feed::foryou::ForYouSource;
use yt_lite_core::feed::pipeline::{self, Services};
use yt_lite_core::feed::query::{EXPLORE_TOPICS, QuerySource};
use yt_lite_core::feed::subscriptions::SubscriptionsSource;
use yt_lite_core::feed::{FeedKind, FeedSource};
use yt_lite_core::net::Http;
use yt_lite_core::resolve::innertube::InnertubeResolver;
use yt_lite_core::resolve::{Prefs, StreamResolver};

uniffi::setup_scaffolding!();

#[derive(Debug, uniffi::Error)]
pub enum FfiError {
    NotSignedIn,
    Failed { message: String },
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiError::NotSignedIn => f.write_str("Not signed in"),
            FfiError::Failed { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for FfiError {}

impl From<anyhow::Error> for FfiError {
    fn from(e: anyhow::Error) -> Self {
        if e.downcast_ref::<NotSignedIn>().is_some() {
            FfiError::NotSignedIn
        } else {
            FfiError::Failed {
                message: format!("{e:#}"),
            }
        }
    }
}

type Result<T> = std::result::Result<T, FfiError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Feed {
    Subscriptions,
    ForYou,
    Explore,
    Search,
}

impl Feed {
    fn kind(self) -> FeedKind {
        match self {
            Feed::Subscriptions => FeedKind::Subscriptions,
            Feed::ForYou => FeedKind::Home,
            Feed::Explore => FeedKind::Explore,
            Feed::Search => FeedKind::Search,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct Video {
    pub id: String,
    pub title: String,
    pub channel: String,
    /// Unix seconds (approximate for For you / Search / Explore).
    pub published: i64,
    pub duration_secs: Option<u32>,
    pub live: bool,
    pub watched: bool,
}

impl From<VideoRow> for Video {
    fn from(v: VideoRow) -> Self {
        Self {
            id: v.id,
            title: v.title,
            channel: v.channel_title,
            published: v.published,
            duration_secs: v.duration_secs,
            live: v.live,
            watched: v.watched,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct Playable {
    /// HLS master playlist (video), or an AAC audio stream (audio only).
    pub url: String,
    pub title: String,
    pub is_live: bool,
    pub audio_only: bool,
}

#[derive(uniffi::Object)]
pub struct YtLite {
    services: Arc<Services>,
    resolver: InnertubeResolver,
    subscriptions: SubscriptionsSource,
    for_you: ForYouSource,
    explore: QuerySource,
    search: QuerySource,
}

#[derive(uniffi::Object)]
pub struct SignIn {
    inner: LoopbackSignIn,
}

#[uniffi::export]
impl SignIn {
    /// The Google consent page to show (ASWebAuthenticationSession).
    pub fn url(&self) -> String {
        self.inner.url().to_string()
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

#[uniffi::export]
impl YtLite {
    /// `root_dir`: an app-container folder (Application Support). The Google
    /// OAuth client ("Desktop app" type) comes from the build's secrets.
    #[uniffi::constructor]
    pub fn new(root_dir: String, client_id: String, client_secret: String) -> Result<Arc<Self>> {
        let paths = Paths::in_dir(PathBuf::from(root_dir));
        let mut config = Config::load_or_init(&paths.config_file)?;
        if !client_id.is_empty() {
            config.google.client_id = client_id;
            config.google.client_secret = client_secret;
        }
        let http = Http::new();
        let db = Db::open(&paths.db_file())?;
        let services = Arc::new(Services {
            auth: Auth::new(config.google.clone(), http.clone(), DEFAULT_KEYRING_SERVICE),
            http: http.clone(),
            db: db.clone(),
            config,
        });
        Ok(Arc::new(Self {
            resolver: InnertubeResolver::new(http, Some(db)),
            services,
            subscriptions: SubscriptionsSource,
            for_you: ForYouSource,
            explore: QuerySource::explore(),
            search: QuerySource::search(),
        }))
    }

    pub fn is_signed_in(&self) -> bool {
        self.services.auth.is_signed_in()
    }

    /// `None` if tokens can be saved; otherwise the credential-store error.
    pub fn credential_store_problem(&self) -> Option<String> {
        self.services.auth.credential_store_problem()
    }

    pub fn has_google_client(&self) -> bool {
        self.services.config.has_google_client()
    }

    pub fn start_sign_in(&self) -> Result<Arc<SignIn>> {
        Ok(Arc::new(SignIn {
            inner: self.services.auth.start_loopback_sign_in()?,
        }))
    }

    /// Blocks until the browser redirects back (or `cancel`).
    pub fn finish_sign_in(&self, sign_in: Arc<SignIn>) -> Result<()> {
        Ok(self.services.auth.finish_loopback_sign_in(&sign_in.inner)?)
    }

    pub fn sign_out(&self) -> Result<()> {
        Ok(self.services.auth.sign_out()?)
    }

    /// Fetches from YouTube, caches, filters Shorts. Returns a status line.
    pub fn refresh(&self, feed: Feed) -> Result<String> {
        let source: &dyn FeedSource = match feed {
            Feed::Subscriptions => &self.subscriptions,
            Feed::ForYou => &self.for_you,
            Feed::Explore => &self.explore,
            Feed::Search => &self.search,
        };
        let stats = pipeline::refresh(source, &self.services, &|_| {})?;
        Ok(stats.to_string())
    }

    /// Cached, Shorts-free videos for a feed.
    pub fn videos(&self, feed: Feed, hide_watched: bool) -> Result<Vec<Video>> {
        let f = &self.services.config.feed;
        let rows = self.services.db.feed(FeedQuery {
            kind: feed.kind(),
            max_age_days: f.max_age_days,
            limit: f.max_items,
            hide_watched,
        })?;
        Ok(rows.into_iter().map(Video::from).collect())
    }

    pub fn search(&self, query: String) -> Result<Vec<Video>> {
        self.search.set_query(&query);
        self.refresh(Feed::Search)?;
        self.videos(Feed::Search, false)
    }

    pub fn explore_topics(&self) -> Vec<String> {
        EXPLORE_TOPICS.iter().map(|(label, _)| label.to_string()).collect()
    }

    pub fn explore(&self, topic: u32) -> Result<Vec<Video>> {
        let (_, query) = EXPLORE_TOPICS
            .get(topic as usize)
            .copied()
            .unwrap_or(EXPLORE_TOPICS[0]);
        self.explore.set_query(query);
        self.refresh(Feed::Explore)?;
        self.videos(Feed::Explore, false)
    }

    pub fn set_watched(&self, id: String, watched: bool) -> Result<()> {
        Ok(self.services.db.set_watched(&id, watched)?)
    }

    /// Resolves streams natively (no yt-dlp on iOS). Video: the HLS master
    /// playlist. `audio_only`: the AAC audio stream, which AVPlayer plays
    /// in the background with no video decoding (podcasts). Live streams are
    /// always HLS.
    pub fn play(&self, id: String, max_height: u32, audio_only: bool) -> Result<Playable> {
        let prefs = Prefs {
            max_tier: max_height,
            // AVPlayer can't decode Opus/WebM.
            audio_mime: Some("audio/mp4".into()),
            ..Prefs::default()
        };
        let r = self.resolver.resolve(&id, &prefs)?;
        let audio_url = r.audio.as_ref().map(|a| a.url.clone()).filter(|_| audio_only && !r.is_live);
        let (url, audio_only) = match (audio_url, r.hls.clone()) {
            (Some(audio), _) => (audio, true),
            (None, Some(hls)) => (hls, false),
            (None, None) => {
                return Err(FfiError::Failed {
                    message: "YouTube returned no playable stream for this video".into(),
                });
            }
        };
        Ok(Playable {
            url,
            title: r.title,
            is_live: r.is_live,
            audio_only,
        })
    }
}
