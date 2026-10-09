//! Native stream resolution: video id -> playable stream URLs, without yt-dlp.
//!
//! Ported from how yt-dlp's YouTube extractor works (as of 2026-10): a single
//! InnerTube `player` request as a client whose stream URLs are plain (no
//! signature cipher, no `n` throttling parameter), so no JavaScript has to be
//! executed. Today that is the `visionos` client; see [`innertube::VISIONOS`].
//!
//! YouTube changes this regularly. When it breaks, front-ends fall back to
//! yt-dlp (desktop) and the fix is usually a constant in `innertube.rs`.

pub mod hls;
pub mod innertube;
pub mod select;

use anyhow::Result;

/// One selected stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub itag: u32,
    pub url: String,
    /// e.g. `video/mp4`
    pub mime: String,
    /// e.g. `avc1.640028`
    pub codecs: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Quality tier in "p" (1080 for 1080p), from YouTube's `qualityLabel`.
    pub tier: Option<u32>,
    pub fps: Option<u32>,
    pub bitrate: u64,
    pub content_length: Option<u64>,
}

/// What a front-end needs to play a video.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub video_id: String,
    pub title: String,
    pub is_live: bool,
    /// Best video-only adaptive stream within the quality preference.
    pub video: Option<Stream>,
    /// Best audio-only adaptive stream (original language, non-DRC).
    pub audio: Option<Stream>,
    /// HLS master playlist (all qualities). What AVPlayer on iOS wants, and
    /// the only option for live streams.
    pub hls: Option<String>,
    /// Stream URLs expire after this many seconds (about 6 h) and are bound to
    /// the requesting IP address.
    pub expires_in: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Prefs {
    /// Maximum quality tier, e.g. 1080.
    pub max_tier: u32,
    /// Video codec preference, best first, applied after quality and fps.
    /// Families: `avc1`, `vp9`, `av01`.
    pub codecs: Vec<String>,
    /// Only consider audio streams of this MIME type, e.g. `audio/mp4` for
    /// players that can't decode Opus/WebM (iOS AVPlayer).
    pub audio_mime: Option<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            max_tier: 1080,
            // H.264 decodes in hardware everywhere; YouTube only offers it up
            // to 1080p, so higher tiers pick VP9/AV1 automatically.
            codecs: vec!["avc1".into(), "vp9".into(), "av01".into()],
            audio_mime: None,
        }
    }
}

/// The video can't be played through this resolver (age gate, made for
/// kids, bot check, offline stream, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unplayable {
    pub status: String,
    pub reason: Option<String>,
}

impl std::fmt::Display for Unplayable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reason {
            Some(r) => write!(f, "YouTube says {}: {r}", self.status),
            None => write!(f, "YouTube says {}", self.status),
        }
    }
}

impl std::error::Error for Unplayable {}

/// Blocking.
pub trait StreamResolver: Send + Sync {
    fn name(&self) -> &'static str;
    fn resolve(&self, video_id: &str, prefs: &Prefs) -> Result<Resolved>;
}
