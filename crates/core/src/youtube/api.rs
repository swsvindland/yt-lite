//! YouTube Data API v3. Every call here costs 1 quota unit (10,000/day default):
//! `subscriptions.list` (50/page), `videos.list` (50 ids/call), `playlistItems.list`.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::auth::Auth;
use crate::feed::FeedItem;
use crate::net::{ApiError, Http};
use crate::shorts::{DurationInfo, LinkHint};

use super::duration::parse_iso8601;

const BASE: &str = "https://www.googleapis.com/youtube/v3";

pub struct Api<'a> {
    pub http: &'a Http,
    pub auth: &'a Auth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub channel_id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoDetails {
    pub id: String,
    pub duration: DurationInfo,
}

impl Api<'_> {
    /// GET with one retry after a 401 (stale access token).
    fn get<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let token = self.auth.access_token()?;
        match self.http.get_json(url, &token) {
            Err(e) if ApiError::is_unauthorized(&e) => {
                self.auth.invalidate();
                let token = self.auth.access_token()?;
                self.http.get_json(url, &token)
            }
            r => r,
        }
    }

    pub fn subscriptions(&self) -> Result<Vec<Subscription>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            next_page_token: Option<String>,
            #[serde(default)]
            items: Vec<Item>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Item {
            snippet: Snippet,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Snippet {
            title: String,
            resource_id: ResourceId,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ResourceId {
            channel_id: String,
        }

        let mut out = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut url = format!(
                "{BASE}/subscriptions?part=snippet&mine=true&maxResults=50&order=alphabetical\
                 &fields=nextPageToken,items(snippet(title,resourceId/channelId))"
            );
            if let Some(t) = &page_token {
                url.push_str("&pageToken=");
                url.push_str(t);
            }
            let page: Page = self.get(&url)?;
            out.extend(page.items.into_iter().map(|i| Subscription {
                channel_id: i.snippet.resource_id.channel_id,
                title: i.snippet.title,
            }));
            match page.next_page_token {
                Some(t) if !t.is_empty() => page_token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Durations for up to 50 ids. Ids missing from the response (private,
    /// deleted) are simply absent from the result.
    pub fn video_details(&self, ids: &[&str]) -> Result<Vec<VideoDetails>> {
        assert!(ids.len() <= 50, "videos.list takes at most 50 ids");
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let url = format!(
            "{BASE}/videos?part=contentDetails,snippet&id={}\
             &fields=items(id,contentDetails/duration,snippet/liveBroadcastContent)",
            ids.join(",")
        );
        let resp: VideosResponse = self.get(&url)?;
        Ok(parse_videos(resp))
    }

    /// Fallback when a channel's RSS feed fails: its uploads playlist.
    pub fn recent_uploads(&self, channel_id: &str, channel_title: &str) -> Result<Vec<FeedItem>> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            items: Vec<Item>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Item {
            snippet: Snippet,
            content_details: Details,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Snippet {
            title: String,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Details {
            video_id: String,
            video_published_at: Option<DateTime<Utc>>,
        }

        let Some(playlist) = super::uploads_playlist_id(channel_id) else {
            return Ok(Vec::new());
        };
        let url = format!(
            "{BASE}/playlistItems?part=snippet,contentDetails&playlistId={playlist}&maxResults=15\
             &fields=items(snippet/title,contentDetails(videoId,videoPublishedAt))"
        );
        let page: Page = self.get(&url)?;
        Ok(page
            .items
            .into_iter()
            .filter_map(|i| {
                Some(FeedItem {
                    video_id: i.content_details.video_id,
                    channel_id: channel_id.to_string(),
                    channel_title: channel_title.to_string(),
                    title: i.snippet.title,
                    // Private/deleted entries have no publish time.
                    published: i.content_details.video_published_at?,
                    link_hint: LinkHint::None,
                })
            })
            .collect())
    }
}

#[derive(Deserialize)]
struct VideosResponse {
    #[serde(default)]
    items: Vec<VideoItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoItem {
    id: String,
    content_details: Option<ContentDetails>,
    snippet: Option<VideoSnippet>,
}

#[derive(Deserialize)]
struct ContentDetails {
    duration: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoSnippet {
    live_broadcast_content: Option<String>,
}

fn parse_videos(resp: VideosResponse) -> Vec<VideoDetails> {
    resp.items
        .into_iter()
        .filter_map(|v| {
            let live = v
                .snippet
                .and_then(|s| s.live_broadcast_content)
                .is_some_and(|l| l == "live" || l == "upcoming");
            let secs = v
                .content_details
                .and_then(|c| c.duration)
                .and_then(|d| parse_iso8601(&d));
            // Upcoming premieres may omit duration entirely.
            let secs = match (secs, live) {
                (Some(s), _) => s,
                (None, true) => 0,
                (None, false) => return None,
            };
            Some(VideoDetails {
                id: v.id,
                duration: DurationInfo {
                    secs,
                    live_or_upcoming: live,
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_videos_list_response() {
        let json = r#"{"items":[
            {"id":"a","contentDetails":{"duration":"PT12M3S"},"snippet":{"liveBroadcastContent":"none"}},
            {"id":"b","contentDetails":{"duration":"PT45S"},"snippet":{"liveBroadcastContent":"none"}},
            {"id":"c","contentDetails":{"duration":"P0D"},"snippet":{"liveBroadcastContent":"upcoming"}},
            {"id":"d","snippet":{"liveBroadcastContent":"live"}},
            {"id":"e","snippet":{"liveBroadcastContent":"none"}}
        ]}"#;
        let v = parse_videos(serde_json::from_str(json).unwrap());
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].duration.secs, 723);
        assert!(!v[0].duration.live_or_upcoming);
        assert_eq!(v[1].duration.secs, 45);
        assert!(v[2].duration.live_or_upcoming);
        assert_eq!(v[3].id, "d");
        assert!(v[3].duration.live_or_upcoming);
    }

    #[test]
    fn empty_items_is_ok() {
        assert!(parse_videos(serde_json::from_str("{}").unwrap()).is_empty());
    }
}
