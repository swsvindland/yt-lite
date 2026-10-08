//! The operating system's own media player, playing the HLS stream from the
//! native resolver: AVPlayer on macOS, WinRT `MediaPlayer` on Windows.
//! Nothing to install; decoding is hardware-accelerated by the OS.
//!
//! [`SystemPlayer`] lives on the UI thread; [`PlayRequest`]s are produced in the
//! background by `player::Player::prepare`.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::SystemPlayer;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::SystemPlayer;

/// What the system player needs to start a video.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayRequest {
    /// HLS master playlist (or any URL the OS player can open).
    pub url: String,
    pub title: String,
    /// Highest quality tier to pick from the HLS variants, e.g. 1080.
    pub max_tier: u32,
    /// Video width / height, to size the window.
    pub aspect: Option<f64>,
}

/// True when this platform has a system player backend.
pub const SUPPORTED: bool = cfg!(any(target_os = "macos", windows));

#[cfg(not(any(target_os = "macos", windows)))]
#[derive(Default)]
pub struct SystemPlayer;

#[cfg(not(any(target_os = "macos", windows)))]
impl SystemPlayer {
    pub fn open(&mut self, _req: &PlayRequest) -> anyhow::Result<()> {
        anyhow::bail!("no system player on this platform; set player.backend = \"mpv\"")
    }
}
