//! Settings file (`config.toml`) and platform directories.
//!
//! Windows: `%APPDATA%\yt-lite\config\config.toml`, cache under
//! `%LOCALAPPDATA%\yt-lite\cache`. Override the config file with `YT_LITE_CONFIG`,
//! or put everything in one folder with `YT_LITE_HOME`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub google: GoogleConfig,
    pub player: PlayerConfig,
    pub feed: FeedConfig,
    pub cache: CacheConfig,
    pub ui: UiConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct GoogleConfig {
    /// OAuth client of type "Desktop app" from Google Cloud Console.
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct PlayerConfig {
    /// `system` (the OS media player: AVPlayer / Windows MediaPlayer) or `mpv`.
    pub backend: String,
    /// Path to mpv, or a bare name looked up on PATH.
    pub mpv_path: String,
    /// Path to yt-dlp, or a bare name looked up on PATH.
    pub ytdlp_path: String,
    /// Maximum video height passed to yt-dlp's format selector.
    pub max_height: u32,
    /// Load the SponsorBlock mpv script if it is present.
    pub sponsorblock: bool,
    /// Explicit path to sponsorblock.lua. When empty, looks in
    /// `<config dir>/mpv-scripts/sponsorblock.lua`.
    pub sponsorblock_script: String,
    /// Extra arguments appended to the mpv command line.
    pub extra_args: Vec<String>,
    /// `native` (resolve streams in-process, fall back to yt-dlp on failure)
    /// or `yt-dlp` (always let mpv use yt-dlp).
    pub resolver: String,
    /// Video codec preference for the native resolver, best first.
    pub codecs: Vec<String>,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            backend: "system".into(),
            mpv_path: "mpv".into(),
            ytdlp_path: "yt-dlp".into(),
            max_height: 1080,
            sponsorblock: false,
            sponsorblock_script: String::new(),
            extra_args: Vec::new(),
            resolver: "native".into(),
            codecs: vec!["avc1".into(), "vp9".into(), "av01".into()],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct FeedConfig {
    /// Automatic refresh interval. Values under 15 are raised to 15.
    pub refresh_interval_minutes: u64,
    /// Only show uploads newer than this.
    pub max_age_days: u32,
    /// Upper bound on rows loaded into the list.
    pub max_items: u32,
    /// Parallel RSS requests.
    pub rss_concurrency: usize,
    pub hide_watched: bool,
    /// Channel ids (UC...) to follow via RSS even without signing in.
    pub extra_channels: Vec<String>,
}

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            refresh_interval_minutes: 15,
            max_age_days: 60,
            max_items: 1000,
            rss_concurrency: 8,
            hide_watched: false,
            extra_channels: Vec::new(),
        }
    }
}

impl FeedConfig {
    pub const MIN_REFRESH_MINUTES: u64 = 15;

    pub fn refresh_interval(&self) -> std::time::Duration {
        let mins = self.refresh_interval_minutes.max(Self::MIN_REFRESH_MINUTES);
        std::time::Duration::from_secs(mins * 60)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct CacheConfig {
    /// Hard cap on decoded thumbnails held in memory.
    pub thumb_memory_mb: usize,
    /// Disk cache for downloaded thumbnails; pruned oldest-first on startup.
    pub thumb_disk_mb: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            thumb_memory_mb: 50,
            thumb_disk_mb: 300,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct UiConfig {
    /// Show current process memory in the status bar.
    pub show_memory: bool,
    pub dark: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            show_memory: true,
            dark: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub config_file: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        // YT_LITE_HOME puts everything under one folder (portable mode).
        if let Some(home) = std::env::var_os("YT_LITE_HOME") {
            return Ok(Self::in_dir(PathBuf::from(home)));
        }
        let dirs = ProjectDirs::from("", "", "yt-lite").context("no home directory")?;
        let (config_dir, data_dir, cache_dir) = (
            dirs.config_dir().to_path_buf(),
            dirs.data_local_dir().to_path_buf(),
            dirs.cache_dir().to_path_buf(),
        );
        let config_file = std::env::var_os("YT_LITE_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| config_dir.join("config.toml"));
        Ok(Self {
            config_file,
            config_dir,
            data_dir,
            cache_dir,
        })
    }

    /// Everything under one root (portable mode, mobile app containers).
    pub fn in_dir(root: PathBuf) -> Self {
        Self {
            config_file: root.join("config.toml"),
            config_dir: root.clone(),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
        }
    }

    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("cache.sqlite3")
    }

    pub fn thumbs_dir(&self) -> PathBuf {
        self.cache_dir.join("thumbs")
    }

    pub fn default_sponsorblock_script(&self) -> PathBuf {
        self.config_dir.join("mpv-scripts").join("sponsorblock.lua")
    }
}

impl Config {
    /// Loads the config, writing a commented default file on first run.
    pub fn load_or_init(path: &Path) -> Result<Self> {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, DEFAULT_CONFIG)
                .with_context(|| format!("writing default config to {}", path.display()))?;
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    pub fn has_google_client(&self) -> bool {
        !self.google.client_id.trim().is_empty() && !self.google.client_secret.trim().is_empty()
    }
}

/// Written on first run. Kept as `config.example.toml` at the repository root.
pub const DEFAULT_CONFIG: &str = include_str!("../../../config.example.toml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_parses_to_defaults() {
        let c = Config::parse(DEFAULT_CONFIG).unwrap();
        assert_eq!(c.player.max_height, 1080);
        assert_eq!(c.player.resolver, "native");
        assert_eq!(c.player.backend, "system");
        assert_eq!(c.feed.refresh_interval_minutes, 15);
        assert_eq!(c.cache.thumb_memory_mb, 50);
        assert!(!c.has_google_client());
    }

    #[test]
    fn partial_file_fills_defaults() {
        let c = Config::parse("[player]\nmax_height = 720\n").unwrap();
        assert_eq!(c.player.max_height, 720);
        assert_eq!(c.player.mpv_path, "mpv");
        assert_eq!(c.feed.max_items, 1000);
    }

    #[test]
    fn refresh_interval_has_floor() {
        let c = Config::parse("[feed]\nrefresh_interval_minutes = 1\n").unwrap();
        assert_eq!(c.feed.refresh_interval().as_secs(), 15 * 60);
    }
}
