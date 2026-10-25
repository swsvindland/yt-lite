//! "For you": recommendations without signing in to YouTube.
//!
//! Seeds are videos you recently watched most of in yt-lite (falling back to
//! the newest subscription uploads). For each seed we fetch YouTube's related
//! videos, then order them by how likely you are to watch them
//! ([`interest`]: relevance to your seeds, your history with the channel,
//! variety), leaving out anything already played. Related listings include
//! the duration, so Shorts filtering rarely needs the Data API. The order is
//! also what smart downloads fetch For you in.

use std::collections::HashSet;

use anyhow::Result;
use chrono::Utc;

use super::interest::{self, Related, Taste};
use super::pipeline::Services;
use super::{FeedKind, FeedSource, Fetched, Progress, Retain};
use crate::net::parallel_map;
use crate::youtube::related;

/// How many played videos seed the feed.
const SEEDS: usize = 10;
/// Fallback seeds from subscriptions when little has been played yet.
const MIN_SEEDS: usize = 4;
/// Their weight next to a seed you watched (1 for the best).
const FALLBACK_WEIGHT: f64 = 0.3;
/// How much history your taste is learned from.
const HISTORY: u32 = 500;
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
        let now = Utc::now().timestamp();
        let history = s.db.watch_history(HISTORY)?;
        let mut seeds = interest::seeds(&history, SEEDS, now);
        if seeds.len() < MIN_SEEDS {
            for id in s.db.newest_in_feed(FeedKind::Subscriptions, SEEDS as u32)? {
                if seeds.len() >= MIN_SEEDS {
                    break;
                }
                if !seeds.iter().any(|(seed, _)| *seed == id) {
                    seeds.push((id, FALLBACK_WEIGHT));
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
        let lists: Vec<Related> = parallel_map(&seeds, 4, |(id, weight)| Related {
            weight: *weight,
            items: related::fetch(&s.http, id).unwrap_or_else(|e| {
                log::warn!("related videos for {id}: {e:#}");
                Vec::new()
            }),
        });
        if lists.iter().all(|l| l.items.is_empty()) {
            anyhow::bail!("couldn't load related videos from YouTube");
        }

        let mut skip: HashSet<String> = s.db.played_ids()?;
        skip.extend(seeds.iter().map(|(id, _)| id.clone()));
        let subscribed =
            s.db.subscribed_channels()?
                .into_iter()
                .map(|c| c.channel_id)
                .collect();
        let taste = Taste::new(&history, subscribed, now);
        let items = interest::rank(lists, &skip, &taste, now, MAX_ITEMS);
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
