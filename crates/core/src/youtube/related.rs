//! Related videos for a video, from the InnerTube `next` endpoint (what
//! youtube.com shows beside a video). Works logged out.

use anyhow::Result;
use chrono::Utc;
use serde_json::json;

use super::listing;
use crate::feed::FeedItem;
use crate::net::Http;

/// Blocking.
pub fn fetch(http: &Http, video_id: &str) -> Result<Vec<FeedItem>> {
    let resp = listing::web_request(http, "next", json!({ "videoId": video_id }))?;
    Ok(listing::parse(&resp, Utc::now()))
}
