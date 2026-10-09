//! SQLite cache: videos (with durations and Shorts verdicts, fetched once),
//! channels, per-source feed membership, and local watched state and
//! playback progress.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::feed::{FeedItem, FeedKind, Retain};
use crate::progress::Progress;
use crate::shorts::{DurationInfo, LinkHint, Verdict};
use crate::youtube::api::{Subscription, VideoDetails};

/// Videos that `videos.list` never returned (deleted/private) are retried this
/// many times before the probe decides on its own.
pub const MAX_ENRICH_ATTEMPTS: i64 = 3;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoRow {
    pub id: String,
    pub title: String,
    pub channel_title: String,
    pub published: i64,
    pub duration_secs: Option<u32>,
    pub live: bool,
    pub watched: bool,
    /// Fraction played (0–1) if it was stopped partway, for a progress bar.
    pub progress: Option<f64>,
    /// When it was last played or marked watched (unix seconds).
    pub last_played: Option<i64>,
}

/// A video awaiting a Shorts verdict.
#[derive(Clone, Debug)]
pub struct Unclassified {
    pub id: String,
    pub link_hint: LinkHint,
    pub duration: Option<DurationInfo>,
    pub enrich_attempts: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct FeedQuery {
    pub kind: FeedKind,
    pub max_age_days: u32,
    pub limit: u32,
    pub hide_watched: bool,
}

/// The `VideoRow` columns of `videos v`, in [`video_row`]'s order.
const VIDEO_COLUMNS: &str = "v.id, v.title, v.channel_title, v.published, v.duration_secs, v.live,
    v.watched_at IS NOT NULL,
    CASE WHEN v.duration_secs > 0 THEN min(v.resume_secs / v.duration_secs, 1.0) END,
    nullif(max(coalesce(v.played_at, 0), coalesce(v.watched_at, 0)), 0)";

fn video_row(r: &rusqlite::Row) -> rusqlite::Result<VideoRow> {
    Ok(VideoRow {
        id: r.get(0)?,
        title: r.get(1)?,
        channel_title: r.get(2)?,
        published: r.get(3)?,
        duration_secs: r.get(4)?,
        live: r.get(5)?,
        watched: r.get(6)?,
        progress: r.get(7)?,
        last_played: r.get(8)?,
    })
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS videos (
    id              TEXT PRIMARY KEY,
    channel_id      TEXT NOT NULL,
    channel_title   TEXT NOT NULL,
    title           TEXT NOT NULL,
    published       INTEGER NOT NULL,
    link_hint       INTEGER NOT NULL DEFAULT 0,
    duration_secs   INTEGER,
    live            INTEGER NOT NULL DEFAULT 0,
    enrich_attempts INTEGER NOT NULL DEFAULT 0,
    short           INTEGER,
    watched_at      INTEGER,
    first_seen      INTEGER NOT NULL,
    played_at       INTEGER,
    resume_secs     REAL
);
CREATE INDEX IF NOT EXISTS videos_published ON videos(published DESC);
CREATE INDEX IF NOT EXISTS videos_unclassified ON videos(short) WHERE short IS NULL;

CREATE TABLE IF NOT EXISTS channels (
    id          TEXT PRIMARY KEY,
    title       TEXT NOT NULL,
    subscribed  INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS feed_items (
    source    TEXT NOT NULL,
    video_id  TEXT NOT NULL,
    position  INTEGER,
    PRIMARY KEY (source, video_id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// Upgrades databases created by older builds.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    add_column(conn, "feed_items", "position", "INTEGER")?;
    if add_column(conn, "videos", "played_at", "INTEGER")? {
        // Older builds marked videos watched as soon as they started playing.
        conn.execute_batch("UPDATE videos SET played_at = watched_at")?;
    }
    add_column(conn, "videos", "resume_secs", "REAL")?;
    Ok(())
}

/// Adds a column the table was created without. Returns whether it did.
fn add_column(conn: &Connection, table: &str, column: &str, decl: &str) -> rusqlite::Result<bool> {
    let exists = conn
        .prepare(&format!(
            "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
        ))?
        .exists([column])?;
    if !exists {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(!exists)
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        // Keep SQLite's page cache small (~2 MB); this is a memory-budgeted app.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA cache_size=-2000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn with<R>(&self, f: impl FnOnce(&mut Connection) -> rusqlite::Result<R>) -> Result<R> {
        let mut conn = self.conn.lock().unwrap();
        Ok(f(&mut conn)?)
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        self.with(|c| {
            c.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
                .optional()
        })
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "INSERT INTO meta(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [key, value],
            )
            .map(drop)
        })
    }

    /// Replaces the subscribed channel set with the API's current list.
    pub fn set_subscriptions(&self, subs: &[Subscription]) -> Result<()> {
        self.with(|c| {
            let tx = c.transaction()?;
            tx.execute("UPDATE channels SET subscribed=0", [])?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO channels(id, title, subscribed) VALUES (?1, ?2, 1)
                     ON CONFLICT(id) DO UPDATE SET title=excluded.title, subscribed=1",
                )?;
                for s in subs {
                    stmt.execute([&s.channel_id, &s.title])?;
                }
            }
            tx.commit()
        })
    }

    pub fn subscribed_channels(&self) -> Result<Vec<Subscription>> {
        self.with(|c| {
            let mut stmt =
                c.prepare("SELECT id, title FROM channels WHERE subscribed=1 ORDER BY title")?;
            stmt.query_map([], |r| {
                Ok(Subscription {
                    channel_id: r.get(0)?,
                    title: r.get(1)?,
                })
            })?
            .collect()
        })
    }

    /// Inserts new videos and refreshes titles of known ones. Durations and
    /// verdicts already cached are kept. Membership in `kind` is pruned
    /// according to `retain`.
    pub fn upsert_items(
        &self,
        kind: FeedKind,
        items: &[FeedItem],
        retain: &Retain,
    ) -> Result<usize> {
        let now = Utc::now().timestamp();
        self.with(|c| {
            let tx = c.transaction()?;
            let mut inserted = 0;
            {
                // Listings that show an exact publish date (RSS) win over
                // ones that only say "3y ago" (related videos).
                let mut ins = tx.prepare(
                    "INSERT INTO videos(id, channel_id, channel_title, title, published, link_hint,
                                        duration_secs, live, first_seen)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(id) DO UPDATE SET
                        title=excluded.title,
                        channel_title=excluded.channel_title,
                        channel_id=CASE WHEN excluded.channel_id != '' THEN excluded.channel_id ELSE videos.channel_id END,
                        link_hint=CASE WHEN excluded.link_hint != 0 THEN excluded.link_hint ELSE videos.link_hint END,
                        duration_secs=COALESCE(videos.duration_secs, excluded.duration_secs),
                        live=CASE WHEN videos.duration_secs IS NULL THEN excluded.live ELSE videos.live END",
                )?;
                // A /shorts/ link is definitive even if an earlier probe said otherwise.
                let mut mark_short = tx.prepare("UPDATE videos SET short=1 WHERE id=?1 AND ?2=1")?;
                let mut member = tx.prepare(
                    "INSERT INTO feed_items(source, video_id, position) VALUES (?1, ?2, ?3)
                     ON CONFLICT(source, video_id) DO UPDATE SET position=excluded.position",
                )?;
                let ordered = matches!(retain, Retain::OnlyFetched);
                for (ix, it) in items.iter().enumerate() {
                    inserted += ins.execute(params![
                        it.video_id,
                        it.channel_id,
                        it.channel_title,
                        it.title,
                        it.published.timestamp(),
                        it.link_hint.as_db(),
                        it.duration.map(|d| d.secs),
                        it.duration.is_some_and(|d| d.live_or_upcoming),
                        now
                    ])?;
                    mark_short.execute(params![it.video_id, it.link_hint.as_db()])?;
                    member.execute(params![kind.as_str(), it.video_id, ordered.then_some(ix as i64)])?;
                }
            }
            tx.execute("CREATE TEMP TABLE IF NOT EXISTS keep_ids(id TEXT PRIMARY KEY)", [])?;
            tx.execute("DELETE FROM keep_ids", [])?;
            let (keep, prune_sql): (Vec<&str>, &str) = match retain {
                Retain::Channels(chs) => (
                    chs.iter().map(String::as_str).collect(),
                    "DELETE FROM feed_items WHERE source=?1 AND video_id IN (
                        SELECT id FROM videos WHERE channel_id NOT IN (SELECT id FROM keep_ids))",
                ),
                Retain::OnlyFetched => (
                    items.iter().map(|i| i.video_id.as_str()).collect(),
                    "DELETE FROM feed_items WHERE source=?1 AND video_id NOT IN (SELECT id FROM keep_ids)",
                ),
            };
            {
                let mut k = tx.prepare("INSERT OR IGNORE INTO keep_ids(id) VALUES (?1)")?;
                for id in keep {
                    k.execute([id])?;
                }
            }
            tx.execute(prune_sql, [kind.as_str()])?;
            tx.commit()?;
            Ok(inserted)
        })
    }

    /// Ids needing `videos.list`: not yet classified and no duration, plus
    /// recent live/upcoming videos whose duration will change.
    pub fn ids_needing_details(&self, limit: u32) -> Result<Vec<String>> {
        let recent = Utc::now().timestamp() - 7 * 86_400;
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM videos
                 WHERE (short IS NULL AND duration_secs IS NULL AND link_hint != 1 AND enrich_attempts < ?1)
                    OR (live = 1 AND published > ?2)
                 ORDER BY published DESC LIMIT ?3",
            )?;
            stmt.query_map(params![MAX_ENRICH_ATTEMPTS, recent, limit], |r| r.get(0))?
                .collect()
        })
    }

    /// Stores durations; ids requested but not returned get an attempt counted.
    pub fn store_details(&self, requested: &[&str], details: &[VideoDetails]) -> Result<()> {
        self.with(|c| {
            let tx = c.transaction()?;
            {
                let mut miss = tx.prepare(
                    "UPDATE videos SET enrich_attempts = enrich_attempts + 1 WHERE id=?1",
                )?;
                for id in requested {
                    if !details.iter().any(|d| d.id == *id) {
                        miss.execute([id])?;
                    }
                }
                let mut set =
                    tx.prepare("UPDATE videos SET duration_secs=?2, live=?3 WHERE id=?1")?;
                for d in details {
                    set.execute(params![d.id, d.duration.secs, d.duration.live_or_upcoming])?;
                }
            }
            tx.commit()
        })
    }

    pub fn unclassified(&self, limit: u32) -> Result<Vec<Unclassified>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, link_hint, duration_secs, live, enrich_attempts FROM videos
                 WHERE short IS NULL ORDER BY published DESC LIMIT ?1",
            )?;
            stmt.query_map([limit], |r| {
                let secs: Option<u32> = r.get(2)?;
                let live: bool = r.get(3)?;
                Ok(Unclassified {
                    id: r.get(0)?,
                    link_hint: LinkHint::from_db(r.get(1)?),
                    duration: secs.map(|secs| DurationInfo {
                        secs,
                        live_or_upcoming: live,
                    }),
                    enrich_attempts: r.get(4)?,
                })
            })?
            .collect()
        })
    }

    pub fn set_verdicts(&self, verdicts: &[(String, Verdict)]) -> Result<()> {
        self.with(|c| {
            let tx = c.transaction()?;
            {
                let mut stmt = tx.prepare("UPDATE videos SET short=?2 WHERE id=?1")?;
                for (id, v) in verdicts {
                    stmt.execute(params![id, v.as_db()])?;
                }
            }
            tx.commit()
        })
    }

    /// The displayable feed: only videos with a cached NotShort verdict.
    /// Ordered feeds (For you) keep their fetched order and ignore
    /// `max_age_days`; others are newest first.
    pub fn feed(&self, q: FeedQuery) -> Result<Vec<VideoRow>> {
        let cutoff = Utc::now().timestamp() - i64::from(q.max_age_days) * 86_400;
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {VIDEO_COLUMNS}
                 FROM feed_items f JOIN videos v ON v.id = f.video_id
                 WHERE f.source = ?1 AND v.short = 0
                   AND (f.position IS NOT NULL OR v.published >= ?2)
                   AND (?3 = 0 OR v.watched_at IS NULL)
                 ORDER BY f.position ASC NULLS LAST, v.published DESC
                 LIMIT ?4"
            ))?;
            stmt.query_map(
                params![q.kind.as_str(), cutoff, q.hide_watched, q.limit],
                video_row,
            )?
            .collect()
        })
    }

    /// Videos played or marked watched, most recent first.
    pub fn history(&self, limit: u32) -> Result<Vec<VideoRow>> {
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {VIDEO_COLUMNS} FROM videos v
                 WHERE v.played_at IS NOT NULL OR v.watched_at IS NOT NULL
                 ORDER BY max(coalesce(v.played_at, 0), coalesce(v.watched_at, 0)) DESC
                 LIMIT ?1"
            ))?;
            stmt.query_map([limit], video_row)?.collect()
        })
    }

    /// Takes a video out of History: it's no longer played or watched, and
    /// its resume point is gone.
    pub fn remove_from_history(&self, id: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE videos SET played_at=NULL, watched_at=NULL, resume_secs=NULL WHERE id=?1",
                [id],
            )
            .map(drop)
        })
    }

    /// Empties History (and with it watched marks and resume points).
    pub fn clear_history(&self) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE videos SET played_at=NULL, watched_at=NULL, resume_secs=NULL
                 WHERE played_at IS NOT NULL OR watched_at IS NOT NULL OR resume_secs IS NOT NULL",
                [],
            )
            .map(drop)
        })
    }

    /// Most recently played or watched video ids (seeds for For you).
    pub fn recently_played(&self, limit: u32) -> Result<Vec<String>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM videos WHERE played_at IS NOT NULL OR watched_at IS NOT NULL
                 ORDER BY max(coalesce(played_at, 0), coalesce(watched_at, 0)) DESC LIMIT ?1",
            )?;
            stmt.query_map([limit], |r| r.get(0))?.collect()
        })
    }

    /// Newest displayable videos of a feed (fallback seeds).
    pub fn newest_in_feed(&self, kind: FeedKind, limit: u32) -> Result<Vec<String>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT v.id FROM feed_items f JOIN videos v ON v.id = f.video_id
                 WHERE f.source = ?1 AND v.short = 0 ORDER BY v.published DESC LIMIT ?2",
            )?;
            stmt.query_map(params![kind.as_str(), limit], |r| r.get(0))?
                .collect()
        })
    }

    /// Videos played or marked watched (For you leaves them out).
    pub fn played_ids(&self) -> Result<std::collections::HashSet<String>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM videos WHERE played_at IS NOT NULL OR watched_at IS NOT NULL",
            )?;
            stmt.query_map([], |r| r.get(0))?.collect()
        })
    }

    /// Marking watched or unwatched also forgets the resume point.
    pub fn set_watched(&self, id: &str, watched: bool) -> Result<()> {
        let ts = watched.then(|| Utc::now().timestamp());
        self.with(|c| {
            c.execute(
                "UPDATE videos SET watched_at=?2, resume_secs=NULL WHERE id=?1",
                params![id, ts],
            )
            .map(drop)
        })
    }

    /// Records that playback of `id` started.
    pub fn mark_played(&self, id: &str) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE videos SET played_at=?2 WHERE id=?1",
                params![id, Utc::now().timestamp()],
            )
            .map(drop)
        })
    }

    /// Where to resume `id`, if it was stopped partway.
    pub fn resume_position(&self, id: &str) -> Result<Option<f64>> {
        self.with(|c| {
            c.query_row("SELECT resume_secs FROM videos WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .map(Option::flatten)
        })
    }

    /// Records where playback of `id` got to: a resume point, or watched once
    /// it's finished (see [`Progress`]). `duration`: the player's, if known;
    /// otherwise the listing's is used. `None` for live streams and unknown
    /// videos, which have no progress.
    pub fn save_progress(
        &self,
        id: &str,
        position: f64,
        duration: Option<f64>,
    ) -> Result<Option<Progress>> {
        self.with(|c| {
            let row: Option<(Option<u32>, bool)> = c
                .query_row(
                    "SELECT duration_secs, live FROM videos WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((listed, false)) = row else {
                return Ok(None);
            };
            let progress = Progress::at(position, duration.or(listed.map(f64::from)));
            match progress {
                Progress::Finished => c.execute(
                    "UPDATE videos SET watched_at=?2, resume_secs=NULL WHERE id=?1",
                    params![id, Utc::now().timestamp()],
                ),
                Progress::Resume(at) => c.execute(
                    "UPDATE videos SET resume_secs=?2 WHERE id=?1",
                    params![id, at],
                ),
                Progress::Start => {
                    c.execute("UPDATE videos SET resume_secs=NULL WHERE id=?1", [id])
                }
            }?;
            Ok(Some(progress))
        })
    }

    /// Ids of every video known to be a Short (for diagnostics/tests).
    #[cfg(test)]
    pub fn short_ids(&self) -> Result<Vec<String>> {
        self.with(|c| {
            let mut stmt = c.prepare("SELECT id FROM videos WHERE short=1 ORDER BY id")?;
            stmt.query_map([], |r| r.get(0))?.collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn item(id: &str, ch: &str, hint: LinkHint, age_days: i64) -> FeedItem {
        FeedItem {
            video_id: id.into(),
            channel_id: ch.into(),
            channel_title: format!("{ch} title"),
            title: format!("{id} title"),
            published: Utc::now() - Duration::days(age_days),
            link_hint: hint,
            duration: None,
        }
    }

    fn query() -> FeedQuery {
        FeedQuery {
            kind: FeedKind::Subscriptions,
            max_age_days: 30,
            limit: 100,
            hide_watched: false,
        }
    }

    #[test]
    fn feed_only_shows_confirmed_non_shorts() {
        let db = Db::open_in_memory().unwrap();
        let chans = Retain::Channels(vec!["UC1".to_string()]);
        db.upsert_items(
            FeedKind::Subscriptions,
            &[
                item("long", "UC1", LinkHint::WatchLink, 1),
                item("short", "UC1", LinkHint::ShortsLink, 2),
                item("pending", "UC1", LinkHint::WatchLink, 3),
            ],
            &chans,
        )
        .unwrap();
        // Nothing is displayable until classified.
        assert!(db.feed(query()).unwrap().is_empty());
        assert_eq!(db.short_ids().unwrap(), vec!["short"]);

        db.set_verdicts(&[("long".into(), Verdict::NotShort)])
            .unwrap();
        let feed = db.feed(query()).unwrap();
        assert_eq!(
            feed.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
            vec!["long"]
        );
    }

    #[test]
    fn shorts_link_overrides_previous_verdict() {
        let db = Db::open_in_memory().unwrap();
        let chans = Retain::Channels(vec!["UC1".to_string()]);
        db.upsert_items(
            FeedKind::Subscriptions,
            &[item("v", "UC1", LinkHint::None, 1)],
            &chans,
        )
        .unwrap();
        db.set_verdicts(&[("v".into(), Verdict::NotShort)]).unwrap();
        db.upsert_items(
            FeedKind::Subscriptions,
            &[item("v", "UC1", LinkHint::ShortsLink, 1)],
            &chans,
        )
        .unwrap();
        assert!(db.feed(query()).unwrap().is_empty());
    }

    #[test]
    fn details_and_enrich_attempts() {
        let db = Db::open_in_memory().unwrap();
        let chans = Retain::Channels(vec!["UC1".to_string()]);
        db.upsert_items(
            FeedKind::Subscriptions,
            &[
                item("a", "UC1", LinkHint::WatchLink, 1),
                item("gone", "UC1", LinkHint::WatchLink, 2),
                item("s", "UC1", LinkHint::ShortsLink, 3),
            ],
            &chans,
        )
        .unwrap();
        let ids = db.ids_needing_details(50).unwrap();
        assert_eq!(
            ids,
            vec!["a", "gone"],
            "shorts-linked videos need no details"
        );
        let details = vec![VideoDetails {
            id: "a".into(),
            duration: DurationInfo {
                secs: 600,
                live_or_upcoming: false,
            },
        }];
        for _ in 0..MAX_ENRICH_ATTEMPTS {
            db.store_details(&["a", "gone"], &details).unwrap();
        }
        assert!(db.ids_needing_details(50).unwrap().is_empty());
        let pending = db.unclassified(10).unwrap();
        let a = pending.iter().find(|u| u.id == "a").unwrap();
        assert_eq!(a.duration.unwrap().secs, 600);
        let gone = pending.iter().find(|u| u.id == "gone").unwrap();
        assert_eq!(gone.enrich_attempts, MAX_ENRICH_ATTEMPTS);
    }

    #[test]
    fn unsubscribed_channels_drop_out_and_age_and_watched_filters() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_items(
            FeedKind::Subscriptions,
            &[
                item("a", "UC1", LinkHint::WatchLink, 1),
                item("b", "UC2", LinkHint::WatchLink, 1),
                item("old", "UC1", LinkHint::WatchLink, 90),
            ],
            &Retain::Channels(vec!["UC1".into(), "UC2".into()]),
        )
        .unwrap();
        db.set_verdicts(&[
            ("a".into(), Verdict::NotShort),
            ("b".into(), Verdict::NotShort),
            ("old".into(), Verdict::NotShort),
        ])
        .unwrap();
        assert_eq!(
            db.feed(query()).unwrap().len(),
            2,
            "old video is past max_age"
        );

        db.upsert_items(
            FeedKind::Subscriptions,
            &[],
            &Retain::Channels(vec!["UC1".into()]),
        )
        .unwrap();
        let ids: Vec<_> = db
            .feed(query())
            .unwrap()
            .into_iter()
            .map(|v| v.id)
            .collect();
        assert_eq!(ids, vec!["a"]);

        db.set_watched("a", true).unwrap();
        assert!(db.feed(query()).unwrap()[0].watched);
        let mut q = query();
        q.hide_watched = true;
        assert!(db.feed(q).unwrap().is_empty());
        db.set_watched("a", false).unwrap();
        assert!(!db.feed(query()).unwrap()[0].watched);
    }

    /// A displayable 10-minute video and a live stream in Subscriptions.
    fn progress_db() -> Db {
        let db = Db::open_in_memory().unwrap();
        let mut ten_min = item("a", "UC1", LinkHint::WatchLink, 1);
        ten_min.duration = Some(DurationInfo {
            secs: 600,
            live_or_upcoming: false,
        });
        let mut live = item("live", "UC1", LinkHint::WatchLink, 1);
        live.duration = Some(DurationInfo {
            secs: 0,
            live_or_upcoming: true,
        });
        db.upsert_items(
            FeedKind::Subscriptions,
            &[ten_min, live],
            &Retain::Channels(vec!["UC1".into()]),
        )
        .unwrap();
        db.set_verdicts(&[
            ("a".into(), Verdict::NotShort),
            ("live".into(), Verdict::NotShort),
        ])
        .unwrap();
        db
    }

    fn row(db: &Db, id: &str) -> VideoRow {
        db.feed(query())
            .unwrap()
            .into_iter()
            .find(|v| v.id == id)
            .unwrap()
    }

    #[test]
    fn progress_resumes_then_finishes() {
        let db = progress_db();
        assert_eq!(db.resume_position("a").unwrap(), None);
        assert_eq!(row(&db, "a").progress, None);

        // Stopped partway: a resume point, not watched. The listing's
        // duration is used when the player doesn't know it.
        assert_eq!(
            db.save_progress("a", 150.0, None).unwrap(),
            Some(Progress::Resume(150.0))
        );
        assert_eq!(db.resume_position("a").unwrap(), Some(150.0));
        let a = row(&db, "a");
        assert!(!a.watched);
        assert_eq!(a.progress, Some(0.25));

        // Barely started again: nothing to resume.
        db.save_progress("a", 3.0, Some(600.0)).unwrap();
        assert_eq!(db.resume_position("a").unwrap(), None);

        // Played to the end: watched, resume point gone.
        db.save_progress("a", 300.0, Some(600.0)).unwrap();
        assert_eq!(
            db.save_progress("a", 595.0, Some(600.0)).unwrap(),
            Some(Progress::Finished)
        );
        let a = row(&db, "a");
        assert!(a.watched);
        assert_eq!(a.progress, None);
        assert_eq!(db.resume_position("a").unwrap(), None);
    }

    #[test]
    fn live_and_unknown_videos_have_no_progress() {
        let db = progress_db();
        assert_eq!(db.save_progress("live", 300.0, None).unwrap(), None);
        assert_eq!(db.resume_position("live").unwrap(), None);
        assert_eq!(db.save_progress("nope", 300.0, None).unwrap(), None);
        assert_eq!(db.resume_position("nope").unwrap(), None);
    }

    #[test]
    fn marking_watched_or_unwatched_forgets_the_resume_point() {
        let db = progress_db();
        db.save_progress("a", 150.0, None).unwrap();
        db.set_watched("a", true).unwrap();
        assert_eq!(db.resume_position("a").unwrap(), None);
        db.save_progress("a", 150.0, None).unwrap();
        db.set_watched("a", false).unwrap();
        assert_eq!(db.resume_position("a").unwrap(), None);
        assert!(!row(&db, "a").watched);
    }

    #[test]
    fn migration_keeps_old_watched_videos_as_played() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE videos (
                id TEXT PRIMARY KEY, channel_id TEXT NOT NULL, channel_title TEXT NOT NULL,
                title TEXT NOT NULL, published INTEGER NOT NULL,
                link_hint INTEGER NOT NULL DEFAULT 0, duration_secs INTEGER,
                live INTEGER NOT NULL DEFAULT 0, enrich_attempts INTEGER NOT NULL DEFAULT 0,
                short INTEGER, watched_at INTEGER, first_seen INTEGER NOT NULL);
             INSERT INTO videos(id, channel_id, channel_title, title, published, watched_at, first_seen)
             VALUES ('w', 'UC1', 'c', 't', 1, 1700000000, 1),
                    ('u', 'UC1', 'c', 't', 1, NULL, 1);",
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        assert_eq!(db.recently_played(5).unwrap(), vec!["w"]);
        let played_at: Option<i64> = db
            .with(|c| {
                c.query_row("SELECT played_at FROM videos WHERE id='w'", [], |r| {
                    r.get(0)
                })
            })
            .unwrap();
        assert_eq!(played_at, Some(1_700_000_000));
        assert_eq!(db.resume_position("w").unwrap(), None);
    }

    #[test]
    fn history_lists_played_and_watched_videos_newest_first() {
        let db = progress_db();
        let mut b = item("b", "UC1", LinkHint::WatchLink, 2);
        b.duration = Some(DurationInfo {
            secs: 300,
            live_or_upcoming: false,
        });
        db.upsert_items(
            FeedKind::Search,
            &[b, item("c", "UC1", LinkHint::None, 3)],
            &Retain::OnlyFetched,
        )
        .unwrap();
        assert!(db.history(10).unwrap().is_empty());

        db.mark_played("a").unwrap();
        db.save_progress("a", 150.0, None).unwrap();
        db.set_watched("b", true).unwrap();
        // "b" was marked watched an hour after "a" was played.
        db.with(|c| {
            c.execute(
                "UPDATE videos SET watched_at = (SELECT played_at FROM videos WHERE id='a') + 3600
                 WHERE id='b'",
                [],
            )
        })
        .unwrap();
        let history = db.history(10).unwrap();
        let ids: Vec<_> = history.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
        assert!(history[0].watched);
        assert_eq!(history[1].progress, Some(0.25));
        assert!(history[1].last_played.is_some());
        assert!(history[0].last_played > history[1].last_played);
        // Feeds report it too.
        assert_eq!(row(&db, "a").last_played, history[1].last_played);

        db.remove_from_history("b").unwrap();
        assert_eq!(db.history(10).unwrap().len(), 1);
        assert!(!db.played_ids().unwrap().contains("b"));

        db.clear_history().unwrap();
        assert!(db.history(10).unwrap().is_empty());
        assert_eq!(db.resume_position("a").unwrap(), None);
        assert!(!row(&db, "a").watched);
    }

    #[test]
    fn only_fetched_replaces_membership() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_items(
            FeedKind::Home,
            &[item("a", "UC1", LinkHint::None, 1)],
            &Retain::OnlyFetched,
        )
        .unwrap();
        db.upsert_items(
            FeedKind::Home,
            &[item("b", "UC2", LinkHint::None, 1)],
            &Retain::OnlyFetched,
        )
        .unwrap();
        db.set_verdicts(&[
            ("a".into(), Verdict::NotShort),
            ("b".into(), Verdict::NotShort),
        ])
        .unwrap();
        let mut q = query();
        q.kind = FeedKind::Home;
        let ids: Vec<_> = db.feed(q).unwrap().into_iter().map(|v| v.id).collect();
        assert_eq!(ids, vec!["b"]);

        // Fetched order is kept and old videos are not age-filtered.
        db.upsert_items(
            FeedKind::Home,
            &[
                item("old", "UC1", LinkHint::None, 900),
                item("a", "UC1", LinkHint::None, 1),
            ],
            &Retain::OnlyFetched,
        )
        .unwrap();
        db.set_verdicts(&[("old".into(), Verdict::NotShort)])
            .unwrap();
        let ids: Vec<_> = db.feed(q).unwrap().into_iter().map(|v| v.id).collect();
        assert_eq!(ids, vec!["old", "a"]);
        // Other sources are unaffected.
        assert!(db.feed(query()).unwrap().is_empty());
    }

    #[test]
    fn listing_durations_are_stored_and_skip_enrichment() {
        let db = Db::open_in_memory().unwrap();
        let mut it = item("d", "UC1", LinkHint::WatchLink, 1);
        it.duration = Some(DurationInfo {
            secs: 900,
            live_or_upcoming: false,
        });
        db.upsert_items(FeedKind::Home, &[it], &Retain::OnlyFetched)
            .unwrap();
        assert!(db.ids_needing_details(10).unwrap().is_empty());
        assert_eq!(db.unclassified(10).unwrap()[0].duration.unwrap().secs, 900);
    }

    #[test]
    fn migrates_old_feed_items_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE feed_items (source TEXT NOT NULL, video_id TEXT NOT NULL,
             PRIMARY KEY (source, video_id)) WITHOUT ROWID;",
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        db.upsert_items(
            FeedKind::Home,
            &[item("a", "UC1", LinkHint::None, 1)],
            &Retain::OnlyFetched,
        )
        .unwrap();
    }

    #[test]
    fn seeds() {
        let db = Db::open_in_memory().unwrap();
        let chans = Retain::Channels(vec!["UC1".to_string()]);
        db.upsert_items(
            FeedKind::Subscriptions,
            &[
                item("new", "UC1", LinkHint::None, 1),
                item("older", "UC1", LinkHint::None, 5),
            ],
            &chans,
        )
        .unwrap();
        db.set_verdicts(&[
            ("new".into(), Verdict::NotShort),
            ("older".into(), Verdict::NotShort),
        ])
        .unwrap();
        assert_eq!(
            db.newest_in_feed(FeedKind::Subscriptions, 1).unwrap(),
            vec!["new"]
        );
        db.set_watched("older", true).unwrap();
        assert_eq!(db.recently_played(5).unwrap(), vec!["older"]);
        assert!(db.played_ids().unwrap().contains("older"));
        // Started but not finished still counts, most recent first.
        db.mark_played("new").unwrap();
        db.with(|c| {
            c.execute(
                "UPDATE videos SET played_at = played_at + 10 WHERE id='new'",
                [],
            )
        })
        .unwrap();
        assert_eq!(db.recently_played(5).unwrap(), vec!["new", "older"]);
        assert!(db.played_ids().unwrap().contains("new"));
    }

    #[test]
    fn subscriptions_round_trip_and_meta() {
        let db = Db::open_in_memory().unwrap();
        db.set_subscriptions(&[
            Subscription {
                channel_id: "UC2".into(),
                title: "B".into(),
            },
            Subscription {
                channel_id: "UC1".into(),
                title: "A".into(),
            },
        ])
        .unwrap();
        db.set_subscriptions(&[Subscription {
            channel_id: "UC1".into(),
            title: "A2".into(),
        }])
        .unwrap();
        let subs = db.subscribed_channels().unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].title, "A2");

        assert_eq!(db.meta_get("k").unwrap(), None);
        db.meta_set("k", "1").unwrap();
        db.meta_set("k", "2").unwrap();
        assert_eq!(db.meta_get("k").unwrap().as_deref(), Some("2"));
    }
}
