//! YouTube search (InnerTube `search`, logged out).

use anyhow::Result;
use chrono::Utc;
use serde_json::json;

use super::listing;
use crate::feed::FeedItem;
use crate::net::Http;

/// Search filters, as the protobuf-in-base64 `params` youtube.com sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchFilter {
    /// Videos only, by relevance.
    Videos,
    /// Videos only, uploaded this week, most viewed first.
    PopularThisWeek,
}

impl SearchFilter {
    fn params(self) -> &'static str {
        match self {
            // {2: {2: 1}}  (type = video)
            SearchFilter::Videos => "EgIQAQ==",
            // {1: 3, 2: {1: 3, 2: 1}}  (sort = view count; upload = week; type = video)
            SearchFilter::PopularThisWeek => "CAMSBAgDEAE=",
        }
    }
}

/// Blocking. First page of results (about 20 videos).
pub fn search(http: &Http, query: &str, filter: SearchFilter) -> Result<Vec<FeedItem>> {
    let resp = listing::web_request(
        http,
        "search",
        json!({ "query": query, "params": filter.params() }),
    )?;
    Ok(listing::parse(&resp, Utc::now()))
}
