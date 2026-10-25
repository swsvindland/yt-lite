//! Downloads for offline playback.
//!
//! Two kinds: videos the user asks for ("saved"), kept until they remove
//! them, and smart downloads, which keep the videos you're most likely to
//! watch next (ones you stopped partway, then subscription uploads, then
//! For you) within a size budget, replacing ones you've watched. The apps
//! decide when downloads may run (smart downloads only on Wi-Fi while
//! charging) and call [`Offline::run`] then; everything here is blocking.
//!
//! A download is the H.264 video and AAC audio streams (or just the audio),
//! fetched in resumable range requests ([`fetch`]) and remuxed into one MP4
//! ([`mux`]) that every player opens offline.

pub mod fetch;
pub mod mux;
pub mod plan;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};

use crate::db::{CandidateLimits, Db, DownloadRow, DownloadState, Finish};
use crate::net::{ApiError, Http};
use crate::resolve::innertube::VISIONOS;
use crate::resolve::{Prefs, StreamResolver, Unplayable};
use fetch::FetchError;
use plan::Kept;

/// Failed tries (other than the network's) before a download the user
/// asked for is given up. Smart downloads give up after one.
const MAX_ATTEMPTS: u32 = 3;
/// Smart downloads leave at least this much space on the device.
const SMART_MIN_FREE: u64 = 2 << 30;
/// Downloads the user asks for leave at least this much.
const SAVED_MIN_FREE: u64 = 512 << 20;
/// A smart download bigger than this share of the budget is skipped, so
/// one ten-hour stream doesn't crowd out everything else.
const MAX_SHARE: u64 = 4;
/// Smart downloads stop looking after this many candidates in a row that
/// don't fit.
const MAX_MISSES: usize = 8;
const NOTIFY_EVERY: Duration = Duration::from_millis(500);

/// What smart downloads may use.
#[derive(Clone, Copy, Debug)]
pub struct SmartPolicy {
    pub budget_bytes: u64,
    pub max_height: u32,
    pub audio_only: bool,
}

/// How a [`Offline::run`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing left to do.
    Done,
    /// `stop` said so (the app's background time is up, the network changed).
    Stopped,
    /// A smart run made way for downloads the user asked for.
    Yielded,
    /// Another run is going (it picks up anything new).
    Busy,
    /// The connection failed; try again later.
    Offline,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub outcome: Outcome,
    pub downloaded: u32,
    pub failed: u32,
    /// The last error, if any.
    pub error: Option<String>,
}

/// The download in progress.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub video_id: String,
    pub done: u64,
    /// 0 until the streams are resolved.
    pub total: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Storage {
    pub saved_bytes: u64,
    pub smart_bytes: u64,
    /// Free space on the device, if known.
    pub free_bytes: Option<u64>,
}

/// A downloaded video to play.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalFile {
    pub path: PathBuf,
    /// Only the sound was downloaded.
    pub audio_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Saved,
    Smart,
}

/// Told about changes: `true` for progress only.
type Listener = Arc<dyn Fn(bool) + Send + Sync>;

#[derive(Clone, Copy, Debug)]
struct Running {
    kind: Kind,
    id: u64,
}

pub struct Offline {
    db: Db,
    http: Http,
    dir: PathBuf,
    /// Subscription uploads older than this aren't smart-downloaded.
    max_age_days: u32,
    /// The run going on, if any.
    running: Mutex<Option<Running>>,
    runs: AtomicU64,
    idle: Condvar,
    /// A saved run is waiting for the smart run to stop.
    yield_requested: AtomicBool,
    /// Smart downloads were turned off: a smart run going on stops.
    smart_cleared: AtomicBool,
    /// Bumped on every request, so a run that's about to finish looks again.
    requests: AtomicU64,
    active: Mutex<Option<Progress>>,
    /// The active download was removed: stop it.
    active_removed: AtomicBool,
    listener: Mutex<Option<Listener>>,
    notified: Mutex<Option<Instant>>,
}

/// Why one download didn't finish.
enum ItemError {
    Stopped,
    /// The user removed it while it downloaded.
    Removed,
    Network(String),
    Failed {
        error: String,
        permanent: bool,
    },
}

impl ItemError {
    fn failed(error: impl Into<String>) -> Self {
        ItemError::Failed {
            error: error.into(),
            permanent: false,
        }
    }

    fn permanent(error: impl Into<String>) -> Self {
        ItemError::Failed {
            error: error.into(),
            permanent: true,
        }
    }

    fn resolve(e: anyhow::Error) -> Self {
        if let Some(u) = e.downcast_ref::<Unplayable>() {
            ItemError::permanent(u.to_string())
        } else if e.downcast_ref::<ureq::Error>().is_some() {
            ItemError::Network(format!("{e:#}"))
        } else if let Some(api) = e.downcast_ref::<ApiError>() {
            ItemError::failed(api.to_string())
        } else {
            ItemError::failed(format!("{e:#}"))
        }
    }
}

impl From<anyhow::Error> for ItemError {
    fn from(e: anyhow::Error) -> Self {
        ItemError::failed(format!("{e:#}"))
    }
}

impl Offline {
    /// `dir`: where downloads are kept ([`crate::config::Paths::downloads_dir`]).
    pub fn new(db: Db, http: Http, dir: PathBuf, max_age_days: u32) -> Self {
        Self {
            db,
            http,
            dir,
            max_age_days,
            running: Mutex::new(None),
            runs: AtomicU64::new(0),
            idle: Condvar::new(),
            yield_requested: AtomicBool::new(false),
            smart_cleared: AtomicBool::new(false),
            requests: AtomicU64::new(0),
            active: Mutex::new(None),
            active_removed: AtomicBool::new(false),
            listener: Mutex::new(None),
            notified: Mutex::new(None),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Called (on the downloading thread) when downloads change: on every
    /// request, start, finish, failure and removal (`false`), and with `true`
    /// for progress, at most twice a second.
    pub fn set_listener(&self, listener: impl Fn(bool) + Send + Sync + 'static) {
        *self.listener.lock().unwrap() = Some(Arc::new(listener));
    }

    /// Queues a download the user asked for (or keeps a smart download of
    /// it). Something must then [`run`](Self::run) downloads.
    pub fn request(&self, id: &str, audio_only: bool, max_height: u32) -> Result<()> {
        ensure!(
            crate::youtube::parse_video_id(id).as_deref() == Some(id),
            "not a video id: {id}"
        );
        self.db
            .request_download(id, false, audio_only, max_height)?;
        self.requests.fetch_add(1, Ordering::SeqCst);
        self.notify(true);
        Ok(())
    }

    /// Deletes a download (or cancels it). Smart downloads won't fetch the
    /// video again.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.stop_if_active(id);
        let file = self.db.remove_download(id)?;
        self.delete_files(id, file.as_deref());
        self.notify(true);
        Ok(())
    }

    /// Deletes every smart download and stops a smart run that's going on
    /// (smart downloads were turned off).
    pub fn clear_smart(&self) -> Result<()> {
        self.smart_cleared.store(true, Ordering::SeqCst);
        for row in self.db.downloads()?.into_iter().filter(|r| r.smart) {
            self.stop_if_active(&row.video.id);
            self.evict(&row)?;
        }
        self.notify(true);
        Ok(())
    }

    /// Every download, waiting ones first (see [`Db::downloads`]).
    pub fn list(&self) -> Result<Vec<DownloadRow>> {
        self.db.downloads()
    }

    pub fn progress(&self) -> Option<Progress> {
        self.active.lock().unwrap().clone()
    }

    /// Downloads the user asked for that are still to do.
    pub fn waiting(&self) -> Result<u32> {
        self.db.saved_downloads_waiting()
    }

    pub fn storage(&self) -> Result<Storage> {
        let mut storage = Storage {
            free_bytes: free_bytes(if self.dir.is_dir() {
                &self.dir
            } else {
                self.dir.parent().unwrap_or(&self.dir)
            }),
            ..Storage::default()
        };
        for row in self.db.downloads()? {
            match (row.state, row.smart) {
                (DownloadState::Done, true) => storage.smart_bytes += row.bytes,
                (DownloadState::Done, false) => storage.saved_bytes += row.bytes,
                _ => {}
            }
        }
        Ok(storage)
    }

    /// The downloaded file of `id`, if there is one.
    pub fn local_file(&self, id: &str) -> Result<Option<LocalFile>> {
        let Some(row) = self.db.download(id)? else {
            return Ok(None);
        };
        let Some(file) = row.file.filter(|_| row.state == DownloadState::Done) else {
            return Ok(None);
        };
        let path = self.dir.join(file);
        if !path.is_file() {
            log::warn!("the download of {id} is gone from {}", path.display());
            self.db.forget_download(id)?;
            return Ok(None);
        }
        Ok(Some(LocalFile {
            path,
            audio_only: row.audio_only,
        }))
    }

    /// Downloads what the user asked for, oldest first, then (with `smart`)
    /// fills smart downloads up to their budget, until done or `stop` says
    /// so. One run at a time: a saved run makes a smart run stop and takes
    /// over; otherwise a second run returns [`Outcome::Busy`] at once (the
    /// first one picks up anything new before it finishes).
    pub fn run(
        &self,
        resolver: &dyn StreamResolver,
        smart: Option<&SmartPolicy>,
        stop: &dyn Fn() -> bool,
    ) -> Result<Report> {
        let kind = if smart.is_some() {
            Kind::Smart
        } else {
            Kind::Saved
        };
        let mut report = Report {
            outcome: Outcome::Done,
            downloaded: 0,
            failed: 0,
            error: None,
        };
        let Some(run) = self.begin(kind, stop) else {
            report.outcome = if stop() {
                Outcome::Stopped
            } else {
                Outcome::Busy
            };
            return Ok(report);
        };
        let _end = RunGuard(self, run);
        if kind == Kind::Smart {
            self.smart_cleared.store(false, Ordering::SeqCst);
        }
        let stopped = || {
            stop()
                || (kind == Kind::Smart
                    && (self.yield_requested.load(Ordering::SeqCst)
                        || self.smart_cleared.load(Ordering::SeqCst)))
        };
        loop {
            let seen = self.requests.load(Ordering::SeqCst);
            let mut outcome = self.saved_pass(resolver, &stopped, &mut report)?;
            if outcome == Outcome::Done
                && let Some(policy) = smart
            {
                outcome = self.smart_pass(resolver, policy, &stopped, &mut report)?;
            }
            if outcome == Outcome::Stopped && !stop() && self.yield_requested.load(Ordering::SeqCst)
            {
                outcome = Outcome::Yielded;
            }
            if outcome != Outcome::Done {
                report.outcome = outcome;
                break;
            }
            // Done, unless something was asked for meanwhile. Checked with
            // `running` locked, so a run that `begin` turns away can't miss it.
            let mut running = self.running.lock().unwrap();
            if self.requests.load(Ordering::SeqCst) == seen {
                self.release(&mut running, run);
                break;
            }
        }
        log::info!(
            "downloads ({kind:?}): {:?}, {} downloaded, {} failed",
            report.outcome,
            report.downloaded,
            report.failed
        );
        Ok(report)
    }

    /// Claims the right to run; returns the run's id.
    fn begin(&self, kind: Kind, stop: &dyn Fn() -> bool) -> Option<u64> {
        let mut running = self.running.lock().unwrap();
        loop {
            match *running {
                None => {
                    let id = self.runs.fetch_add(1, Ordering::SeqCst);
                    *running = Some(Running { kind, id });
                    return Some(id);
                }
                // What the user asked for comes first.
                Some(Running {
                    kind: Kind::Smart, ..
                }) if kind == Kind::Saved && !stop() => {
                    self.yield_requested.store(true, Ordering::SeqCst);
                    running = self
                        .idle
                        .wait_timeout(running, Duration::from_millis(250))
                        .unwrap()
                        .0;
                }
                Some(_) => return None,
            }
        }
    }

    /// Ends run `run`, if it still holds the claim.
    fn release(&self, running: &mut MutexGuard<Option<Running>>, run: u64) {
        if running.is_some_and(|r| r.id == run) {
            **running = None;
            self.yield_requested.store(false, Ordering::SeqCst);
            *self.active.lock().unwrap() = None;
            self.idle.notify_all();
        }
    }

    fn saved_pass(
        &self,
        resolver: &dyn StreamResolver,
        stopped: &dyn Fn() -> bool,
        report: &mut Report,
    ) -> Result<Outcome> {
        while let Some(job) = self.db.next_saved_download()? {
            if stopped() {
                return Ok(Outcome::Stopped);
            }
            match self.download(resolver, &job, SAVED_MIN_FREE, stopped, &mut |_| true) {
                Ok(_) => report.downloaded += 1,
                Err(e) => {
                    if let Some(outcome) = self.failed(&job, e, report)? {
                        return Ok(outcome);
                    }
                }
            }
        }
        Ok(Outcome::Done)
    }

    fn smart_pass(
        &self,
        resolver: &dyn StreamResolver,
        policy: &SmartPolicy,
        stopped: &dyn Fn() -> bool,
        report: &mut Report,
    ) -> Result<Outcome> {
        let candidates = self.db.smart_candidates(CandidateLimits {
            in_progress: 20,
            subscriptions: 300,
            for_you: 150,
            max_age_days: self.max_age_days,
        })?;
        let ranks: HashMap<&str, usize> = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id.as_str(), i))
            .collect();

        // Watched ones have done their job; unfinished ones nobody wants
        // any more go too.
        let mut kept = Vec::new();
        let mut have = HashSet::new();
        for row in self.db.downloads()? {
            let rank = ranks.get(row.video.id.as_str()).copied();
            match (row.smart, row.state) {
                (false, _) => {
                    have.insert(row.video.id);
                }
                (true, DownloadState::Done) if row.video.watched => self.evict(&row)?,
                (true, DownloadState::Done) => {
                    have.insert(row.video.id.clone());
                    kept.push(Kept {
                        id: row.video.id,
                        bytes: row.bytes,
                        rank,
                        completed_at: row.completed_at.unwrap_or(0),
                    });
                }
                (true, DownloadState::Queued) if rank.is_none() => self.evict(&row)?,
                (true, _) => {}
            }
        }
        let budget = policy.budget_bytes;
        let mut used: u64 = kept.iter().map(|k| k.bytes).sum();
        // The budget got smaller.
        for id in plan::make_room(&kept, used, 0, budget, None).unwrap_or_default() {
            self.evict_kept(&id, &mut kept, &mut used)?;
        }

        let mut misses = 0;
        for (rank, candidate) in candidates.iter().enumerate() {
            if stopped() {
                return Ok(Outcome::Stopped);
            }
            if misses >= MAX_MISSES {
                break;
            }
            if have.contains(&candidate.id) {
                continue;
            }
            let estimate = plan::estimate_bytes(
                candidate.duration_secs,
                policy.max_height,
                policy.audio_only,
            );
            if estimate > budget / MAX_SHARE {
                continue;
            }
            if plan::make_room(&kept, used, estimate, budget, Some(rank)).is_none() {
                misses += 1;
                continue;
            }
            self.db
                .request_download(&candidate.id, true, policy.audio_only, policy.max_height)?;
            let Some(job) = self.db.download(&candidate.id)? else {
                continue;
            };
            if !job.smart || job.state != DownloadState::Queued {
                continue;
            }
            // Now the exact size is known: make room for it, or skip it.
            let mut reserve =
                |total: u64| match plan::make_room(&kept, used, total, budget, Some(rank)) {
                    Some(victims) => {
                        for id in victims {
                            if let Err(e) = self.evict_kept(&id, &mut kept, &mut used) {
                                log::warn!("deleting smart download {id}: {e:#}");
                            }
                        }
                        true
                    }
                    None => false,
                };
            match self.download(resolver, &job, SMART_MIN_FREE, stopped, &mut reserve) {
                Ok(Some(bytes)) => {
                    report.downloaded += 1;
                    misses = 0;
                    used += bytes;
                    kept.push(Kept {
                        id: candidate.id.clone(),
                        bytes,
                        rank: Some(rank),
                        completed_at: chrono::Utc::now().timestamp(),
                    });
                }
                Ok(None) => {
                    misses += 1;
                    self.db.forget_download(&candidate.id)?;
                }
                Err(e) => {
                    if let Some(outcome) = self.failed(&job, e, report)? {
                        return Ok(outcome);
                    }
                }
            }
        }
        // Whatever this pass left unfinished (too big now, no room, a
        // stopped run's leftovers) isn't wanted: don't keep its parts.
        for row in self.db.downloads()? {
            if row.smart && row.state == DownloadState::Queued {
                self.evict(&row)?;
            }
        }
        Ok(Outcome::Done)
    }

    /// Records why `job` didn't finish. Returns the run's outcome if it
    /// should stop there.
    fn failed(
        &self,
        job: &DownloadRow,
        e: ItemError,
        report: &mut Report,
    ) -> Result<Option<Outcome>> {
        let id = &job.video.id;
        match e {
            ItemError::Removed => Ok(None),
            ItemError::Stopped => Ok(Some(Outcome::Stopped)),
            ItemError::Network(error) => {
                log::info!("download of {id} paused: {error}");
                report.error = Some(error);
                Ok(Some(Outcome::Offline))
            }
            ItemError::Failed { error, permanent } => {
                // A smart download is optional: the next video takes its place.
                let give_up = permanent || job.smart || job.attempts + 1 >= MAX_ATTEMPTS;
                log::warn!(
                    "download of {id} failed{}: {error}",
                    if give_up { " for good" } else { "" }
                );
                self.db.fail_download(id, &error, give_up)?;
                if give_up {
                    report.failed += 1;
                    self.delete_files(id, None);
                }
                report.error = Some(error);
                self.notify(true);
                Ok(None)
            }
        }
    }

    /// Downloads `job`. `reserve(total)` is asked once the size is known;
    /// `false` skips the video (`Ok(None)`). Returns the size on disk.
    fn download(
        &self,
        resolver: &dyn StreamResolver,
        job: &DownloadRow,
        min_free: u64,
        stopped: &dyn Fn() -> bool,
        reserve: &mut dyn FnMut(u64) -> bool,
    ) -> Result<Option<u64>, ItemError> {
        let id = job.video.id.as_str();
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| ItemError::failed(format!("Couldn't create the downloads folder: {e}")))?;
        self.start(id);
        let prefs = Prefs {
            max_tier: job.max_height,
            codecs: vec!["avc1".into()],
            only_codecs: true,
            audio_mime: Some("audio/mp4".into()),
        };
        let stop = || stopped() || self.active_removed.load(Ordering::SeqCst);
        let mut streams = job.streams.clone();
        let mut reserved = false;
        let mut resolves = 0;
        let parts = 'resolve: loop {
            resolves += 1;
            let r = resolver.resolve(id, &prefs).map_err(ItemError::resolve)?;
            if r.is_live {
                return Err(ItemError::permanent("Live streams can't be downloaded"));
            }
            let mut wanted = Vec::new();
            if !job.audio_only {
                let video = r.video.ok_or_else(|| {
                    ItemError::permanent("YouTube has no H.264 version of this video")
                })?;
                wanted.push(("video", video));
            }
            let audio = r
                .audio
                .ok_or_else(|| ItemError::permanent("YouTube has no AAC audio for this video"))?;
            wanted.push(("audio", audio));
            let mut parts = Vec::new();
            for (kind, s) in wanted {
                let len = s
                    .content_length
                    .ok_or_else(|| ItemError::failed("YouTube didn't say how big the video is"))?;
                parts.push((
                    self.dir.join(format!("{id}.{kind}.part")),
                    s.url,
                    len,
                    s.itag,
                ));
            }
            let now: Vec<String> = parts.iter().map(|p| format!("{}:{}", p.3, p.2)).collect();
            let now = now.join(" ");
            if streams.as_deref() != Some(now.as_str()) {
                // Different streams than the parts on disk: start over.
                for (path, ..) in &parts {
                    remove(path);
                }
                self.db.set_download_streams(id, &now)?;
                streams = Some(now);
            }
            let total: u64 = parts.iter().map(|p| p.2).sum();
            if !reserved {
                if !reserve(total) {
                    return Ok(None);
                }
                // Room for the rest of the parts, and the remuxed copy of them.
                let have: u64 = parts.iter().map(|p| file_len(&p.0)).sum();
                if let Some(free) = free_bytes(&self.dir)
                    && free < total.saturating_sub(have) + total + min_free
                {
                    return Err(ItemError::failed("Not enough free storage"));
                }
                reserved = true;
            }
            let mut before = 0;
            for (path, url, len, _) in &parts {
                let result = fetch::fetch(
                    &self.http,
                    url,
                    VISIONOS.user_agent,
                    path,
                    *len,
                    &stop,
                    &mut |n| self.set_progress(before + n, total),
                );
                match result {
                    Ok(()) => before += len,
                    // Stream URLs expire after about 6 hours.
                    Err(FetchError::Refused(status)) if resolves < 3 => {
                        log::info!("download of {id}: HTTP {status}, resolving again");
                        continue 'resolve;
                    }
                    Err(FetchError::Refused(status)) => {
                        return Err(ItemError::failed(format!(
                            "YouTube refused the download (HTTP {status})"
                        )));
                    }
                    Err(FetchError::Stopped) if self.active_removed.load(Ordering::SeqCst) => {
                        return Err(ItemError::Removed);
                    }
                    Err(FetchError::Stopped) => return Err(ItemError::Stopped),
                    Err(FetchError::Network(e)) => return Err(ItemError::Network(e)),
                    Err(FetchError::Io(e)) => {
                        return Err(ItemError::failed(format!(
                            "Couldn't save the download: {e}"
                        )));
                    }
                }
            }
            break parts;
        };

        let file = format!("{id}.{}", if job.audio_only { "m4a" } else { "mp4" });
        let assembling = self.dir.join(format!("{id}.mux.part"));
        let inputs: Vec<&Path> = parts.iter().map(|p| p.0.as_path()).collect();
        let muxed = mux::mux(&inputs, &assembling);
        for (path, ..) in &parts {
            remove(path);
        }
        let muxed = muxed.map_err(|e| {
            remove(&assembling);
            ItemError::failed(format!("Couldn't put the download together: {e:#}"))
        })?;
        let path = self.dir.join(&file);
        std::fs::rename(&assembling, &path)
            .map_err(|e| ItemError::failed(format!("Couldn't save the download: {e}")))?;
        match self.db.finish_download(id, &file, muxed.bytes)? {
            Finish::Done { replaced } => {
                if let Some(old) = replaced {
                    remove(&self.dir.join(old));
                }
                log::info!(
                    "downloaded {id}{}: {:.1} MB",
                    if job.smart { " (smart)" } else { "" },
                    muxed.bytes as f64 / 1e6
                );
                self.notify(true);
                Ok(Some(muxed.bytes))
            }
            Finish::Gone => {
                remove(&path);
                Err(ItemError::Removed)
            }
        }
    }

    /// Deletes a smart download, which may be downloaded again later.
    fn evict(&self, row: &DownloadRow) -> Result<()> {
        self.delete_files(&row.video.id, row.file.as_deref());
        self.db.forget_download(&row.video.id)
    }

    fn evict_kept(&self, id: &str, kept: &mut Vec<Kept>, used: &mut u64) -> Result<()> {
        if let Some(row) = self.db.download(id)? {
            self.evict(&row)?;
        }
        if let Some(i) = kept.iter().position(|k| k.id == id) {
            *used = used.saturating_sub(kept.remove(i).bytes);
        }
        Ok(())
    }

    fn delete_files(&self, id: &str, file: Option<&str>) {
        if let Some(file) = file {
            remove(&self.dir.join(file));
        }
        for part in ["video", "audio", "mux"] {
            remove(&self.dir.join(format!("{id}.{part}.part")));
        }
    }

    fn start(&self, id: &str) {
        self.active_removed.store(false, Ordering::SeqCst);
        *self.active.lock().unwrap() = Some(Progress {
            video_id: id.to_string(),
            done: 0,
            total: 0,
        });
        self.notify(true);
    }

    fn stop_if_active(&self, id: &str) {
        if self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|p| p.video_id == id)
        {
            self.active_removed.store(true, Ordering::SeqCst);
        }
    }

    fn set_progress(&self, done: u64, total: u64) {
        if let Some(p) = self.active.lock().unwrap().as_mut() {
            p.done = done;
            p.total = total;
        }
        self.notify(false);
    }

    /// `always`: something other than progress changed.
    fn notify(&self, always: bool) {
        {
            let mut last = self.notified.lock().unwrap();
            if !always && last.is_some_and(|t| t.elapsed() < NOTIFY_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        let listener = self.listener.lock().unwrap().clone();
        if let Some(listener) = listener {
            listener(!always);
        }
    }
}

/// Ends the run however [`Offline::run`] returns.
struct RunGuard<'a>(&'a Offline, u64);

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.release(&mut self.0.running.lock().unwrap(), self.1);
        self.0.notify(true);
    }
}

fn remove(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("deleting {}: {e}", path.display()),
    }
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Space available to the app on the volume holding `path`.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a statvfs to fill in.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    Some((stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64))
}

#[cfg(not(unix))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::AtomicUsize;

    use super::mux::tests::{S, fragmented};
    use super::*;
    use crate::db::{Offline as OfflineState, VideoRow};
    use crate::feed::{FeedItem, FeedKind, Retain};
    use crate::resolve::{Resolved, Stream};
    use crate::shorts::{DurationInfo, LinkHint, Verdict};

    /// Serves byte ranges of in-memory files over plain HTTP, like
    /// googlevideo. Paths starting with `expired` answer 403.
    fn serve(files: HashMap<String, Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let files = Arc::new(files);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let files = files.clone();
                std::thread::spawn(move || handle(stream, &files));
            }
        });
        base
    }

    fn handle(stream: TcpStream, files: &HashMap<String, Vec<u8>>) {
        let mut out = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let mut range = None;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                    break;
                }
                if let Some(r) = header.to_ascii_lowercase().strip_prefix("range: bytes=") {
                    let (a, b) = r.trim().split_once('-').unwrap();
                    range = Some((a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()));
                }
            }
            let name = path.trim_start_matches('/');
            let reply = match files.get(name) {
                _ if name.starts_with("expired") => {
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_vec()
                }
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
                Some(body) => {
                    let (a, b) = range.unwrap_or((0, body.len() - 1));
                    let b = b.min(body.len() - 1);
                    let mut r = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {a}-{b}/{}\r\n\r\n",
                        b + 1 - a,
                        body.len()
                    )
                    .into_bytes();
                    r.extend_from_slice(&body[a..=b]);
                    r
                }
            };
            if out.write_all(&reply).is_err() {
                return;
            }
        }
    }

    /// Resolves to the test server: `<id>.v` and `<id>.a`. With
    /// `expire_first`, the first answer's URLs are refused.
    struct FakeResolver {
        base: String,
        lens: HashMap<String, (u64, u64)>,
        calls: AtomicUsize,
        expire_first: bool,
    }

    impl StreamResolver for FakeResolver {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn resolve(&self, id: &str, prefs: &Prefs) -> Result<Resolved> {
            assert!(prefs.only_codecs && prefs.codecs == ["avc1"]);
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let prefix = if self.expire_first && n == 0 {
                "expired-"
            } else {
                ""
            };
            let (v, a) = self.lens[id];
            let stream = |suffix: &str, itag, len| Stream {
                itag,
                url: format!("{}/{prefix}{id}.{suffix}", self.base),
                mime: String::new(),
                codecs: String::new(),
                width: None,
                height: None,
                tier: None,
                fps: None,
                bitrate: 0,
                content_length: Some(len),
            };
            Ok(Resolved {
                video_id: id.into(),
                title: id.into(),
                is_live: false,
                video: Some(stream("v", 136, v)),
                audio: Some(stream("a", 140, a)),
                hls: None,
                expires_in: None,
            })
        }
    }

    /// Videos `ids` (11 characters each), `kb` KB of video each, served and
    /// listed in Subscriptions (newest first, in this order) or For you.
    struct Fixture {
        offline: Offline,
        resolver: FakeResolver,
        db: Db,
        dir: PathBuf,
    }

    fn video(tag: u8, kb: u32) -> (Vec<u8>, Vec<u8>) {
        let v: Vec<S> = (0..10)
            .map(|i| (3000, kb * 1024 / 10, i % 5 == 0, 0))
            .collect();
        let a: Vec<S> = (0..10).map(|_| (1024, 64, true, 0)).collect();
        (
            fragmented(tag, b"vide", 30000, &v, 5, None),
            fragmented(tag, b"soun", 10240, &a, 5, None),
        )
    }

    fn fixture(name: &str, subscriptions: &[(&str, u32)], for_you: &[(&str, u32)]) -> Fixture {
        let db = Db::open_in_memory().unwrap();
        let mut files = HashMap::new();
        let mut lens = HashMap::new();
        let mut verdicts = vec![];
        for (kind, list) in [
            (FeedKind::Subscriptions, subscriptions),
            (FeedKind::Home, for_you),
        ] {
            let items: Vec<FeedItem> = list
                .iter()
                .enumerate()
                .map(|(i, &(id, _))| FeedItem {
                    video_id: id.into(),
                    channel_id: "UC1".into(),
                    channel_title: "Channel".into(),
                    title: format!("{id} title"),
                    published: chrono::Utc::now() - chrono::Duration::hours(i as i64 + 1),
                    link_hint: LinkHint::WatchLink,
                    duration: Some(DurationInfo {
                        secs: 1,
                        live_or_upcoming: false,
                    }),
                })
                .collect();
            let retain = match kind {
                FeedKind::Subscriptions => Retain::Channels(vec!["UC1".into()]),
                _ => Retain::OnlyFetched,
            };
            db.upsert_items(kind, &items, &retain).unwrap();
            for (i, &(id, kb)) in list.iter().enumerate() {
                let (v, a) = video(i as u8 + 1, kb);
                lens.insert(id.to_string(), (v.len() as u64, a.len() as u64));
                files.insert(format!("{id}.v"), v);
                files.insert(format!("{id}.a"), a);
                verdicts.push((id.to_string(), Verdict::NotShort));
            }
        }
        db.set_verdicts(&verdicts).unwrap();
        let base = serve(files);
        let dir =
            std::env::temp_dir().join(format!("yt-lite-offline-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let offline = Offline::new(db.clone(), Http::new(), dir.clone(), 60);
        Fixture {
            offline,
            resolver: FakeResolver {
                base,
                lens,
                calls: AtomicUsize::new(0),
                expire_first: false,
            },
            db,
            dir,
        }
    }

    impl Fixture {
        fn run(&self, smart: Option<&SmartPolicy>) -> Report {
            self.offline.run(&self.resolver, smart, &|| false).unwrap()
        }

        fn row(&self, id: &str) -> VideoRow {
            let feed = |kind| {
                self.db
                    .feed(crate::db::FeedQuery {
                        kind,
                        max_age_days: 60,
                        limit: 100,
                        hide_watched: false,
                    })
                    .unwrap()
            };
            feed(FeedKind::Subscriptions)
                .into_iter()
                .chain(feed(FeedKind::Home))
                .find(|v| v.id == id)
                .unwrap()
        }

        /// Smart downloads that finished, by id.
        fn smart(&self) -> Vec<String> {
            let mut ids: Vec<String> = self
                .offline
                .list()
                .unwrap()
                .into_iter()
                .filter(|r| r.smart && r.state == DownloadState::Done)
                .map(|r| r.video.id)
                .collect();
            ids.sort();
            ids
        }

        fn files(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.dir)
                .map(|d| {
                    d.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    const A: &str = "aaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbb";
    const C: &str = "ccccccccccc";
    const D: &str = "ddddddddddd";
    const E: &str = "eeeeeeeeeee";

    #[test]
    fn saved_download_end_to_end() {
        let f = fixture("saved", &[(A, 20)], &[]);
        assert!(
            f.offline.request("../../etc", false, 720).is_err(),
            "ids only"
        );
        f.offline.request(A, false, 720).unwrap();
        assert_eq!(f.row(A).offline, OfflineState::Queued);
        assert_eq!(f.offline.waiting().unwrap(), 1);
        let changes = Arc::new(AtomicUsize::new(0));
        let counter = changes.clone();
        f.offline.set_listener(move |progress_only| {
            if !progress_only {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });

        let report = f.run(None);
        assert_eq!(
            (report.outcome, report.downloaded, report.failed),
            (Outcome::Done, 1, 0)
        );
        assert!(
            changes.load(Ordering::SeqCst) >= 2,
            "start and finish reported"
        );
        assert_eq!(f.row(A).offline, OfflineState::Saved);
        assert_eq!(f.offline.waiting().unwrap(), 0);
        assert_eq!(f.offline.progress(), None);
        let local = f.offline.local_file(A).unwrap().unwrap();
        assert!(!local.audio_only);
        assert_eq!(f.files(), [format!("{A}.mp4")], "no parts left behind");
        let mp4 = std::fs::read(&local.path).unwrap();
        assert_eq!(&mp4[4..8], b"ftyp");
        let storage = f.offline.storage().unwrap();
        assert_eq!(storage.saved_bytes, mp4.len() as u64);
        assert_eq!(storage.smart_bytes, 0);
        assert!(storage.free_bytes.is_some());

        // Nothing left: a second run is quick.
        assert_eq!(f.run(None).downloaded, 0);
    }

    #[test]
    fn audio_only_then_with_the_picture() {
        let f = fixture("audio", &[(A, 20)], &[]);
        f.offline.request(A, true, 720).unwrap();
        f.run(None);
        assert!(f.offline.local_file(A).unwrap().unwrap().audio_only);
        assert_eq!(f.files(), [format!("{A}.m4a")]);
        // Asking for audio again changes nothing; asking for video replaces it.
        f.offline.request(A, true, 720).unwrap();
        assert_eq!(f.row(A).offline, OfflineState::Saved);
        f.offline.request(A, false, 720).unwrap();
        assert_eq!(f.row(A).offline, OfflineState::Queued);
        assert!(
            f.offline.local_file(A).unwrap().is_none(),
            "not playable while it's replaced"
        );
        f.run(None);
        assert!(!f.offline.local_file(A).unwrap().unwrap().audio_only);
        assert_eq!(f.files(), [format!("{A}.mp4")]);
    }

    #[test]
    fn expired_urls_are_resolved_again() {
        let mut f = fixture("expired", &[(A, 20)], &[]);
        f.resolver.expire_first = true;
        f.offline.request(A, false, 720).unwrap();
        let report = f.run(None);
        assert_eq!(report.downloaded, 1, "{report:?}");
        assert_eq!(f.resolver.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn no_connection_keeps_it_queued() {
        let mut f = fixture("offline", &[(A, 20)], &[]);
        // Nothing listens there.
        let unused = TcpListener::bind("127.0.0.1:0").unwrap();
        f.resolver.base = format!("http://{}", unused.local_addr().unwrap());
        drop(unused);
        f.offline.request(A, false, 720).unwrap();
        let report = f.offline.run(&f.resolver, None, &|| false).unwrap();
        assert_eq!(report.outcome, Outcome::Offline);
        assert_eq!(f.row(A).offline, OfflineState::Queued);
        assert_eq!(f.offline.waiting().unwrap(), 1);
    }

    #[test]
    fn stop_leaves_it_to_resume() {
        let f = fixture("stop", &[(A, 20)], &[]);
        f.offline.request(A, false, 720).unwrap();
        let report = f.offline.run(&f.resolver, None, &|| true).unwrap();
        assert_eq!(report.outcome, Outcome::Stopped);
        assert_eq!(f.offline.waiting().unwrap(), 1);
        assert_eq!(f.run(None).downloaded, 1);
    }

    #[test]
    fn removing_deletes_and_smart_downloads_leave_it_alone() {
        let f = fixture("remove", &[(A, 20), (B, 20)], &[]);
        f.offline.request(A, false, 720).unwrap();
        f.run(None);
        f.offline.remove(A).unwrap();
        assert_eq!(f.row(A).offline, OfflineState::No);
        assert!(f.files().is_empty());
        assert!(f.offline.list().unwrap().is_empty());
        let policy = SmartPolicy {
            budget_bytes: 100 << 20,
            max_height: 720,
            audio_only: false,
        };
        f.run(Some(&policy));
        assert_eq!(f.smart(), [B]);
        // Asking for it again brings it back.
        f.offline.request(A, false, 720).unwrap();
        f.run(None);
        assert_eq!(f.row(A).offline, OfflineState::Saved);
    }

    #[test]
    fn smart_downloads_fill_the_budget_in_order_and_replace_watched() {
        // About 1 MB each; room for two.
        let f = fixture(
            "smart",
            &[(A, 1000), (B, 1000), (C, 1000)],
            &[(D, 1000), (E, 1000)],
        );
        let policy = SmartPolicy {
            budget_bytes: 2_500_000,
            max_height: 720,
            audio_only: false,
        };
        let report = f.run(Some(&policy));
        assert_eq!(report.outcome, Outcome::Done);
        // Newest subscription uploads first.
        assert_eq!(f.smart(), [A, B]);
        assert_eq!(f.row(A).offline, OfflineState::Smart);
        assert_eq!(f.row(C).offline, OfflineState::No);
        assert!(f.offline.storage().unwrap().smart_bytes <= policy.budget_bytes);

        // Watching one makes room for the next.
        f.db.set_watched(A, true).unwrap();
        f.run(Some(&policy));
        assert_eq!(f.smart(), [B, C]);
        assert!(!f.files().contains(&format!("{A}.mp4")));

        // Something partly watched comes first, even from For you.
        f.db.mark_played(E).unwrap();
        f.db.save_progress(E, 30.0, Some(600.0)).unwrap();
        f.run(Some(&policy));
        assert_eq!(f.smart(), [B, E]);

        // Keeping one makes it the user's: it no longer counts.
        f.offline.request(B, false, 720).unwrap();
        assert_eq!(f.row(B).offline, OfflineState::Saved);
        f.run(Some(&policy));
        assert_eq!(f.smart(), [C, E]);

        // A smaller budget drops the least wanted; turning it off drops all.
        let small = SmartPolicy {
            budget_bytes: 1_300_000,
            ..policy
        };
        f.run(Some(&small));
        assert_eq!(f.smart(), [E]);
        f.offline.clear_smart().unwrap();
        assert!(f.smart().is_empty());
        assert_eq!(f.files(), [format!("{B}.mp4")]);
        // Turned off while a smart run goes on: it stops. The next one
        // starts afresh.
        let turned_off = AtomicBool::new(false);
        let report = f
            .offline
            .run(&f.resolver, Some(&policy), &|| {
                if !turned_off.swap(true, Ordering::SeqCst) {
                    f.offline.clear_smart().unwrap();
                }
                false
            })
            .unwrap();
        assert_eq!((report.outcome, report.downloaded), (Outcome::Stopped, 0));
        assert_eq!(f.run(Some(&policy)).outcome, Outcome::Done);
        assert_eq!(f.smart().len(), 2);
    }

    #[test]
    fn smart_leftovers_that_no_longer_fit_are_deleted() {
        let f = fixture("leftover", &[(A, 1000), (B, 1000)], &[]);
        let policy = SmartPolicy {
            budget_bytes: 100 << 20,
            max_height: 720,
            audio_only: false,
        };
        // A smart run stopped partway through A.
        let polls = AtomicUsize::new(0);
        let stopped = f
            .offline
            .run(&f.resolver, Some(&policy), &|| {
                f.offline.progress().is_some_and(|p| p.done > 0)
                    && polls.fetch_add(1, Ordering::SeqCst) > 0
            })
            .unwrap();
        assert_eq!(stopped.outcome, Outcome::Stopped);
        assert!(
            f.files().iter().any(|n| n.ends_with(".part")),
            "{:?}",
            f.files()
        );
        // Now the budget is too small for what was stopped: its parts go.
        let tiny = SmartPolicy {
            budget_bytes: 1 << 20,
            ..policy
        };
        assert_eq!(f.run(Some(&tiny)).outcome, Outcome::Done);
        assert!(
            !f.files().iter().any(|n| n.ends_with(".part")),
            "{:?}",
            f.files()
        );
        assert!(
            f.offline
                .list()
                .unwrap()
                .iter()
                .all(|r| r.state == DownloadState::Done)
        );
    }

    #[test]
    fn failed_smart_downloads_are_not_retried() {
        let mut f = fixture("smart-fail", &[(A, 20), (B, 20)], &[]);
        // A's streams aren't on the server.
        f.resolver.lens.insert(A.into(), (1, 1));
        f.resolver.base.push_str("/missing");
        let policy = SmartPolicy {
            budget_bytes: 100 << 20,
            max_height: 720,
            audio_only: false,
        };
        let report = f.run(Some(&policy));
        assert_eq!(report.failed, 2, "{report:?}");
        assert!(f.files().is_empty(), "{:?}", f.files());
        // Next time they're left out rather than tried again.
        let calls = f.resolver.calls.load(Ordering::SeqCst);
        f.run(Some(&policy));
        assert_eq!(f.resolver.calls.load(Ordering::SeqCst), calls);
    }

    #[test]
    fn one_run_at_a_time() {
        let f = fixture("busy", &[(A, 20)], &[]);
        let run = f.offline.begin(Kind::Saved, &|| false).unwrap();
        assert_eq!(f.offline.begin(Kind::Smart, &|| false), None);
        assert_eq!(f.offline.begin(Kind::Saved, &|| false), None);
        f.offline
            .release(&mut f.offline.running.lock().unwrap(), run);
        // A saved run waits for a smart one to make way.
        let smart = f.offline.begin(Kind::Smart, &|| false).unwrap();
        std::thread::scope(|s| {
            let waiting = s.spawn(|| f.offline.begin(Kind::Saved, &|| false));
            while !f.offline.yield_requested.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            f.offline
                .release(&mut f.offline.running.lock().unwrap(), smart);
            assert!(waiting.join().unwrap().is_some());
        });
        // A stale release doesn't end someone else's run.
        f.offline
            .release(&mut f.offline.running.lock().unwrap(), smart);
        assert!(f.offline.running.lock().unwrap().is_some());
    }
}
