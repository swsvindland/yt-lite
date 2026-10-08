//! Subscriptions feed: channel list from the Data API, uploads from each
//! channel's free RSS feed, falling back to the uploads playlist (1 quota unit)
//! only when RSS fails.

use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;

use super::pipeline::Services;
use super::{FeedKind, FeedSource, Fetched, Progress, Retain};
use crate::auth::NotSignedIn;
use crate::net::parallel_map;
use crate::youtube::api::{Api, Subscription};
use crate::youtube::rss;

pub struct SubscriptionsSource;

impl SubscriptionsSource {
    /// Current subscriptions from the API; on failure (offline, quota) the
    /// last cached list. Not signed in = only `extra_channels`.
    fn channels(&self, s: &Services) -> Result<(Vec<Subscription>, bool)> {
        let api = Api {
            http: &s.http,
            auth: &s.auth,
        };
        let (mut channels, signed_in) = match api.subscriptions() {
            Ok(subs) => {
                s.db.set_subscriptions(&subs)?;
                (subs, true)
            }
            Err(e) if e.downcast_ref::<NotSignedIn>().is_some() => (Vec::new(), false),
            Err(e) => {
                log::warn!("subscriptions.list failed, using cached list: {e:#}");
                (s.db.subscribed_channels()?, true)
            }
        };
        for id in &s.config.feed.extra_channels {
            let id = id.trim();
            if !id.is_empty() && !channels.iter().any(|c| c.channel_id == id) {
                channels.push(Subscription {
                    channel_id: id.to_string(),
                    title: String::new(),
                });
            }
        }
        Ok((channels, signed_in))
    }
}

impl FeedSource for SubscriptionsSource {
    fn kind(&self) -> FeedKind {
        FeedKind::Subscriptions
    }

    fn display_name(&self) -> &'static str {
        "Subscriptions"
    }

    fn fetch(&self, s: &Services, progress: Progress) -> Result<Fetched> {
        progress("Loading subscriptions…".into());
        let (channels, signed_in) = self.channels(s)?;
        if channels.is_empty() {
            if !signed_in {
                return Err(NotSignedIn.into());
            }
            return Ok(Fetched {
                items: Vec::new(),
                retain: Retain::Channels(Vec::new()),
            });
        }
        let total = channels.len();
        let done = AtomicUsize::new(0);
        let fallbacks = AtomicUsize::new(0);
        let api = Api {
            http: &s.http,
            auth: &s.auth,
        };

        let results = parallel_map(&channels, s.config.feed.rss_concurrency, |ch| {
            let r = s
                .http
                .get_youtube_text(&rss::feed_url(&ch.channel_id))
                .and_then(|xml| rss::parse(&xml, &ch.channel_id))
                .map(|feed| feed.items)
                .or_else(|rss_err| {
                    if !signed_in {
                        return Err(rss_err);
                    }
                    log::warn!(
                        "RSS failed for {} ({rss_err:#}); using uploads playlist",
                        ch.channel_id
                    );
                    fallbacks.fetch_add(1, Ordering::Relaxed);
                    api.recent_uploads(&ch.channel_id, &ch.title)
                });
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(10) || n == total {
                progress(format!("Fetching channel feeds {n}/{total}"));
            }
            r.map_err(|e| (ch.channel_id.clone(), e))
        });

        let mut items = Vec::new();
        let mut failed = 0;
        for r in results {
            match r {
                Ok(mut v) => items.append(&mut v),
                Err((ch, e)) => {
                    failed += 1;
                    log::warn!("channel {ch}: {e:#}");
                }
            }
        }
        log::info!(
            "subscriptions: {total} channels, {} videos, {} playlist fallbacks, {failed} failed",
            items.len(),
            fallbacks.load(Ordering::Relaxed)
        );
        let channel_ids = channels.into_iter().map(|c| c.channel_id).collect();
        Ok(Fetched {
            items,
            retain: Retain::Channels(channel_ids),
        })
    }
}
