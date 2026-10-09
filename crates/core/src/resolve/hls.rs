//! HLS master playlists: finding the audio-only rendition.
//!
//! Audio-only playback on iOS uses this rather than the AAC file from
//! `adaptiveFormats`. googlevideo throttles requests for more than about
//! 10 MB to ~32 KB/s, and AVPlayer asks for a progressive file in one
//! request, so anything longer than ~10 minutes never starts. HLS segments
//! are a few seconds each, and AVPlayer gets exact durations and seeking.

use anyhow::Result;

use crate::net::Http;

/// Downloads a master playlist and returns its audio-only rendition (see
/// [`audio_rendition`]). Blocking.
pub fn fetch_audio_rendition(http: &Http, master_url: &str) -> Result<Option<String>> {
    let master = http.get_bytes(master_url)?;
    Ok(audio_rendition(
        master_url,
        &String::from_utf8_lossy(&master),
    ))
}

/// The media playlist URL of the best audio-only rendition: from the audio
/// group of the highest-bandwidth variant (AAC rather than HE-AAC on
/// YouTube), the original-language track if the video is dubbed. `None` if
/// the playlist has no separate audio.
pub fn audio_rendition(master_url: &str, master: &str) -> Option<String> {
    let mut renditions = vec![];
    // (bandwidth, audio group) of the best variant so far.
    let mut best: Option<(u64, &str)> = None;
    for line in master.lines() {
        if let Some(list) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = Attrs::parse(list);
            if a.get("TYPE") == Some("AUDIO") && a.get("URI").is_some() {
                renditions.push(a);
            }
        } else if let Some(list) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let a = Attrs::parse(list);
            let bandwidth = a.get("BANDWIDTH").and_then(|b| b.parse().ok());
            if let (Some(bandwidth), Some(group)) = (bandwidth, a.get("AUDIO"))
                && best.is_none_or(|(b, _)| bandwidth > b)
            {
                best = Some((bandwidth, group));
            }
        }
    }
    let group: Vec<&Attrs> = renditions
        .iter()
        .filter(|r| best.is_none_or(|(_, g)| r.get("GROUP-ID") == Some(g)))
        .collect();
    let pick = group
        .iter()
        .find(|r| {
            r.get("NAME")
                .is_some_and(|n| n.to_lowercase().contains("original"))
        })
        .or_else(|| group.iter().find(|r| r.get("DEFAULT") == Some("YES")))
        .or_else(|| group.first())?;
    let uri = pick.get("URI")?;
    if uri.contains("://") {
        return Some(uri.to_string());
    }
    let base = url::Url::parse(master_url).ok()?;
    base.join(uri).ok().map(String::from)
}

/// An HLS attribute list: `KEY=VALUE,KEY="quoted, value"`.
struct Attrs<'a>(Vec<(&'a str, &'a str)>);

impl<'a> Attrs<'a> {
    fn parse(mut list: &'a str) -> Self {
        let mut pairs = vec![];
        while let Some((key, rest)) = list.split_once('=') {
            let (value, next) = match rest.strip_prefix('"') {
                Some(quoted) => quoted.split_once('"').unwrap_or((quoted, "")),
                None => rest.split_once(',').unwrap_or((rest, "")),
            };
            pairs.push((key.trim(), value));
            list = next.strip_prefix(',').unwrap_or(next);
        }
        Self(pairs)
    }

    fn get(&self, key: &str) -> Option<&'a str> {
        self.0.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUBBED: &str = include_str!("../../tests/fixtures/hls_master_dubbed.m3u8");
    const MASTER_URL: &str =
        "https://manifest.googlevideo.com/api/manifest/hls_variant/id/x/file/index.m3u8";

    #[test]
    fn picks_aac_original_language_from_real_master() {
        // Groups 233 (HE-AAC) and 234 (AAC), each with Japanese and
        // Portuguese dubs and the English original.
        let uri = audio_rendition(MASTER_URL, DUBBED).unwrap();
        assert!(uri.contains("/itag/234/lang/en/"), "{uri}");
    }

    #[test]
    fn single_track_takes_the_default_of_the_best_group() {
        let master = r#"#EXTM3U
#EXT-X-MEDIA:URI="https://m/a233.m3u8",TYPE=AUDIO,GROUP-ID="233",NAME="Default",DEFAULT=YES,AUTOSELECT=YES
#EXT-X-MEDIA:URI="https://m/a234.m3u8",TYPE=AUDIO,GROUP-ID="234",NAME="Default",DEFAULT=YES,AUTOSELECT=YES
#EXT-X-STREAM-INF:BANDWIDTH=164976,CODECS="avc1.4D400C,mp4a.40.5",RESOLUTION=256x128,AUDIO="233"
https://m/v160.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2657078,CODECS="avc1.640028,mp4a.40.2",RESOLUTION=1920x960,AUDIO="234"
https://m/v1080.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=294327,CODECS="avc1.4D400D,mp4a.40.5",RESOLUTION=426x214,AUDIO="233"
https://m/v240.m3u8
"#;
        assert_eq!(
            audio_rendition(MASTER_URL, master).as_deref(),
            Some("https://m/a234.m3u8")
        );
    }

    #[test]
    fn muxed_playlist_has_no_audio_rendition() {
        let master = r#"#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=1500000,CODECS="avc1.4d401f,mp4a.40.2",RESOLUTION=1280x720
https://m/720.m3u8
"#;
        assert_eq!(audio_rendition(MASTER_URL, master), None);
    }

    #[test]
    fn relative_uri_resolves_against_the_master() {
        let master = r#"#EXTM3U
#EXT-X-MEDIA:URI="audio/en.m3u8",TYPE=AUDIO,GROUP-ID="a",NAME="English",DEFAULT=YES
#EXT-X-STREAM-INF:BANDWIDTH=100,AUDIO="a"
video.m3u8
"#;
        assert_eq!(
            audio_rendition("https://h/p/master.m3u8?x=1", master).as_deref(),
            Some("https://h/p/audio/en.m3u8")
        );
    }

    #[test]
    fn attribute_lists_keep_commas_inside_quotes() {
        let a = Attrs::parse(
            r#"BANDWIDTH=294327,CODECS="avc1.4D400D,mp4a.40.5",AUDIO="233",CLOSED-CAPTIONS=NONE"#,
        );
        assert_eq!(a.get("BANDWIDTH"), Some("294327"));
        assert_eq!(a.get("CODECS"), Some("avc1.4D400D,mp4a.40.5"));
        assert_eq!(a.get("AUDIO"), Some("233"));
        assert_eq!(a.get("CLOSED-CAPTIONS"), Some("NONE"));
        assert_eq!(a.get("RESOLUTION"), None);
    }
}
