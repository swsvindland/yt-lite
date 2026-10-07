//! Format selection over a `player` response's `streamingData`. Pure, so it
//! is unit tested against a saved response.

use serde::Deserialize;

use super::{Prefs, Stream};

/// One entry of `streamingData.adaptiveFormats` / `formats`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawFormat {
    pub itag: u32,
    pub url: Option<String>,
    pub signature_cipher: Option<String>,
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub bitrate: Option<u64>,
    pub content_length: Option<String>,
    pub quality_label: Option<String>,
    pub audio_track: Option<AudioTrack>,
    pub is_drc: Option<bool>,
    #[serde(rename = "type")]
    pub stream_type: Option<String>,
    pub target_duration_sec: Option<f64>,
    pub drm_families: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTrack {
    pub display_name: Option<String>,
    pub audio_is_default: Option<bool>,
}

/// `video/mp4; codecs="avc1.640028"` -> (`video/mp4`, `avc1.640028`).
pub fn split_mime(mime_type: &str) -> (String, String) {
    let mut parts = mime_type.splitn(2, ';');
    let mime = parts.next().unwrap_or_default().trim().to_string();
    let codecs = parts
        .next()
        .and_then(|rest| rest.split_once("codecs="))
        .map(|(_, c)| c.trim().trim_matches('"').to_string())
        .unwrap_or_default();
    (mime, codecs)
}

/// Codec family used for preferences: `avc1`, `vp9`, `av01`, `opus`, `mp4a`.
pub fn codec_family(codecs: &str) -> &'static str {
    let c = codecs.to_ascii_lowercase();
    if c.starts_with("avc1") || c.starts_with("avc3") {
        "avc1"
    } else if c.starts_with("vp9") || c.starts_with("vp09") {
        "vp9"
    } else if c.starts_with("av01") {
        "av01"
    } else if c.starts_with("opus") {
        "opus"
    } else if c.starts_with("mp4a") {
        "mp4a"
    } else {
        "other"
    }
}

/// "1080p60" -> 1080. Falls back to the short side of the frame.
fn tier(f: &RawFormat) -> Option<u32> {
    f.quality_label
        .as_deref()
        .and_then(|l| {
            let digits: String = l.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        })
        .or_else(|| match (f.width, f.height) {
            (Some(w), Some(h)) => Some(w.min(h)),
            (None, h) => h,
            (w, None) => w,
        })
}

fn has_query_param(url: &str, name: &str) -> bool {
    url::Url::parse(url)
        .map(|u| u.query_pairs().any(|(k, _)| k == name))
        .unwrap_or(false)
}

/// Formats we can hand to a player as-is.
fn usable(f: &RawFormat) -> bool {
    let Some(url) = &f.url else { return false };
    f.signature_cipher.is_none()
        && f.drm_families.is_none()
        // Live fragments and OTF streams need a segment-aware downloader.
        && f.target_duration_sec.is_none()
        && f.stream_type.as_deref() != Some("FORMAT_STREAM_TYPE_OTF")
        // An `n` parameter must be transformed by player JS or the download
        // is throttled to a crawl; yt-dlp skips such formats too.
        && !has_query_param(url, "n")
}

fn to_stream(f: &RawFormat) -> Stream {
    let (mime, codecs) = split_mime(&f.mime_type);
    Stream {
        itag: f.itag,
        url: f.url.clone().unwrap_or_default(),
        mime,
        codecs,
        width: f.width,
        height: f.height,
        tier: tier(f),
        fps: f.fps,
        bitrate: f.bitrate.unwrap_or(0),
        content_length: f.content_length.as_deref().and_then(|s| s.parse().ok()),
    }
}

pub fn pick_video(formats: &[RawFormat], prefs: &Prefs) -> Option<Stream> {
    let candidates: Vec<&RawFormat> = formats
        .iter()
        .filter(|f| usable(f) && f.mime_type.starts_with("video/"))
        .collect();
    let rank = |f: &RawFormat| {
        let family = codec_family(&split_mime(&f.mime_type).1);
        prefs
            .codecs
            .iter()
            .position(|c| c == family)
            .unwrap_or(prefs.codecs.len())
    };
    let within: Vec<&RawFormat> = candidates
        .iter()
        .copied()
        .filter(|f| tier(f).is_some_and(|t| t <= prefs.max_tier))
        .collect();
    let best = if within.is_empty() {
        // Nothing small enough: take the lowest tier available.
        candidates.into_iter().min_by_key(|f| tier(f).unwrap_or(u32::MAX))
    } else {
        within.into_iter().max_by(|a, b| {
            (tier(a), a.fps.unwrap_or(0))
                .cmp(&(tier(b), b.fps.unwrap_or(0)))
                // Lower rank index = more preferred.
                .then_with(|| rank(b).cmp(&rank(a)))
                .then_with(|| a.bitrate.cmp(&b.bitrate))
        })
    };
    best.map(to_stream)
}

pub fn pick_audio(formats: &[RawFormat]) -> Option<Stream> {
    let candidates: Vec<&RawFormat> = formats
        .iter()
        .filter(|f| usable(f) && f.mime_type.starts_with("audio/") && f.is_drc != Some(true))
        .collect();
    // Multi-language videos (auto-dubbing) list every track; keep the original.
    let is_original = |f: &&RawFormat| {
        f.audio_track.as_ref().is_some_and(|t| {
            t.audio_is_default == Some(true)
                || t.display_name
                    .as_deref()
                    .is_some_and(|n| n.to_ascii_lowercase().contains("original"))
        })
    };
    let originals: Vec<&RawFormat> = candidates.iter().copied().filter(is_original).collect();
    let pool = if originals.is_empty() { candidates } else { originals };
    pool.into_iter()
        .max_by(|a, b| {
            a.bitrate
                .cmp(&b.bitrate)
                // Opus is a little better than AAC at the same bitrate.
                .then_with(|| {
                    let opus = |f: &RawFormat| codec_family(&split_mime(&f.mime_type).1) == "opus";
                    opus(a).cmp(&opus(b))
                })
        })
        .map(to_stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(itag: u32, mime: &str, label: Option<&str>, w: u32, h: u32, fps: u32, bitrate: u64) -> RawFormat {
        RawFormat {
            itag,
            url: Some(format!("https://example.googlevideo.com/videoplayback?itag={itag}")),
            signature_cipher: None,
            mime_type: mime.into(),
            width: Some(w),
            height: Some(h),
            fps: Some(fps),
            bitrate: Some(bitrate),
            content_length: Some("1000".into()),
            quality_label: label.map(Into::into),
            audio_track: None,
            is_drc: None,
            stream_type: None,
            target_duration_sec: None,
            drm_families: None,
        }
    }

    fn audio(itag: u32, mime: &str, bitrate: u64, track: Option<(&str, bool)>, drc: bool) -> RawFormat {
        RawFormat {
            width: None,
            height: None,
            fps: None,
            quality_label: None,
            audio_track: track.map(|(n, d)| AudioTrack {
                display_name: Some(n.into()),
                audio_is_default: Some(d),
            }),
            is_drc: drc.then_some(true),
            ..fmt(itag, mime, None, 0, 0, 0, bitrate)
        }
    }

    #[test]
    fn mime_parsing() {
        assert_eq!(
            split_mime(r#"video/mp4; codecs="avc1.640028""#),
            ("video/mp4".into(), "avc1.640028".into())
        );
        assert_eq!(split_mime("audio/webm"), ("audio/webm".into(), "".into()));
        assert_eq!(codec_family("vp09.00.40.08"), "vp9");
        assert_eq!(codec_family("av01.0.08M.08"), "av01");
        assert_eq!(codec_family("mp4a.40.2"), "mp4a");
    }

    #[test]
    fn picks_highest_tier_within_cap_preferring_codec_order() {
        let formats = vec![
            fmt(313, r#"video/webm; codecs="vp9""#, Some("2160p"), 3840, 2160, 30, 20_000_000),
            fmt(137, r#"video/mp4; codecs="avc1.640028""#, Some("1080p"), 1920, 1080, 30, 4_000_000),
            fmt(248, r#"video/webm; codecs="vp9""#, Some("1080p"), 1920, 1080, 30, 2_500_000),
            fmt(136, r#"video/mp4; codecs="avc1.4d401f""#, Some("720p"), 1280, 720, 30, 2_000_000),
        ];
        let v = pick_video(&formats, &Prefs::default()).unwrap();
        assert_eq!(v.itag, 137);
        assert_eq!(v.tier, Some(1080));

        let vp9_first = Prefs {
            codecs: vec!["vp9".into(), "avc1".into()],
            ..Prefs::default()
        };
        assert_eq!(pick_video(&formats, &vp9_first).unwrap().itag, 248);

        let uhd = Prefs {
            max_tier: 2160,
            ..Prefs::default()
        };
        assert_eq!(pick_video(&formats, &uhd).unwrap().itag, 313);
    }

    #[test]
    fn higher_fps_wins_at_same_tier() {
        let formats = vec![
            fmt(136, r#"video/mp4; codecs="avc1""#, Some("720p"), 1280, 720, 30, 2_000_000),
            fmt(298, r#"video/mp4; codecs="avc1""#, Some("720p60"), 1280, 720, 60, 3_000_000),
        ];
        assert_eq!(pick_video(&formats, &Prefs::default()).unwrap().itag, 298);
    }

    #[test]
    fn falls_back_to_lowest_tier_when_all_exceed_cap() {
        let formats = vec![
            fmt(1, r#"video/mp4; codecs="avc1""#, Some("1440p"), 2560, 1440, 30, 1),
            fmt(2, r#"video/mp4; codecs="avc1""#, Some("2160p"), 3840, 2160, 30, 1),
        ];
        assert_eq!(pick_video(&formats, &Prefs::default()).unwrap().itag, 1);
    }

    #[test]
    fn rejects_ciphered_throttled_and_live_fragments() {
        let mut ciphered = fmt(1, r#"video/mp4; codecs="avc1""#, Some("1080p"), 1920, 1080, 30, 9);
        ciphered.url = None;
        ciphered.signature_cipher = Some("s=...&url=...".into());
        let mut throttled = fmt(2, r#"video/mp4; codecs="avc1""#, Some("1080p"), 1920, 1080, 30, 8);
        throttled.url = Some("https://x.googlevideo.com/videoplayback?itag=2&n=abcdef".into());
        let mut live = fmt(3, r#"video/mp4; codecs="avc1""#, Some("1080p"), 1920, 1080, 30, 7);
        live.target_duration_sec = Some(5.0);
        let mut otf = fmt(4, r#"video/mp4; codecs="avc1""#, Some("1080p"), 1920, 1080, 30, 6);
        otf.stream_type = Some("FORMAT_STREAM_TYPE_OTF".into());
        // `mn=` must not be mistaken for `n=`.
        let mut ok = fmt(5, r#"video/mp4; codecs="avc1""#, Some("480p"), 854, 480, 30, 1);
        ok.url = Some("https://x.googlevideo.com/videoplayback?itag=5&mn=sn-abc".into());
        let v = pick_video(&[ciphered, throttled, live, otf, ok], &Prefs::default()).unwrap();
        assert_eq!(v.itag, 5);
    }

    #[test]
    fn audio_prefers_original_track_and_skips_drc() {
        let formats = vec![
            audio(251, r#"audio/webm; codecs="opus""#, 140_000, Some(("Japanese", false)), false),
            audio(140, r#"audio/mp4; codecs="mp4a.40.2""#, 130_000, Some(("English original", true)), false),
            audio(251, r#"audio/webm; codecs="opus""#, 136_000, Some(("English original", true)), false),
            audio(251, r#"audio/webm; codecs="opus""#, 999_000, Some(("English original", true)), true),
        ];
        let a = pick_audio(&formats).unwrap();
        assert_eq!((a.itag, a.bitrate), (251, 136_000));
    }

    #[test]
    fn audio_without_tracks_takes_best_bitrate() {
        let formats = vec![
            audio(140, r#"audio/mp4; codecs="mp4a.40.2""#, 130_000, None, false),
            audio(249, r#"audio/webm; codecs="opus""#, 50_000, None, false),
        ];
        assert_eq!(pick_audio(&formats).unwrap().itag, 140);
    }
}
