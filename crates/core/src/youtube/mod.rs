pub mod api;
pub mod duration;
pub mod related;
pub mod rss;

pub fn watch_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

/// 320x180, 16:9 with no letterboxing; matches the card's display size.
pub fn thumbnail_url(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/mqdefault.jpg")
}

/// Accepts a bare 11-character id or any common YouTube URL form.
pub fn parse_video_id(input: &str) -> Option<String> {
    let valid = |s: &str| {
        s.len() == 11
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    let input = input.trim();
    if valid(input) {
        return Some(input.to_string());
    }
    let url = url::Url::parse(input).ok()?;
    let host = url
        .host_str()?
        .trim_start_matches("www.")
        .trim_start_matches("m.");
    let candidate = match host {
        "youtu.be" => url.path_segments()?.next()?.to_string(),
        "youtube.com" | "music.youtube.com" => {
            if let Some((_, v)) = url.query_pairs().find(|(k, _)| k == "v") {
                v.into_owned()
            } else {
                let mut segs = url.path_segments()?;
                match segs.next()? {
                    "shorts" | "embed" | "live" | "v" => segs.next()?.to_string(),
                    _ => return None,
                }
            }
        }
        _ => return None,
    };
    valid(&candidate).then_some(candidate)
}

/// A channel's uploads playlist id is its channel id with `UC` -> `UU`.
pub fn uploads_playlist_id(channel_id: &str) -> Option<String> {
    channel_id
        .strip_prefix("UC")
        .map(|rest| format!("UU{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_video_ids() {
        let id = Some("dQw4w9WgXcQ".to_string());
        assert_eq!(parse_video_id("dQw4w9WgXcQ"), id);
        assert_eq!(
            parse_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10"),
            id
        );
        assert_eq!(parse_video_id("https://youtu.be/dQw4w9WgXcQ?si=x"), id);
        assert_eq!(
            parse_video_id("https://m.youtube.com/shorts/dQw4w9WgXcQ"),
            id
        );
        assert_eq!(
            parse_video_id("https://www.youtube.com/embed/dQw4w9WgXcQ"),
            id
        );
        assert_eq!(
            parse_video_id("https://example.com/watch?v=dQw4w9WgXcQ"),
            None
        );
        assert_eq!(parse_video_id("short"), None);
    }

    #[test]
    fn uploads_playlist() {
        assert_eq!(uploads_playlist_id("UCabc").as_deref(), Some("UUabc"));
        assert_eq!(uploads_playlist_id("HCabc"), None);
    }
}
