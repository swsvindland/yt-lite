//! Windows: the built-in WinRT MediaPlayer (see the `yt-lite-winplayer` crate).

use anyhow::Result;

use super::PlayRequest;

/// The player window is owned by `yt-lite-winplayer` on the UI thread.
#[derive(Default)]
pub struct SystemPlayer;

impl SystemPlayer {
    pub fn open(&mut self, req: &PlayRequest) -> Result<()> {
        // HLS variant selection is left to MediaPlayer's adaptive streaming.
        yt_lite_winplayer::open(&req.url, &req.title, req.aspect)
    }
}
