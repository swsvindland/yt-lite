//! SQLite cache: videos (with durations and Shorts verdicts, fetched once),
//! channels, per-source feed membership, and local watched state.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::feed::{FeedItem, FeedKind, Retain};
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
    first_seen      INTEGER NOT NULL
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
    PRIMARY KEY (source, video_id)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

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
    pub fn upsert_items(&self, kind: FeedKind, items: &[FeedItem], retain: &Retain) -> Result<usize> {
        let now = Utc::now().timestamp();
        self.with(|c| {
            let tx = c.transaction()?;
            let mut inserted = 0;
            {
                let mut ins = tx.prepare(
                    "INSERT INTO videos(id, channel_id, channel_title, title, published, link_hint, first_seen)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(id) DO UPDATE SET
                        title=excluded.title,
                        channel_title=excluded.channel_title,
                        link_hint=CASE WHEN excluded.link_hint != 0 THEN excluded.link_hint ELSE videos.link_hint END",
                )?;
                // A /shorts/ link is definitive even if an earlier probe said otherwise.
                let mut mark_short = tx.prepare("UPDATE videos SET short=1 WHERE id=?1 AND ?2=1")?;
                let mut member = tx.prepare(
                    "INSERT OR IGNORE INTO feed_items(source, video_id) VALUES (?1, ?2)",
                )?;
                for it in items {
                    inserted += ins.execute(params![
                        it.video_id,
                        it.channel_id,
                        it.channel_title,
                        it.title,
                        it.published.timestamp(),
                        it.link_hint.as_db(),
                        now
                    ])?;
                    mark_short.execute(params![it.video_id, it.link_hint.as_db()])?;
                    member.execute([kind.as_str(), &it.video_id])?;
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
                let mut miss =
                    tx.prepare("UPDATE videos SET enrich_attempts = enrich_attempts + 1 WHERE id=?1")?;
                for id in requested {
                    if !details.iter().any(|d| d.id == *id) {
                        miss.execute([id])?;
                    }
                }
                let mut set = tx.prepare("UPDATE videos SET duration_secs=?2, live=?3 WHERE id=?1")?;
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
    pub fn feed(&self, q: FeedQuery) -> Result<Vec<VideoRow>> {
        let cutoff = Utc::now().timestamp() - i64::from(q.max_age_days) * 86_400;
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT v.id, v.title, v.channel_title, v.published, v.duration_secs, v.live,
                        v.watched_at IS NOT NULL
                 FROM feed_items f JOIN videos v ON v.id = f.video_id
                 WHERE f.source = ?1 AND v.short = 0 AND v.published >= ?2
                   AND (?3 = 0 OR v.watched_at IS NULL)
                 ORDER BY v.published DESC
                 LIMIT ?4",
            )?;
            stmt.query_map(
                params![q.kind.as_str(), cutoff, q.hide_watched, q.limit],
                |r| {
                    Ok(VideoRow {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        channel_title: r.get(2)?,
                        published: r.get(3)?,
                        duration_secs: r.get(4)?,
                        live: r.get(5)?,
                        watched: r.get(6)?,
                    })
                },
            )?
            .collect()
        })
    }

    pub fn set_watched(&self, id: &str, watched: bool) -> Result<()> {
        let ts = watched.then(|| Utc::now().timestamp());
        self.with(|c| {
            c.execute("UPDATE videos SET watched_at=?2 WHERE id=?1", params![id, ts])
                .map(drop)
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

        db.set_verdicts(&[("long".into(), Verdict::NotShort)]).unwrap();
        let feed = db.feed(query()).unwrap();
        assert_eq!(feed.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), vec!["long"]);
    }

    #[test]
    fn shorts_link_overrides_previous_verdict() {
        let db = Db::open_in_memory().unwrap();
        let chans = Retain::Channels(vec!["UC1".to_string()]);
        db.upsert_items(FeedKind::Subscriptions, &[item("v", "UC1", LinkHint::None, 1)], &chans)
            .unwrap();
        db.set_verdicts(&[("v".into(), Verdict::NotShort)]).unwrap();
        db.upsert_items(FeedKind::Subscriptions, &[item("v", "UC1", LinkHint::ShortsLink, 1)], &chans)
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
        assert_eq!(ids, vec!["a", "gone"], "shorts-linked videos need no details");
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
        assert_eq!(db.feed(query()).unwrap().len(), 2, "old video is past max_age");

        db.upsert_items(FeedKind::Subscriptions, &[], &Retain::Channels(vec!["UC1".into()])).unwrap();
        let ids: Vec<_> = db.feed(query()).unwrap().into_iter().map(|v| v.id).collect();
        assert_eq!(ids, vec!["a"]);

        db.set_watched("a", true).unwrap();
        assert!(db.feed(query()).unwrap()[0].watched);
        let mut q = query();
        q.hide_watched = true;
        assert!(db.feed(q).unwrap().is_empty());
        db.set_watched("a", false).unwrap();
        assert!(!db.feed(query()).unwrap()[0].watched);
    }

    #[test]
    fn only_fetched_replaces_membership() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_items(FeedKind::Home, &[item("a", "UC1", LinkHint::None, 1)], &Retain::OnlyFetched)
            .unwrap();
        db.upsert_items(FeedKind::Home, &[item("b", "UC2", LinkHint::None, 1)], &Retain::OnlyFetched)
            .unwrap();
        db.set_verdicts(&[("a".into(), Verdict::NotShort), ("b".into(), Verdict::NotShort)])
            .unwrap();
        let mut q = query();
        q.kind = FeedKind::Home;
        let ids: Vec<_> = db.feed(q).unwrap().into_iter().map(|v| v.id).collect();
        assert_eq!(ids, vec!["b"]);
        // Other sources are unaffected.
        assert!(db.feed(query()).unwrap().is_empty());
    }

    #[test]
    fn subscriptions_round_trip_and_meta() {
        let db = Db::open_in_memory().unwrap();
        db.set_subscriptions(&[
            Subscription { channel_id: "UC2".into(), title: "B".into() },
            Subscription { channel_id: "UC1".into(), title: "A".into() },
        ])
        .unwrap();
        db.set_subscriptions(&[Subscription { channel_id: "UC1".into(), title: "A2".into() }])
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
