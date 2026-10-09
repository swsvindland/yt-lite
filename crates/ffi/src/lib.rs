//! Swift/Kotlin-facing API for the iOS and Android apps. Every method is
//! blocking (network or SQLite); the apps call them from background threads.

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
use yt_lite_core::resolve::{Prefs, StreamResolver, hls};

mod secret_store;

uniffi::setup_scaffolding!();

#[derive(Debug, uniffi::Error)]
pub enum FfiError {
    NotSignedIn,
    // Not `message`: it would clash with Kotlin's Throwable.message.
    Failed { reason: String },
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiError::NotSignedIn => f.write_str("Not signed in"),
            FfiError::Failed { reason } => f.write_str(reason),
        }
    }
}

impl std::error::Error for FfiError {}

/// An exception thrown by app code called from Rust (a [`SecretStore`]).
impl From<uniffi::UnexpectedUniFFICallbackError> for FfiError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        FfiError::Failed { reason: e.reason }
    }
}

impl From<anyhow::Error> for FfiError {
    fn from(e: anyhow::Error) -> Self {
        if e.downcast_ref::<NotSignedIn>().is_some() {
            FfiError::NotSignedIn
        } else {
            FfiError::Failed {
                reason: format!("{e:#}"),
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
    /// Fraction played (0–1) if it was stopped partway, for a progress bar.
    pub progress: Option<f64>,
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
            progress: v.progress,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct Playable {
    /// HLS: the master playlist (video), or its audio-only rendition (audio
    /// only; the master itself if it has none).
    pub url: String,
    pub title: String,
    pub is_live: bool,
    pub audio_only: bool,
    /// Where to start (seconds): the resume point, or 0.
    pub start_secs: f64,
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

    /// `return_url`: where to send the browser after Google redirects back
    /// (Android: an app link that brings the app back over the browser tab).
    #[uniffi::method(default(return_url = None))]
    pub fn start_sign_in(&self, return_url: Option<String>) -> Result<Arc<SignIn>> {
        Ok(Arc::new(SignIn {
            inner: self
                .services
                .auth
                .start_loopback_sign_in()?
                .returning_to(return_url),
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

    /// Also forgets the resume point.
    pub fn set_watched(&self, id: String, watched: bool) -> Result<()> {
        Ok(self.services.db.set_watched(&id, watched)?)
    }

    /// Records where playback got to: a resume point, or watched once it's
    /// finished. `duration_secs`: the player's, if it knows it.
    pub fn save_progress(
        &self,
        id: String,
        position_secs: f64,
        duration_secs: Option<f64>,
    ) -> Result<()> {
        self.services
            .db
            .save_progress(&id, position_secs, duration_secs)?;
        Ok(())
    }

    /// Resolves streams natively (no yt-dlp on iOS). Video: the HLS master
    /// playlist. `audio_only`: the master's audio-only rendition, which
    /// AVPlayer plays with no video decoding (podcasts), or the master itself
    /// if that can't be found, so audio-only never falls back to video.
    /// Starts at the resume point unless `from_start`.
    pub fn play(
        &self,
        id: String,
        max_height: u32,
        audio_only: bool,
        from_start: bool,
    ) -> Result<Playable> {
        let prefs = Prefs {
            max_tier: max_height,
            ..Prefs::default()
        };
        let r = self.resolver.resolve(&id, &prefs)?;
        let Some(master) = r.hls else {
            return Err(FfiError::Failed {
                reason: "YouTube returned no playable stream for this video".into(),
            });
        };
        let url = if audio_only {
            match hls::fetch_audio_rendition(&self.services.http, &master) {
                Ok(Some(audio)) => audio,
                Ok(None) => master,
                Err(e) => {
                    log::warn!("audio rendition for {id}: {e:#}");
                    master
                }
            }
        } else {
            master
        };
        let db = &self.services.db;
        if let Err(e) = db.mark_played(&id) {
            log::warn!("mark_played {id}: {e:#}");
        }
        let start_secs = if from_start || r.is_live {
            None
        } else {
            db.resume_position(&id).unwrap_or_else(|e| {
                log::warn!("resume_position {id}: {e:#}");
                None
            })
        };
        Ok(Playable {
            url,
            title: r.title,
            is_live: r.is_live,
            audio_only,
            start_secs: start_secs.unwrap_or(0.0),
        })
    }
}
