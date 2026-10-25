//! For you's order: how likely you are to watch each candidate, learned from
//! what you played in yt-lite. Pure, so it's unit tested.
//!
//! - **Seeds** are recent videos you watched most of (recency × how much of
//!   it you watched); ones you left after a few seconds aren't seeds.
//! - A candidate's **relevance** adds up over every seed whose related list
//!   has it, weighted by the seed and by how high YouTube listed it there.
//! - **Channel affinity** comes from your history with the channel: a
//!   finished video counts 1, one stopped partway the share you watched, one
//!   left within seconds −½, each fading with a 45-day half-life.
//! - Smaller factors: subscribed channels, freshness, and whether the length
//!   is like what you usually finish.
//! - Then **variety**: each further video from a channel already picked
//!   counts for less, so one channel can't fill the top.

use std::collections::{HashMap, HashSet};

use super::FeedItem;

const DAY: f64 = 86_400.0;
/// History fades with this half-life for channel affinity.
const AFFINITY_HALF_LIFE_DAYS: f64 = 45.0;
/// Seeds favor what you watched lately more strongly.
const SEED_HALF_LIFE_DAYS: f64 = 14.0;
/// A video left within seconds says you weren't interested.
const ABANDONED: f64 = -0.5;
/// Marked watched without playing it here: likely seen elsewhere.
const MARKED: f64 = 0.5;
/// How fast relevance falls with the position in a related list.
const RANK_SCALE: f64 = 8.0;
/// Each further video from a channel already picked is worth this much less.
const SAME_CHANNEL: f64 = 0.7;
const SUBSCRIBED: f64 = 1.15;

/// One video you played or marked watched.
#[derive(Clone, Debug, PartialEq)]
pub struct Watched {
    pub id: String,
    pub channel_id: String,
    pub channel_title: String,
    /// When you last played it (or marked it watched), unix seconds.
    pub at: i64,
    pub played: bool,
    pub finished: bool,
    pub resume_secs: Option<f64>,
    pub duration_secs: Option<u32>,
}

impl Watched {
    /// How interested this showed you were, −½ to 1.
    pub fn interest(&self) -> f64 {
        if self.finished {
            return if self.played { 1.0 } else { MARKED };
        }
        match (self.resume_secs, self.duration_secs) {
            (Some(at), Some(d)) if d > 0 => (at / f64::from(d)).clamp(0.05, 1.0),
            // A resume point but no length: some interest.
            (Some(_), _) => 0.5,
            // Played, and nothing to resume: left within seconds.
            (None, _) => ABANDONED,
        }
    }
}

/// A related list to rank from: a seed's weight and what YouTube listed.
pub struct Related {
    pub weight: f64,
    pub items: Vec<FeedItem>,
}

/// What your history says about channels and lengths.
#[derive(Clone, Debug, Default)]
pub struct Taste {
    /// −1 (you leave its videos) to 1 (you finish them), by channel key.
    affinity: HashMap<String, f64>,
    subscribed: HashSet<String>,
    /// The longest videos you usually watch (90th percentile, seconds).
    usual_max_secs: Option<f64>,
}

impl Taste {
    /// `history` most recent first; `subscribed`: channel ids.
    pub fn new(history: &[Watched], subscribed: HashSet<String>, now: i64) -> Self {
        let mut sums: HashMap<String, f64> = HashMap::new();
        for w in history {
            let fade = half_life(now - w.at, AFFINITY_HALF_LIFE_DAYS);
            *sums
                .entry(channel_key(&w.channel_id, &w.channel_title))
                .or_default() += fade * w.interest();
        }
        let affinity = sums
            .into_iter()
            .map(|(k, s)| {
                (
                    k,
                    if s >= 0.0 {
                        1.0 - (-s).exp()
                    } else {
                        s.exp() - 1.0
                    },
                )
            })
            .collect();
        // Lengths of what you watched most of.
        let mut lengths: Vec<f64> = history
            .iter()
            .filter(|w| w.interest() >= 0.5)
            .filter_map(|w| w.duration_secs.filter(|&d| d > 0).map(f64::from))
            .collect();
        lengths.sort_by(f64::total_cmp);
        let usual_max_secs = (lengths.len() >= 5).then(|| lengths[(lengths.len() - 1) * 9 / 10]);
        Self {
            affinity,
            subscribed,
            usual_max_secs,
        }
    }

    fn affinity(&self, item: &FeedItem) -> f64 {
        self.affinity
            .get(&channel_key(&item.channel_id, &item.channel_title))
            .copied()
            .unwrap_or(0.0)
    }

    /// Everything about `item` but its relevance.
    fn factor(&self, item: &FeedItem, now: i64) -> f64 {
        let affinity = self.affinity(item);
        let channel = if affinity >= 0.0 {
            1.0 + 1.5 * affinity
        } else {
            1.0 + 0.8 * affinity
        };
        let subscribed = if self.subscribed.contains(&item.channel_id) {
            SUBSCRIBED
        } else {
            1.0
        };
        // Listings say "3 years ago", so this is rough; old favorites still rank.
        let age_days = ((now - item.published.timestamp()) as f64 / DAY).max(0.0);
        let fresh = (1.0 + age_days / 365.0).powf(-0.5);
        let length = match (self.usual_max_secs, item.duration) {
            (Some(usual), Some(d)) if f64::from(d.secs) > 1.5 * usual => 0.6,
            _ => 1.0,
        };
        channel * subscribed * fresh * length
    }
}

/// Up to `n` seeds from `history` (most recent first): videos you watched
/// most of, weighted by how recently and how much. Weights are 0–1.
pub fn seeds(history: &[Watched], n: usize, now: i64) -> Vec<(String, f64)> {
    let mut seeds: Vec<(String, f64)> = history
        .iter()
        .filter(|w| w.played && w.interest() > 0.0)
        .map(|w| {
            (
                w.id.clone(),
                half_life(now - w.at, SEED_HALF_LIFE_DAYS) * w.interest(),
            )
        })
        .collect();
    seeds.sort_by(|a, b| b.1.total_cmp(&a.1));
    seeds.truncate(n);
    let top = seeds.first().map_or(1.0, |s| s.1).max(f64::MIN_POSITIVE);
    for s in &mut seeds {
        s.1 /= top;
    }
    seeds
}

/// The candidates of `lists`, most likely to be watched first, without
/// `skip` and duplicates, at most `max`.
pub fn rank(
    lists: Vec<Related>,
    skip: &HashSet<String>,
    taste: &Taste,
    now: i64,
    max: usize,
) -> Vec<FeedItem> {
    let mut relevance: HashMap<String, f64> = HashMap::new();
    let mut items: Vec<FeedItem> = Vec::new();
    for list in lists {
        for (position, item) in list.items.into_iter().enumerate() {
            if skip.contains(&item.video_id) {
                continue;
            }
            let r = list.weight / (1.0 + position as f64 / RANK_SCALE);
            match relevance.get_mut(&item.video_id) {
                Some(total) => *total += r,
                None => {
                    relevance.insert(item.video_id.clone(), r);
                    items.push(item);
                }
            }
        }
    }
    let mut scored: Vec<(f64, FeedItem)> = items
        .into_iter()
        .map(|item| (relevance[&item.video_id] * taste.factor(&item, now), item))
        .collect();
    // Best first, then variety: a channel's next video is worth a bit less
    // each time, so the order is picked greedily.
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut picked: HashMap<String, i32> = HashMap::new();
    let mut out = Vec::new();
    while out.len() < max && !scored.is_empty() {
        let (best, _) = scored
            .iter()
            .enumerate()
            .map(|(i, (score, item))| {
                let seen = picked.get(&channel_key(&item.channel_id, &item.channel_title));
                (i, score * SAME_CHANNEL.powi(seen.copied().unwrap_or(0)))
            })
            .fold((0, f64::MIN), |best, x| if x.1 > best.1 { x } else { best });
        let (_, item) = scored.remove(best);
        *picked
            .entry(channel_key(&item.channel_id, &item.channel_title))
            .or_default() += 1;
        out.push(item);
    }
    out
}

/// Channels by id, or by name where a listing has no id.
fn channel_key(id: &str, title: &str) -> String {
    if id.is_empty() {
        format!("title:{title}")
    } else {
        id.to_string()
    }
}

fn half_life(age_secs: i64, days: f64) -> f64 {
    0.5f64.powf((age_secs.max(0) as f64 / DAY) / days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shorts::{DurationInfo, LinkHint};
    use chrono::{TimeZone, Utc};

    const NOW: i64 = 1_800_000_000;

    fn item(id: &str, channel: &str) -> FeedItem {
        FeedItem {
            video_id: id.into(),
            channel_id: channel.into(),
            channel_title: format!("{channel} title"),
            title: id.into(),
            published: Utc.timestamp_opt(NOW - 7 * 86_400, 0).unwrap(),
            link_hint: LinkHint::WatchLink,
            duration: Some(DurationInfo {
                secs: 600,
                live_or_upcoming: false,
            }),
        }
    }

    fn watched(
        id: &str,
        channel: &str,
        days_ago: f64,
        played: bool,
        finished: bool,
        resume: Option<f64>,
    ) -> Watched {
        Watched {
            id: id.into(),
            channel_id: channel.into(),
            channel_title: format!("{channel} title"),
            at: NOW - (days_ago * 86_400.0) as i64,
            played,
            finished,
            resume_secs: resume,
            duration_secs: Some(600),
        }
    }

    fn ids(items: &[FeedItem]) -> Vec<&str> {
        items.iter().map(|i| i.video_id.as_str()).collect()
    }

    fn one_list(items: Vec<FeedItem>) -> Vec<Related> {
        vec![Related { weight: 1.0, items }]
    }

    #[test]
    fn interest_from_how_much_was_watched() {
        assert_eq!(watched("a", "c", 0.0, true, true, None).interest(), 1.0);
        assert_eq!(watched("a", "c", 0.0, false, true, None).interest(), MARKED);
        assert_eq!(
            watched("a", "c", 0.0, true, false, Some(300.0)).interest(),
            0.5
        );
        assert_eq!(
            watched("a", "c", 0.0, true, false, None).interest(),
            ABANDONED
        );
    }

    #[test]
    fn seeds_are_recent_videos_you_watched_most_of() {
        let history = [
            watched("left", "c", 0.1, true, false, None),
            watched("half", "c", 0.5, true, false, Some(300.0)),
            watched("done", "c", 1.0, true, true, None),
            watched("old", "c", 60.0, true, true, None),
            watched("marked", "c", 0.2, false, true, None),
        ];
        let seeds = seeds(&history, 2, NOW);
        let names: Vec<&str> = seeds.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(names, ["done", "half"], "not abandoned, not merely marked");
        assert_eq!(seeds[0].1, 1.0);
        assert!(seeds[1].1 < 0.6);
    }

    #[test]
    fn channels_you_finish_rise_and_channels_you_leave_sink() {
        let history = [
            watched("w1", "fav", 2.0, true, true, None),
            watched("w2", "fav", 5.0, true, true, None),
            watched("w3", "meh", 3.0, true, false, None),
        ];
        let taste = Taste::new(&history, HashSet::new(), NOW);
        // YouTube's order: meh, new, fav.
        let list = one_list(vec![item("m", "meh"), item("n", "new"), item("f", "fav")]);
        assert_eq!(
            ids(&rank(list, &HashSet::new(), &taste, NOW, 10)),
            ["f", "n", "m"]
        );
    }

    #[test]
    fn listed_beside_several_seeds_beats_listed_once() {
        let taste = Taste::default();
        let lists = vec![
            Related {
                weight: 1.0,
                items: vec![item("a", "c1"), item("both", "c2")],
            },
            Related {
                weight: 0.8,
                items: vec![item("b", "c3"), item("both", "c2")],
            },
        ];
        assert_eq!(
            ids(&rank(lists, &HashSet::new(), &taste, NOW, 10))[0],
            "both"
        );
    }

    #[test]
    fn variety_keeps_one_channel_from_filling_the_top() {
        let history = [watched("w", "fav", 1.0, true, true, None)];
        let taste = Taste::new(&history, HashSet::new(), NOW);
        let mut items: Vec<FeedItem> = (0..5).map(|i| item(&format!("fav{i}"), "fav")).collect();
        items.extend((0..5).map(|i| item(&format!("other{i}"), &format!("o{i}"))));
        let ranked = rank(one_list(items), &HashSet::new(), &taste, NOW, 10);
        let top5 = &ids(&ranked)[..5];
        assert!(top5[0].starts_with("fav"));
        assert!(
            top5.iter().filter(|id| id.starts_with("fav")).count() < 5,
            "{top5:?}"
        );
        assert_eq!(ranked.len(), 10);
    }

    #[test]
    fn skips_duplicates_and_respects_max() {
        let skip: HashSet<String> = ["seen".to_string()].into();
        let lists = vec![
            Related {
                weight: 1.0,
                items: vec![item("seen", "c"), item("a", "c1"), item("b", "c2")],
            },
            Related {
                weight: 1.0,
                items: vec![item("a", "c1"), item("c", "c3")],
            },
        ];
        let ranked = rank(lists, &skip, &Taste::default(), NOW, 2);
        assert_eq!(ids(&ranked), ["a", "c"]);
    }

    #[test]
    fn much_longer_than_usual_and_old_videos_count_for_less() {
        let history: Vec<Watched> = (0..6)
            .map(|i| watched(&format!("w{i}"), &format!("h{i}"), 1.0, true, true, None))
            .collect();
        let taste = Taste::new(&history, HashSet::new(), NOW);
        let mut long = item("long", "c1");
        long.duration = Some(DurationInfo {
            secs: 4 * 3600,
            live_or_upcoming: false,
        });
        let mut old = item("old", "c2");
        old.published = Utc.timestamp_opt(NOW - 5 * 365 * 86_400, 0).unwrap();
        let list = one_list(vec![long, old, item("fit", "c3")]);
        assert_eq!(ids(&rank(list, &HashSet::new(), &taste, NOW, 10))[0], "fit");
    }

    #[test]
    fn subscribed_channels_get_a_nudge() {
        let taste = Taste::new(&[], ["sub".to_string()].into(), NOW);
        let list = one_list(vec![item("x", "other"), item("s", "sub")]);
        // Close in YouTube's order (positions 0 and 1); the nudge decides.
        assert_eq!(ids(&rank(list, &HashSet::new(), &taste, NOW, 10))[0], "s");
    }
}
