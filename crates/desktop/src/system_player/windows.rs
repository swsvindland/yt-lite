//! Windows: the built-in WinRT MediaPlayer (see the `yt-lite-winplayer` crate).

use anyhow::Result;
use yt_lite_winplayer::Times;

use super::PlayRequest;
use crate::player::Position;

/// The player window is owned by `yt-lite-winplayer` on the UI thread.
#[derive(Default)]
pub struct SystemPlayer {
    /// The video in the window, until its final position is reported.
    video_id: Option<String>,
    /// Final positions of videos the window moved on from.
    finished: Vec<Position>,
}

impl SystemPlayer {
    pub fn open(&mut self, req: &PlayRequest) -> Result<()> {
        self.finish_current();
        // HLS variant selection is left to MediaPlayer's adaptive streaming.
        yt_lite_winplayer::open(&req.url, &req.title, req.aspect, req.start_secs)?;
        self.video_id = Some(req.video_id.clone());
        Ok(())
    }

    /// Where playback got to since the last call: the open video's position,
    /// and the final positions of videos that were closed or replaced.
    pub fn poll(&mut self) -> Vec<Position> {
        let mut out = std::mem::take(&mut self.finished);
        let Some(id) = &self.video_id else {
            return out;
        };
        if let Some(t) = yt_lite_winplayer::take_closed() {
            out.push(position(id.clone(), t, true));
            self.video_id = None;
        } else if let Some(t) = yt_lite_winplayer::position() {
            out.push(position(id.clone(), t, false));
        }
        out
    }

    /// Keeps the final position of the video in the window before another
    /// replaces it.
    fn finish_current(&mut self) {
        let Some(id) = self.video_id.take() else {
            return;
        };
        if let Some(t) = yt_lite_winplayer::take_closed().or_else(yt_lite_winplayer::position) {
            self.finished.push(position(id, t, true));
        }
    }
}

fn position(video_id: String, t: Times, done: bool) -> Position {
    Position {
        video_id,
        secs: t.position,
        duration: t.duration,
        done,
    }
}
