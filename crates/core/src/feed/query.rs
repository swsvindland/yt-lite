//! Search-backed feeds: Search (your query) and Explore (popular this week by
//! topic). Both work without signing in. Results go through the shared
//! pipeline, so Shorts are filtered like everywhere else.

use std::sync::Mutex;

use anyhow::Result;

use super::pipeline::Services;
use super::{FeedKind, FeedSource, Fetched, Progress, Retain};
use crate::youtube::search::{SearchFilter, search};

/// Explore topics: (label, query).
pub const EXPLORE_TOPICS: &[(&str, &str)] = &[
    ("Popular", "trending"),
    ("Music", "music video"),
    ("Gaming", "gaming"),
    ("Tech", "tech review"),
    ("Science", "science explained"),
    ("News", "news"),
    ("Sports", "sports highlights"),
    ("Comedy", "comedy"),
    ("Cooking", "cooking recipe"),
    ("Cars", "cars"),
];

pub struct QuerySource {
    kind: FeedKind,
    name: &'static str,
    filter: SearchFilter,
    query: Mutex<Option<String>>,
}

impl QuerySource {
    pub fn search() -> Self {
        Self {
            kind: FeedKind::Search,
            name: "Search",
            filter: SearchFilter::Videos,
            query: Mutex::new(None),
        }
    }

    pub fn explore() -> Self {
        Self {
            kind: FeedKind::Explore,
            name: "Explore",
            filter: SearchFilter::PopularThisWeek,
            query: Mutex::new(Some(EXPLORE_TOPICS[0].1.to_string())),
        }
    }

    pub fn set_query(&self, query: &str) {
        *self.query.lock().unwrap() = Some(query.trim().to_string()).filter(|q| !q.is_empty());
    }

    pub fn query(&self) -> Option<String> {
        self.query.lock().unwrap().clone()
    }
}

impl FeedSource for QuerySource {
    fn kind(&self) -> FeedKind {
        self.kind
    }

    fn display_name(&self) -> &'static str {
        self.name
    }

    fn fetch(&self, s: &Services, progress: Progress) -> Result<Fetched> {
        let Some(query) = self.query() else {
            return Ok(Fetched {
                items: Vec::new(),
                retain: Retain::OnlyFetched,
            });
        };
        progress(format!("Searching for “{query}”…"));
        let items = search(&s.http, &query, self.filter)?;
        Ok(Fetched {
            items,
            retain: Retain::OnlyFetched,
        })
    }
}
