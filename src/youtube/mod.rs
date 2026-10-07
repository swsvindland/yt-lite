pub mod api;
pub mod duration;
pub mod rss;

pub fn watch_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

/// 320x180, 16:9 with no letterboxing; matches the card's display size.
pub fn thumbnail_url(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/mqdefault.jpg")
}

/// A channel's uploads playlist id is its channel id with `UC` -> `UU`.
pub fn uploads_playlist_id(channel_id: &str) -> Option<String> {
    channel_id.strip_prefix("UC").map(|rest| format!("UU{rest}"))
}
