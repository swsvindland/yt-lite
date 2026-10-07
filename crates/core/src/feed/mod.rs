//! Feed sources and the shared pipeline that every source's output goes through.
//!
//! A [`FeedSource`] only *discovers* videos. Enrichment (durations) and the
//! Shorts filter are applied centrally in [`pipeline`], so no source — including
//! the Phase 2 InnerTube "home" source — can put a Short in front of the UI.

pub mod pipeline;
pub mod subscriptions;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::shorts::LinkHint;

/// A video discovered by a source, before enrichment and filtering.
#[derive(Clone, Debug, PartialEq)]
pub struct FeedItem {
    pub video_id: String,
    pub channel_id: String,
    pub channel_title: String,
    pub title: String,
    pub published: DateTime<Utc>,
    pub link_hint: LinkHint,
}

/// Stable identifiers for sources; also used as the `source` column in SQLite
/// so each feed can be listed independently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FeedKind {
    Subscriptions,
    /// Phase 2: InnerTube recommendations. Not implemented yet.
    #[allow(dead_code)]
    Home,
}

impl FeedKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FeedKind::Subscriptions => "subscriptions",
            FeedKind::Home => "home",
        }
    }
}

/// The result of one fetch.
pub struct Fetched {
    pub items: Vec<FeedItem>,
    pub retain: Retain,
}

/// Which previously-cached items stay in the feed after a fetch.
#[derive(Clone, Debug)]
pub enum Retain {
    /// Keep cached items from these channels (subscriptions: history stays,
    /// unsubscribed channels drop out).
    Channels(Vec<String>),
    /// The feed is exactly what was just fetched (e.g. a recommendations page).
    #[allow(dead_code)]
    OnlyFetched,
}

/// Progress callback for long refreshes, e.g. "RSS 40/180".
pub type Progress<'a> = &'a (dyn Fn(String) + Sync);

/// A source of videos. Implementations are blocking and run on a background
/// thread; they may use the HTTP client, the database, and auth tokens from
/// [`pipeline::Services`].
///
/// To add the InnerTube home feed in Phase 2, implement this trait in a new
/// module and register it in the UI's source list. If InnerTube breaks, disable
/// that one source; nothing else changes.
pub trait FeedSource: Send + Sync {
    fn kind(&self) -> FeedKind;
    fn display_name(&self) -> &'static str;
    fn fetch(&self, services: &pipeline::Services, progress: Progress) -> Result<Fetched>;
}
