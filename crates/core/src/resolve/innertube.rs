//! InnerTube `player` request (the same private API youtube.com's own
//! clients use).
//!
//! Client choice follows yt-dlp (`yt_dlp/extractor/youtube/_base.py`,
//! `INNERTUBE_CLIENTS`). As of 2026-10 `visionos` is the only client whose
//! formats need neither a JS runtime nor a PO token (`android_vr` started
//! returning 403s in 2026-08). If YouTube breaks it, update [`VISIONOS`] from
//! yt-dlp's current values, or add the next working client here.

use std::sync::Mutex;

use anyhow::{Context as _, Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use super::select::{RawFormat, pick_audio, pick_video};
use super::{Prefs, Resolved, StreamResolver, Unplayable};
use crate::db::Db;
use crate::net::Http;

const PLAYER_URL: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";
const VISITOR_META_KEY: &str = "innertube.visitor_data";

/// An InnerTube client identity.
#[derive(Clone, Copy, Debug)]
pub struct ClientProfile {
    pub name: &'static str,
    /// `X-YouTube-Client-Name`
    pub name_id: u32,
    pub version: &'static str,
    pub device_make: &'static str,
    pub device_model: &'static str,
    pub os_name: &'static str,
    pub os_version: &'static str,
    pub user_agent: &'static str,
}

/// yt-dlp `INNERTUBE_CLIENTS['visionos']` (added 2026-07-09).
pub const VISIONOS: ClientProfile = ClientProfile {
    name: "VISIONOS",
    name_id: 101,
    version: "1.02",
    device_make: "Apple",
    device_model: "RealityDevice17,1",
    os_name: "visionOS",
    os_version: "26.5.23O471",
    user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 \
                 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
};

pub struct InnertubeResolver {
    http: Http,
    client: ClientProfile,
    /// Logged-out session id. Without it YouTube answers LOGIN_REQUIRED
    /// ("confirm you're not a bot"); the response to that carries a fresh one.
    visitor: Mutex<Option<String>>,
    db: Option<Db>,
}

impl InnertubeResolver {
    /// `db` persists visitor data across runs (optional).
    pub fn new(http: Http, db: Option<Db>) -> Self {
        let visitor = db
            .as_ref()
            .and_then(|db| db.meta_get(VISITOR_META_KEY).ok().flatten());
        Self {
            http,
            client: VISIONOS,
            visitor: Mutex::new(visitor),
            db,
        }
    }

    fn set_visitor(&self, v: &str) {
        let mut cur = self.visitor.lock().unwrap();
        if cur.as_deref() == Some(v) {
            return;
        }
        *cur = Some(v.to_string());
        if let Some(db) = &self.db
            && let Err(e) = db.meta_set(VISITOR_META_KEY, v)
        {
            log::warn!("saving visitor data: {e:#}");
        }
    }

    fn player(&self, video_id: &str, visitor: Option<&str>) -> Result<PlayerResponse> {
        let c = &self.client;
        let mut client = json!({
            "clientName": c.name,
            "clientVersion": c.version,
            "deviceMake": c.device_make,
            "deviceModel": c.device_model,
            "osName": c.os_name,
            "osVersion": c.os_version,
            "userAgent": c.user_agent,
            "hl": "en",
            "timeZone": "UTC",
            "utcOffsetMinutes": 0,
        });
        if let Some(v) = visitor {
            client["visitorData"] = v.into();
        }
        let body = json!({
            "context": { "client": client },
            "videoId": video_id,
            "playbackContext": {
                "contentPlaybackContext": { "html5Preference": "HTML5_PREF_WANTS" }
            },
            "contentCheckOk": true,
            "racyCheckOk": true,
        });
        let name_id = c.name_id.to_string();
        let mut headers = vec![
            ("X-YouTube-Client-Name", name_id.as_str()),
            ("X-YouTube-Client-Version", c.version),
            ("Origin", "https://www.youtube.com"),
            ("User-Agent", c.user_agent),
        ];
        if let Some(v) = visitor {
            headers.push(("X-Goog-Visitor-Id", v));
        }
        self.http
            .post_json(PLAYER_URL, &headers, &body)
            .with_context(|| format!("InnerTube player request for {video_id}"))
    }
}

impl StreamResolver for InnertubeResolver {
    fn name(&self) -> &'static str {
        "innertube-visionos"
    }

    fn resolve(&self, video_id: &str, prefs: &Prefs) -> Result<Resolved> {
        let visitor = self.visitor.lock().unwrap().clone();
        let mut resp = self.player(video_id, visitor.as_deref())?;
        if let Some(fresh) = resp.visitor_data() {
            let retry = resp.status() == "LOGIN_REQUIRED" && visitor.as_deref() != Some(fresh);
            let fresh = fresh.to_string();
            self.set_visitor(&fresh);
            if retry {
                log::debug!("player: LOGIN_REQUIRED, retrying with fresh visitor data");
                resp = self.player(video_id, Some(&fresh))?;
            }
        }
        interpret(video_id, resp, prefs)
    }
}

/// Turns a player response into streams. Pure; tested with saved responses.
pub fn interpret(video_id: &str, resp: PlayerResponse, prefs: &Prefs) -> Result<Resolved> {
    let status = resp.status().to_string();
    if status != "OK" {
        let reason = resp.playability_status.and_then(|p| p.reason);
        return Err(Unplayable { status, reason }.into());
    }
    let details = resp.video_details.unwrap_or_default();
    let data = resp
        .streaming_data
        .ok_or_else(|| anyhow!("player response has no streamingData"))?;
    let is_live = details.is_live.unwrap_or(false);
    let all: Vec<RawFormat> = data
        .adaptive_formats
        .into_iter()
        .chain(data.formats)
        .collect();
    let (video, audio) = if is_live {
        (None, None)
    } else {
        (pick_video(&all, prefs), pick_audio(&all))
    };
    if video.is_none() && data.hls_manifest_url.is_none() {
        return Err(anyhow!(
            "no playable formats ({} listed; all ciphered, throttled or DRM)",
            all.len()
        ));
    }
    Ok(Resolved {
        video_id: video_id.to_string(),
        title: details.title.unwrap_or_default(),
        is_live,
        video,
        audio,
        hls: data.hls_manifest_url,
        expires_in: data.expires_in_seconds.and_then(|s| s.parse().ok()),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerResponse {
    response_context: Option<ResponseContext>,
    playability_status: Option<PlayabilityStatus>,
    streaming_data: Option<StreamingData>,
    video_details: Option<VideoDetails>,
}

impl PlayerResponse {
    fn status(&self) -> &str {
        self.playability_status
            .as_ref()
            .map(|p| p.status.as_str())
            .unwrap_or("MISSING")
    }

    fn visitor_data(&self) -> Option<&str> {
        self.response_context.as_ref()?.visitor_data.as_deref()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseContext {
    visitor_data: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlayabilityStatus {
    status: String,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamingData {
    expires_in_seconds: Option<String>,
    #[serde(default)]
    adaptive_formats: Vec<RawFormat>,
    #[serde(default)]
    formats: Vec<RawFormat>,
    hls_manifest_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoDetails {
    title: Option<String>,
    is_live: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = include_str!("../../tests/fixtures/player_visionos_ok.json");
    const LOGIN: &str = include_str!("../../tests/fixtures/player_login_required.json");

    #[test]
    fn interprets_real_visionos_response() {
        let resp: PlayerResponse = serde_json::from_str(OK).unwrap();
        assert_eq!(resp.visitor_data(), Some("REDACTED_VISITOR"));
        let r = interpret("3iRUwVzRDZQ", resp, &Prefs::default()).unwrap();
        assert_eq!(r.title, "Xiaomi 18 Fold: How Does This Happen?");
        assert!(!r.is_live);
        assert_eq!(r.expires_in, Some(21540));
        assert!(r.hls.is_some());

        // 2:1 video: "1080p" is 2160x1080; H.264 preferred at that tier.
        let v = r.video.unwrap();
        assert_eq!(v.tier, Some(1080));
        assert_eq!(v.itag, 137);
        assert!(v.codecs.starts_with("avc1"));

        // Original-language Opus, not one of the dubbed tracks.
        let a = r.audio.unwrap();
        assert_eq!(a.itag, 251);
        assert_eq!(a.mime, "audio/webm");
    }

    #[test]
    fn max_tier_is_respected_on_real_response() {
        let resp: PlayerResponse = serde_json::from_str(OK).unwrap();
        let prefs = Prefs {
            max_tier: 720,
            ..Prefs::default()
        };
        let v = interpret("x", resp, &prefs).unwrap().video.unwrap();
        assert_eq!(v.tier, Some(720));
        let resp: PlayerResponse = serde_json::from_str(OK).unwrap();
        let prefs = Prefs {
            max_tier: 4320,
            ..Prefs::default()
        };
        let v = interpret("x", resp, &prefs).unwrap().video.unwrap();
        assert_eq!(v.tier, Some(2160));
    }

    #[test]
    fn login_required_is_unplayable_and_carries_visitor() {
        let resp: PlayerResponse = serde_json::from_str(LOGIN).unwrap();
        assert_eq!(resp.visitor_data(), Some("FRESH_VISITOR"));
        let err = interpret("x", resp, &Prefs::default()).unwrap_err();
        let u = err.downcast_ref::<Unplayable>().unwrap();
        assert_eq!(u.status, "LOGIN_REQUIRED");
        assert!(u.reason.as_deref().unwrap().contains("not a bot"));
    }

    #[test]
    fn live_uses_hls_only() {
        let json = r#"{"playabilityStatus":{"status":"OK"},
            "videoDetails":{"title":"Live","isLive":true},
            "streamingData":{"hlsManifestUrl":"https://m/hls.m3u8","adaptiveFormats":[
              {"itag":299,"url":"https://x/v?itag=299","mimeType":"video/mp4; codecs=\"avc1\"","qualityLabel":"1080p60","targetDurationSec":5.0}]}}"#;
        let r = interpret("x", serde_json::from_str(json).unwrap(), &Prefs::default()).unwrap();
        assert!(r.is_live);
        assert!(r.video.is_none());
        assert_eq!(r.hls.as_deref(), Some("https://m/hls.m3u8"));
    }

    #[test]
    fn no_usable_formats_is_an_error() {
        let json = r#"{"playabilityStatus":{"status":"OK"},"streamingData":{"adaptiveFormats":[
            {"itag":137,"signatureCipher":"s=x&url=y","mimeType":"video/mp4; codecs=\"avc1\""}]}}"#;
        let err = interpret("x", serde_json::from_str(json).unwrap(), &Prefs::default()).unwrap_err();
        assert!(err.to_string().contains("no playable formats"));
    }
}
