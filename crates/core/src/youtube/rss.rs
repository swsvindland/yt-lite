//! Per-channel uploads feed: `https://www.youtube.com/feeds/videos.xml?channel_id=...`
//!
//! Free (no API quota) and returns the latest 15 uploads. Notable details as of
//! 2026-10: entry links for Shorts point at `/shorts/<id>`, and the feed-level
//! `<yt:channelId>` omits the `UC` prefix (entry-level ids keep it).

use anyhow::{Context as _, Result, anyhow};
use chrono::{DateTime, Utc};

use crate::feed::FeedItem;
use crate::shorts::LinkHint;

const ATOM: &str = "http://www.w3.org/2005/Atom";
const YT: &str = "http://www.youtube.com/xml/schemas/2015";

pub fn feed_url(channel_id: &str) -> String {
    format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}")
}

pub struct ChannelFeed {
    #[cfg_attr(not(test), allow(dead_code))]
    pub channel_title: String,
    pub items: Vec<FeedItem>,
}

pub fn parse(xml: &str, channel_id: &str) -> Result<ChannelFeed> {
    let doc = roxmltree::Document::parse(xml).context("invalid RSS XML")?;
    let feed = doc.root_element();
    if !feed.has_tag_name((ATOM, "feed")) {
        return Err(anyhow!("not an Atom feed"));
    }
    let channel_title = child_text(feed, ATOM, "title").unwrap_or_default();

    let mut items = Vec::new();
    for entry in feed.children().filter(|n| n.has_tag_name((ATOM, "entry"))) {
        let Some(video_id) = child_text(entry, YT, "videoId") else {
            continue;
        };
        let title = child_text(entry, ATOM, "title").unwrap_or_default();
        let published = child_text(entry, ATOM, "published")
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|d| d.with_timezone(&Utc));
        let Some(published) = published else {
            continue;
        };
        let link = entry
            .children()
            .find(|n| n.has_tag_name((ATOM, "link")) && n.attribute("rel") == Some("alternate"))
            .and_then(|n| n.attribute("href"))
            .unwrap_or_default();
        let entry_channel = child_text(entry, YT, "channelId").unwrap_or_else(|| channel_id.into());
        let author = entry
            .children()
            .find(|n| n.has_tag_name((ATOM, "author")))
            .and_then(|a| child_text(a, ATOM, "name"))
            .unwrap_or_else(|| channel_title.clone());

        items.push(FeedItem {
            video_id,
            channel_id: entry_channel,
            channel_title: author,
            title,
            published,
            link_hint: LinkHint::from_url(link),
        });
    }
    Ok(ChannelFeed {
        channel_title,
        items,
    })
}

fn child_text(node: roxmltree::Node, ns: &str, name: &str) -> Option<String> {
    node.children()
        .find(|n| n.has_tag_name((ns, name)))
        .and_then(|n| n.text())
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MKBHD: &str = include_str!("../../tests/fixtures/rss_mkbhd.xml");
    const NO_SHORTS: &str = include_str!("../../tests/fixtures/rss_no_shorts.xml");
    const MANY_SHORTS: &str = include_str!("../../tests/fixtures/rss_many_shorts.xml");

    #[test]
    fn parses_entries_and_metadata() {
        let feed = parse(MKBHD, "UCBJycsmduvYEL83R_U4JriQ").unwrap();
        assert_eq!(feed.channel_title, "Marques Brownlee");
        assert_eq!(feed.items.len(), 15);
        let first = &feed.items[0];
        assert_eq!(first.video_id, "3iRUwVzRDZQ");
        assert_eq!(first.channel_id, "UCBJycsmduvYEL83R_U4JriQ");
        assert_eq!(first.channel_title, "Marques Brownlee");
        assert_eq!(first.title, "Xiaomi 18 Fold: How Does This Happen?");
        assert_eq!(
            first.published,
            DateTime::parse_from_rfc3339("2026-10-02T20:50:25+00:00").unwrap()
        );
        assert_eq!(first.link_hint, LinkHint::WatchLink);
    }

    #[test]
    fn detects_shorts_links() {
        let feed = parse(MKBHD, "UCBJycsmduvYEL83R_U4JriQ").unwrap();
        let shorts: Vec<_> = feed
            .items
            .iter()
            .filter(|i| i.link_hint == LinkHint::ShortsLink)
            .map(|i| i.video_id.as_str())
            .collect();
        assert!(shorts.contains(&"R6yNUnRXZ64"));
        assert!(shorts.contains(&"uJdjKOBikTE"));
        assert!(shorts.contains(&"v-_d2e7x4KA"));
        // Every entry carries a link one way or the other.
        assert!(feed.items.iter().all(|i| i.link_hint != LinkHint::None));
    }

    #[test]
    fn channel_without_shorts() {
        let feed = parse(NO_SHORTS, "UCsBjURrPoezykLs9EqgamOA").unwrap();
        assert_eq!(feed.items.len(), 15);
        assert!(feed.items.iter().all(|i| i.link_hint == LinkHint::WatchLink));
    }

    #[test]
    fn shorts_heavy_channel() {
        let feed = parse(MANY_SHORTS, "UCYO_jab_esuFRV4b17AJtAw").unwrap();
        let n = feed
            .items
            .iter()
            .filter(|i| i.link_hint == LinkHint::ShortsLink)
            .count();
        assert_eq!(n, 11);
    }

    #[test]
    fn rejects_non_feed() {
        assert!(parse("<html><body>consent</body></html>", "UCx").is_err());
        assert!(parse("not xml", "UCx").is_err());
    }

    #[test]
    fn empty_feed_is_ok() {
        let xml = r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"><title>Empty</title></feed>"#;
        let feed = parse(xml, "UCx").unwrap();
        assert_eq!(feed.channel_title, "Empty");
        assert!(feed.items.is_empty());
    }
}
