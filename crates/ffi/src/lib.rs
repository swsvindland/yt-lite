//! Swift/Kotlin-facing API for the iOS and Android apps. Every method is
//! blocking (network or SQLite); the apps call them from background threads.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use yt_lite_core::auth::{Auth, DEFAULT_KEYRING_SERVICE, LoopbackSignIn, NotSignedIn};
use yt_lite_core::config::{Config, Paths};
use yt_lite_core::db::{self, Db, DownloadRow, FeedQuery, VideoRow};
use yt_lite_core::feed::foryou::ForYouSource;
use yt_lite_core::feed::pipeline::{self, Services};
use yt_lite_core::feed::query::{EXPLORE_TOPICS, QuerySource};
use yt_lite_core::feed::subscriptions::SubscriptionsSource;
use yt_lite_core::feed::{FeedKind, FeedSource};
use yt_lite_core::net::Http;
use yt_lite_core::offline::{self, LocalFile, SmartPolicy};
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

const HISTORY_LIMIT: u32 = 500;

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
    /// When it was last played or marked watched (unix seconds).
    pub last_played: Option<i64>,
    pub offline: Offline,
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
            last_played: v.last_played,
            offline: match v.offline {
                db::Offline::No => Offline::No,
                db::Offline::Queued => Offline::Queued,
                db::Offline::Failed => Offline::Failed,
                db::Offline::Saved => Offline::Saved,
                db::Offline::Smart => Offline::Smart,
            },
        }
    }
}

/// Whether a video is (being) downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Offline {
    No,
    /// The user asked for it; waiting or downloading.
    Queued,
    /// The user asked for it and it failed (Downloads says why).
    Failed,
    /// Downloaded because the user asked: kept until they remove it.
    Saved,
    /// A smart download: kept while it's likely to be watched.
    Smart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DownloadState {
    Queued,
    Downloading,
    Done,
    Failed,
}

/// A download, for the Downloads list.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Download {
    pub video: Video,
    /// Picked by smart downloads rather than asked for.
    pub smart: bool,
    pub state: DownloadState,
    pub audio_only: bool,
    /// Size on disk once done; while downloading, the total (0 until known).
    pub bytes: u64,
    /// While downloading.
    pub done_bytes: u64,
    pub error: Option<String>,
}

/// The download in progress.
#[derive(Debug, Clone, uniffi::Record)]
pub struct DownloadProgress {
    pub video_id: String,
    pub title: String,
    pub smart: bool,
    pub done_bytes: u64,
    /// 0 until known.
    pub total_bytes: u64,
    /// Downloads the user asked for that are still to do (this one included).
    pub waiting: u32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct OfflineStorage {
    pub saved_bytes: u64,
    pub smart_bytes: u64,
    pub free_bytes: Option<u64>,
}

/// What smart downloads may use.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SmartDownloads {
    pub budget_bytes: u64,
    pub max_height: u32,
    /// Download just the sound (people who listen rather than watch).
    pub audio_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RunOutcome {
    /// Nothing left to do.
    Done,
    /// The `CancelToken` was cancelled.
    Stopped,
    /// A smart run made way for downloads the user asked for.
    Yielded,
    /// Another run is going; it picks up anything new.
    Busy,
    /// The connection failed; try again later.
    Offline,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct RunResult {
    pub outcome: RunOutcome,
    pub downloaded: u32,
    pub failed: u32,
    pub error: Option<String>,
}

/// Told when downloads change, on a background thread: on every request,
/// start, finish, failure and removal (lists may need reloading), and with
/// `progress_only` while one downloads, at most twice a second.
#[uniffi::export(with_foreign)]
pub trait DownloadListener: Send + Sync {
    fn downloads_changed(&self, progress_only: bool);
}

/// Stops a `run_downloads` from another thread (the app's background time
/// is up, Wi-Fi or the charger went away).
#[derive(Debug, Default, uniffi::Object)]
pub struct CancelToken(AtomicBool);

#[uniffi::export]
impl CancelToken {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct Playable {
    /// HLS: the master playlist (video), or its audio-only rendition (audio
    /// only; the master itself if it has none). Downloaded: a `file://` URL
    /// of an MP4 (an M4A if only the sound was downloaded).
    pub url: String,
    pub title: String,
    pub is_live: bool,
    /// Play only the sound. A downloaded MP4 still has its picture: turn the
    /// video track off.
    pub audio_only: bool,
    /// `url` is a downloaded file.
    pub offline: bool,
    /// Where to start (seconds): the resume point, or 0.
    pub start_secs: f64,
}

#[derive(uniffi::Object)]
pub struct YtLite {
    services: Arc<Services>,
    resolver: InnertubeResolver,
    offline: offline::Offline,
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
        let offline = offline::Offline::new(
            db.clone(),
            http.clone(),
            paths.downloads_dir(),
            config.feed.max_age_days,
        );
        let services = Arc::new(Services {
            auth: Auth::new(config.google.clone(), http.clone(), DEFAULT_KEYRING_SERVICE),
            http: http.clone(),
            db: db.clone(),
            config,
        });
        Ok(Arc::new(Self {
            resolver: InnertubeResolver::new(http, Some(db)),
            offline,
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

    /// Videos played or marked watched, most recent first.
    pub fn history(&self) -> Result<Vec<Video>> {
        let rows = self.services.db.history(HISTORY_LIMIT)?;
        Ok(rows.into_iter().map(Video::from).collect())
    }

    /// Also unmarks it watched and forgets its resume point.
    pub fn remove_from_history(&self, id: String) -> Result<()> {
        Ok(self.services.db.remove_from_history(&id)?)
    }

    pub fn clear_history(&self) -> Result<()> {
        Ok(self.services.db.clear_history()?)
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

    /// A downloaded video plays from its file. Otherwise streams are
    /// resolved natively (no yt-dlp on iOS). Video: the HLS master playlist.
    /// `audio_only`: the master's audio-only rendition, which AVPlayer plays
    /// with no video decoding (podcasts), or the master itself if that can't
    /// be found, so audio-only never falls back to video. If only the sound
    /// was downloaded and streaming fails (offline), that plays instead.
    /// Starts at the resume point unless `from_start`.
    pub fn play(
        &self,
        id: String,
        max_height: u32,
        audio_only: bool,
        from_start: bool,
    ) -> Result<Playable> {
        let local = self.offline.local_file(&id).unwrap_or_else(|e| {
            log::warn!("looking for a download of {id}: {e:#}");
            None
        });
        let mut playable = match local {
            Some(file) if audio_only || !file.audio_only => {
                self.local_playable(&id, &file, audio_only)?
            }
            _ => match self.stream(&id, max_height, audio_only) {
                Ok(p) => p,
                Err(e) => match local {
                    Some(file) => {
                        log::info!("playing the downloaded sound of {id}: {e}");
                        self.local_playable(&id, &file, true)?
                    }
                    None => return Err(e),
                },
            },
        };
        let db = &self.services.db;
        if let Err(e) = db.mark_played(&id) {
            log::warn!("mark_played {id}: {e:#}");
        }
        if !from_start && !playable.is_live {
            playable.start_secs = db
                .resume_position(&id)
                .unwrap_or_else(|e| {
                    log::warn!("resume_position {id}: {e:#}");
                    None
                })
                .unwrap_or(0.0);
        }
        Ok(playable)
    }

    // Downloads

    /// Called when downloads change (see `DownloadListener`).
    pub fn set_download_listener(&self, listener: Arc<dyn DownloadListener>) {
        self.offline
            .set_listener(move |progress_only| listener.downloads_changed(progress_only));
    }

    /// Queues a download (or keeps a smart download of it). Then run
    /// downloads (`run_downloads`), in the background.
    pub fn download(&self, id: String, max_height: u32, audio_only: bool) -> Result<()> {
        Ok(self.offline.request(&id, audio_only, max_height)?)
    }

    /// Deletes a download or cancels it. Smart downloads leave the video
    /// alone from then on.
    pub fn remove_download(&self, id: String) -> Result<()> {
        Ok(self.offline.remove(&id)?)
    }

    /// Deletes every smart download (they were turned off).
    pub fn clear_smart_downloads(&self) -> Result<()> {
        Ok(self.offline.clear_smart()?)
    }

    /// Downloads the user asked for (waiting, failed, done), and finished
    /// smart downloads. Waiting ones first, then failed, then done, newest
    /// first.
    pub fn downloads(&self) -> Result<Vec<Download>> {
        let progress = self.offline.progress();
        let rows = self.offline.list()?;
        Ok(rows
            .into_iter()
            .filter(|r| !r.smart || r.state == db::DownloadState::Done)
            .map(|r| download(r, progress.as_ref()))
            .collect())
    }

    pub fn download_progress(&self) -> Option<DownloadProgress> {
        let p = self.offline.progress()?;
        let row = self.services.db.download(&p.video_id).ok().flatten();
        Some(DownloadProgress {
            title: row
                .as_ref()
                .map(|r| r.video.title.clone())
                .unwrap_or_default(),
            smart: row.is_some_and(|r| r.smart),
            video_id: p.video_id,
            done_bytes: p.done,
            total_bytes: p.total,
            waiting: self.offline.waiting().unwrap_or(0),
        })
    }

    /// Downloads the user asked for that are still to do.
    pub fn downloads_waiting(&self) -> Result<u32> {
        Ok(self.offline.waiting()?)
    }

    pub fn offline_storage(&self) -> Result<OfflineStorage> {
        let s = self.offline.storage()?;
        Ok(OfflineStorage {
            saved_bytes: s.saved_bytes,
            smart_bytes: s.smart_bytes,
            free_bytes: s.free_bytes,
        })
    }

    /// Where downloads are kept (iOS excludes it from iCloud backups).
    pub fn downloads_dir(&self) -> String {
        self.offline.dir().to_string_lossy().into_owned()
    }

    /// Blocking: downloads what the user asked for, then, with `smart`,
    /// refreshes Subscriptions and For you and fills smart downloads up to
    /// the budget. Returns when done, when `cancel` is cancelled, or when the
    /// connection fails. Only one run at a time (see `RunOutcome::Busy`).
    pub fn run_downloads(
        &self,
        smart: Option<SmartDownloads>,
        cancel: Arc<CancelToken>,
    ) -> Result<RunResult> {
        let stop = || cancel.is_cancelled();
        let policy = smart.map(|s| SmartPolicy {
            budget_bytes: s.budget_bytes,
            max_height: s.max_height,
            audio_only: s.audio_only,
        });
        if policy.is_some() {
            // Fresh lists first, so tonight's uploads are what gets saved.
            for feed in [Feed::Subscriptions, Feed::ForYou] {
                if stop() {
                    break;
                }
                match self.refresh(feed) {
                    Ok(_) | Err(FfiError::NotSignedIn) => {}
                    Err(e) => log::warn!("refreshing {feed:?} before smart downloads: {e}"),
                }
            }
        }
        let report = self.offline.run(&self.resolver, policy.as_ref(), &stop)?;
        Ok(RunResult {
            outcome: match report.outcome {
                offline::Outcome::Done => RunOutcome::Done,
                offline::Outcome::Stopped => RunOutcome::Stopped,
                offline::Outcome::Yielded => RunOutcome::Yielded,
                offline::Outcome::Busy => RunOutcome::Busy,
                offline::Outcome::Offline => RunOutcome::Offline,
            },
            downloaded: report.downloaded,
            failed: report.failed,
            error: report.error,
        })
    }
}

impl YtLite {
    fn stream(&self, id: &str, max_height: u32, audio_only: bool) -> Result<Playable> {
        let prefs = Prefs {
            max_tier: max_height,
            ..Prefs::default()
        };
        let r = self.resolver.resolve(id, &prefs)?;
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
        Ok(Playable {
            url,
            title: r.title,
            is_live: r.is_live,
            audio_only,
            offline: false,
            start_secs: 0.0,
        })
    }

    fn local_playable(&self, id: &str, file: &LocalFile, audio_only: bool) -> Result<Playable> {
        let url = url::Url::from_file_path(&file.path).map_err(|()| FfiError::Failed {
            reason: format!("bad download path {}", file.path.display()),
        })?;
        let title = self
            .services
            .db
            .download(id)?
            .map(|r| r.video.title)
            .unwrap_or_default();
        Ok(Playable {
            url: url.into(),
            title,
            is_live: false,
            audio_only: audio_only || file.audio_only,
            offline: true,
            start_secs: 0.0,
        })
    }
}

fn download(r: DownloadRow, progress: Option<&offline::Progress>) -> Download {
    let active = progress.filter(|p| p.video_id == r.video.id);
    let state = match r.state {
        db::DownloadState::Done => DownloadState::Done,
        db::DownloadState::Failed => DownloadState::Failed,
        _ if active.is_some() => DownloadState::Downloading,
        _ => DownloadState::Queued,
    };
    Download {
        smart: r.smart,
        state,
        audio_only: r.audio_only,
        bytes: active.map_or(r.bytes, |p| p.total),
        done_bytes: active.map_or(0, |p| p.done),
        error: r.error.filter(|_| r.state != db::DownloadState::Done),
        video: r.video.into(),
    }
}
