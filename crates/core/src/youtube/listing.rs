//! Parsing of InnerTube video listings (related videos, search results,
//! browse pages) and the logged-out WEB client used to request them.
//!
//! Two item formats are in use: `lockupViewModel` (2025+, related videos) and
//! `videoRenderer` (search results). Shorts (`shortsLockupViewModel`,
//! `reelShelfRenderer`, `reelItemRenderer`) are skipped; the pipeline's Shorts
//! filter still checks everything else.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use crate::feed::FeedItem;
use crate::net::Http;
use crate::shorts::{DurationInfo, LinkHint};

use super::duration::parse_clock;

const WEB_CLIENT_VERSION: &str = "2.20261001.00.00";

/// POSTs to `youtubei/v1/<endpoint>` as the logged-out WEB client; `fields`
/// are merged into the body next to `context`.
pub fn web_request(http: &Http, endpoint: &str, fields: Value) -> Result<Value> {
    let mut body = serde_json::json!({
        "context": { "client": {
            "clientName": "WEB",
            "clientVersion": WEB_CLIENT_VERSION,
            "hl": "en",
            "gl": "US",
        }},
    });
    if let (Some(b), Value::Object(extra)) = (body.as_object_mut(), fields) {
        b.extend(extra);
    }
    let headers = [
        ("X-YouTube-Client-Name", "1"),
        ("X-YouTube-Client-Version", WEB_CLIENT_VERSION),
        ("Origin", "https://www.youtube.com"),
        ("Cookie", "SOCS=CAI"),
    ];
    http.post_json(
        &format!("https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false"),
        &headers,
        &body,
    )
}

/// Extracts every video in document order.
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
            if let Some(renderer) = map.get("videoRenderer") {
                if let Some(item) = parse_video_renderer(renderer, now) {
                    out.push(item);
                }
                return;
            }
            if [
                "shortsLockupViewModel",
                "reelShelfRenderer",
                "reelItemRenderer",
            ]
            .iter()
            .any(|k| map.contains_key(*k))
            {
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

/// `videoRenderer`: search results.
fn parse_video_renderer(v: &Value, now: DateTime<Utc>) -> Option<FeedItem> {
    let video_id = v["videoId"].as_str()?.to_string();
    let title = text(&v["title"])?;
    let owner = &v["ownerText"]["runs"][0];
    let channel_title = owner["text"].as_str().unwrap_or_default().to_string();
    let channel_id = owner["navigationEndpoint"]["browseEndpoint"]["browseId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let published = text(&v["publishedTimeText"])
        .and_then(|t| parse_age(&t))
        .map(|age| now - age)
        .unwrap_or(now);
    let live =
        v["badges"].as_array().into_iter().flatten().any(|b| {
            b["metadataBadgeRenderer"]["style"].as_str() == Some("BADGE_STYLE_TYPE_LIVE_NOW")
        });
    let duration = match text(&v["lengthText"]).and_then(|t| parse_clock(&t)) {
        Some(secs) => Some(DurationInfo {
            secs,
            live_or_upcoming: false,
        }),
        None if live => Some(DurationInfo {
            secs: 0,
            live_or_upcoming: true,
        }),
        None => None,
    };
    let url = v["navigationEndpoint"]["commandMetadata"]["webCommandMetadata"]["url"]
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

/// `{"simpleText": ..}` or `{"runs": [{"text": ..}, ..]}`.
fn text(v: &Value) -> Option<String> {
    if let Some(s) = v["simpleText"].as_str() {
        return Some(s.to_string());
    }
    let runs = v["runs"].as_array()?;
    Some(runs.iter().filter_map(|r| r["text"].as_str()).collect())
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

    const RELATED: &str = include_str!("../../tests/fixtures/next_related.json");
    const SEARCH: &str = include_str!("../../tests/fixtures/search_results.json");

    #[test]
    fn parses_related_lockups() {
        let now = Utc::now();
        let v: Value = serde_json::from_str(RELATED).unwrap();
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
    fn parses_search_results() {
        let now = Utc::now();
        let v: Value = serde_json::from_str(SEARCH).unwrap();
        let items = parse(&v, now);
        assert_eq!(items.len(), 12, "Shorts shelf skipped");
        assert!(!items.iter().any(|i| i.video_id == "zzzzzzzzzzz"));
        let first = &items[0];
        assert_eq!(first.video_id, "rQ_J9WH6CGk");
        assert_eq!(first.channel_title, "BekBrace");
        assert_eq!(first.channel_id, "UC7EVSn5inapL20oPSwAwEUg");
        assert!(first.title.starts_with("Rust Programming Full Course"));
        assert_eq!(first.duration.unwrap().secs, 3 * 3600 + 5 * 60 + 4);
        assert_eq!(first.link_hint, LinkHint::WatchLink);
        let age = now - first.published;
        assert!(age.num_days() >= 365 * 2 - 1 && age.num_days() < 365 * 3);
        assert!(
            items
                .iter()
                .all(|i| !i.title.is_empty() && !i.channel_title.is_empty())
        );
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
