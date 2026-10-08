//! Related videos for a video, from the InnerTube `next` endpoint (what
//! youtube.com shows beside a video). Works logged out.
//!
//! Items are `lockupViewModel`s (2025+ format). Shorts appear as
//! `shortsLockupViewModel` and are skipped here; the pipeline's Shorts filter
//! still checks everything else.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use crate::feed::FeedItem;
use crate::net::Http;
use crate::shorts::{DurationInfo, LinkHint};

use super::duration::parse_clock;

const NEXT_URL: &str = "https://www.youtube.com/youtubei/v1/next?prettyPrint=false";
const WEB_CLIENT_VERSION: &str = "2.20261001.00.00";

/// Blocking.
pub fn fetch(http: &Http, video_id: &str) -> Result<Vec<FeedItem>> {
    let body = json!({
        "context": { "client": {
            "clientName": "WEB",
            "clientVersion": WEB_CLIENT_VERSION,
            "hl": "en",
            "gl": "US",
        }},
        "videoId": video_id,
    });
    let headers = [
        ("X-YouTube-Client-Name", "1"),
        ("X-YouTube-Client-Version", WEB_CLIENT_VERSION),
        ("Origin", "https://www.youtube.com"),
        ("Cookie", "SOCS=CAI"),
    ];
    let resp: Value = http.post_json(NEXT_URL, &headers, &body)?;
    Ok(parse(&resp, Utc::now()))
}

/// Extracts every video lockup in document order.
pub fn parse(resp: &Value, now: DateTime<Utc>) -> Vec<FeedItem> {
    let mut out = Vec::new();
    collect(resp, now, &mut out);
    out
}

fn collect(v: &Value, now: DateTime<Utc>, out: &mut Vec<FeedItem>) {
    match v {
        Value::Object(map) => {
            if let Some(lockup) = map.get("lockupViewModel") {
                if let Some(item) = parse_lockup(lockup, now) {
                    out.push(item);
                }
                return;
            }
            if map.contains_key("shortsLockupViewModel") {
                return;
            }
            for child in map.values() {
                collect(child, now, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect(child, now, out);
            }
        }
        _ => {}
    }
}

fn parse_lockup(l: &Value, now: DateTime<Utc>) -> Option<FeedItem> {
    if l["contentType"].as_str()? != "LOCKUP_CONTENT_TYPE_VIDEO" {
        return None;
    }
    let video_id = l["contentId"].as_str()?.to_string();
    let md = &l["metadata"]["lockupMetadataViewModel"];
    let title = md["title"]["content"].as_str()?.to_string();
    let rows = md["metadata"]["contentMetadataViewModel"]["metadataRows"].as_array();
    let parts = |row: usize| -> Vec<&str> {
        rows.and_then(|r| r.get(row))
            .and_then(|r| r["metadataParts"].as_array())
            .map(|ps| {
                ps.iter()
                    .filter_map(|p| p["text"]["content"].as_str())
                    .collect()
            })
            .unwrap_or_default()
    };
    let channel_title = parts(0).first().copied().unwrap_or_default().to_string();
    let published = parts(1)
        .iter()
        .find_map(|p| parse_age(p))
        .map(|age| now - age)
        .unwrap_or(now);
    let channel_id = find_channel_id(&md["image"]).unwrap_or_default();

    let badge = l["contentImage"]["thumbnailViewModel"]["overlays"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|o| {
            o["thumbnailBottomOverlayViewModel"]["badges"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .find_map(|b| b["thumbnailBadgeViewModel"]["text"].as_str());
    let duration = match badge {
        Some(text) => match parse_clock(text) {
            Some(secs) => Some(DurationInfo {
                secs,
                live_or_upcoming: false,
            }),
            None if is_live_badge(text) => Some(DurationInfo {
                secs: 0,
                live_or_upcoming: true,
            }),
            None => None,
        },
        None => None,
    };
    let url = l["rendererContext"]["commandContext"]["onTap"]["innertubeCommand"]["commandMetadata"]
        ["webCommandMetadata"]["url"]
        .as_str()
        .unwrap_or_default();

    Some(FeedItem {
        video_id,
        channel_id,
        channel_title,
        title,
        published,
        link_hint: LinkHint::from_url(url),
        duration,
    })
}

fn is_live_badge(text: &str) -> bool {
    let t = text.to_ascii_uppercase();
    t.contains("LIVE") || t.contains("UPCOMING") || t.contains("PREMIERE")
}

fn find_channel_id(v: &Value) -> Option<String> {
    match v {
        Value::Object(map) => {
            if let Some(id) = map.get("browseId").and_then(Value::as_str)
                && id.starts_with("UC")
            {
                return Some(id.to_string());
            }
            map.values().find_map(find_channel_id)
        }
        Value::Array(items) => items.iter().find_map(find_channel_id),
        _ => None,
    }
}

/// "8y ago", "16h ago", "2mo ago", "3 weeks ago", "Streamed 5 days ago".
pub fn parse_age(text: &str) -> Option<Duration> {
    let text = text.trim().to_ascii_lowercase();
    let words: Vec<&str> = text.strip_suffix("ago")?.split_whitespace().collect();
    // Compact "8y" is the last word; long form "8 years" is the last two.
    let rest = match words.as_slice() {
        [.., n, unit] if n.chars().all(|c| c.is_ascii_digit()) => format!("{n}{unit}"),
        [.., last] => last.to_string(),
        [] => return None,
    };
    let split = rest.find(|c: char| !c.is_ascii_digit())?;
    let (num, unit) = rest.split_at(split);
    let n: i64 = num.parse().ok()?;
    let unit = unit.trim_end_matches('s');
    let secs = match unit {
        "second" | "sec" => 1,
        "m" | "min" | "minute" => 60,
        "h" | "hr" | "hour" => 3600,
        "d" | "day" => 86_400,
        "w" | "wk" | "week" => 7 * 86_400,
        "mo" | "month" => 30 * 86_400,
        "y" | "yr" | "year" => 365 * 86_400,
        _ => return None,
    };
    Some(Duration::seconds(n * secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/next_related.json");

    #[test]
    fn parses_related_lockups() {
        let now = Utc::now();
        let v: Value = serde_json::from_str(FIXTURE).unwrap();
        let items = parse(&v, now);
        // 17 videos; courses, playlists and the Shorts entry are skipped.
        assert_eq!(items.len(), 17);
        assert!(!items.iter().any(|i| i.video_id == "abcdefghijk"));
        let first = &items[0];
        assert_eq!(first.video_id, "IHZwWFHWa-w");
        assert_eq!(first.channel_title, "3Blue1Brown");
        assert_eq!(first.channel_id, "UCYO_jab_esuFRV4b17AJtAw");
        assert!(first.title.starts_with("Gradient descent"));
        assert_eq!(first.duration.unwrap().secs, 20 * 60 + 33);
        assert_eq!(first.link_hint, LinkHint::WatchLink);
        // "8y ago"
        let age = now - first.published;
        assert!(age.num_days() > 365 * 7 && age.num_days() < 365 * 9);
    }

    #[test]
    fn ages() {
        assert_eq!(parse_age("8y ago"), Some(Duration::days(8 * 365)));
        assert_eq!(parse_age("16h ago"), Some(Duration::hours(16)));
        assert_eq!(parse_age("2mo ago"), Some(Duration::days(60)));
        assert_eq!(parse_age("5m ago"), Some(Duration::minutes(5)));
        assert_eq!(parse_age("3 weeks ago"), Some(Duration::weeks(3)));
        assert_eq!(parse_age("Streamed 5 days ago"), Some(Duration::days(5)));
        assert_eq!(parse_age("1 year ago"), Some(Duration::days(365)));
        assert_eq!(parse_age("9.6M"), None);
        assert_eq!(parse_age("ago"), None);
    }
}
