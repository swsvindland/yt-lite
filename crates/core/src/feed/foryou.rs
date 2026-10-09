//! "For you": recommendations without signing in to YouTube.
//!
//! Seeds are the videos you most recently played in yt-lite (falling back to
//! the newest subscription uploads). For each seed we fetch YouTube's related
//! videos, interleave the lists round-robin so no single seed dominates, and
//! drop duplicates and anything already played. Related listings include the
//! duration, so Shorts filtering rarely needs the Data API.

use std::collections::HashSet;

use anyhow::Result;

use super::pipeline::Services;
use super::{FeedItem, FeedKind, FeedSource, Fetched, Progress, Retain};
use crate::net::parallel_map;
use crate::youtube::related;

/// How many played videos seed the feed.
const SEEDS: u32 = 10;
/// Fallback seeds from subscriptions when little has been played yet.
const MIN_SEEDS: usize = 4;
const MAX_ITEMS: usize = 150;

pub struct ForYouSource;

impl FeedSource for ForYouSource {
    fn kind(&self) -> FeedKind {
        FeedKind::Home
    }

    fn display_name(&self) -> &'static str {
        "For you"
    }

    fn fetch(&self, s: &Services, progress: Progress) -> Result<Fetched> {
        let mut seeds = s.db.recently_played(SEEDS)?;
        if seeds.len() < MIN_SEEDS {
            for id in s.db.newest_in_feed(FeedKind::Subscriptions, SEEDS)? {
                if seeds.len() >= MIN_SEEDS {
                    break;
                }
                if !seeds.contains(&id) {
                    seeds.push(id);
                }
            }
        }
        if seeds.is_empty() {
            return Ok(Fetched {
                items: Vec::new(),
                retain: Retain::OnlyFetched,
            });
        }

        progress(format!(
            "Finding videos related to {} you watched…",
            seeds.len()
        ));
        let lists: Vec<Vec<FeedItem>> =
            parallel_map(&seeds, 4, |id| match related::fetch(&s.http, id) {
                Ok(items) => items,
                Err(e) => {
                    log::warn!("related videos for {id}: {e:#}");
                    Vec::new()
                }
            });
        if lists.iter().all(Vec::is_empty) {
            anyhow::bail!("couldn't load related videos from YouTube");
        }

        let mut skip: HashSet<String> = s.db.played_ids()?;
        skip.extend(seeds.iter().cloned());
        let items = interleave(lists, &skip, MAX_ITEMS);
        log::info!(
            "for you: {} seeds -> {} candidates",
            seeds.len(),
            items.len()
        );
        Ok(Fetched {
            items,
            retain: Retain::OnlyFetched,
        })
    }
}

/// Round-robin merge: first item of each list, then the second of each, ...
/// skipping ids in `skip` and duplicates.
pub fn interleave(lists: Vec<Vec<FeedItem>>, skip: &HashSet<String>, max: usize) -> Vec<FeedItem> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut iters: Vec<_> = lists.into_iter().map(Vec::into_iter).collect();
    let mut out = Vec::new();
    loop {
        let mut progressed = false;
        for it in iters.iter_mut() {
            if let Some(item) = it.next() {
                progressed = true;
                if !skip.contains(&item.video_id) && seen.insert(item.video_id.clone()) {
                    out.push(item);
                    if out.len() >= max {
                        return out;
                    }
                }
            }
        }
        if !progressed {
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shorts::LinkHint;
    use chrono::Utc;

    fn item(id: &str) -> FeedItem {
        FeedItem {
            video_id: id.into(),
            channel_id: String::new(),
            channel_title: String::new(),
            title: id.into(),
            published: Utc::now(),
            link_hint: LinkHint::WatchLink,
            duration: None,
        }
    }

    fn ids(items: &[FeedItem]) -> Vec<&str> {
        items.iter().map(|i| i.video_id.as_str()).collect()
    }

    #[test]
    fn interleaves_dedupes_and_skips() {
        let lists = vec![
            vec![item("a1"), item("a2"), item("shared"), item("a3")],
            vec![item("b1"), item("shared"), item("watched")],
            vec![item("c1")],
        ];
        let skip: HashSet<String> = ["watched".to_string()].into();
        let out = interleave(lists, &skip, 100);
        assert_eq!(ids(&out), vec!["a1", "b1", "c1", "a2", "shared", "a3"]);
    }

    #[test]
    fn respects_max() {
        let lists = vec![vec![item("a"), item("b")], vec![item("c"), item("d")]];
        let out = interleave(lists, &HashSet::new(), 3);
        assert_eq!(ids(&out), vec!["a", "c", "b"]);
    }
}
