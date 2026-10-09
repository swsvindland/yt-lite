//! AVPlayer in an AVKit `AVPlayerView` window: native controls, fullscreen,
//! Picture in Picture, hardware decoding, and HLS out of the box.

use std::cell::Cell;

use anyhow::{Result, anyhow};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowDelegate, NSWindowStyleMask};
use objc2_av_foundation::{AVPlayer, AVPlayerItem, AVPlayerItemStatus};
use objc2_av_kit::{AVPlayerView, AVPlayerViewControlsStyle};
use objc2_core_foundation::CGSize;
use objc2_core_media::CMTime;
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};

use super::PlayRequest;
use crate::player::Position;

/// Playback position and duration, in seconds.
type Times = (f64, Option<f64>);

struct DelegateIvars {
    player: Retained<AVPlayer>,
    /// Where the video was when the window closed, until `poll` collects it.
    closed_at: Cell<Option<Times>>,
}

define_class!(
    // Stops playback and frees the stream buffers when the window closes.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "YtLitePlayerWindowDelegate"]
    #[ivars = DelegateIvars]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            let ivars = self.ivars();
            ivars.closed_at.set(times(&ivars.player));
            unsafe {
                ivars.player.pause();
                ivars.player.replaceCurrentItemWithPlayerItem(None);
            }
            log::info!("player window closed");
        }
    }
);

impl WindowDelegate {
    fn new(mtm: MainThreadMarker, player: Retained<AVPlayer>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            player,
            closed_at: Cell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }
}

struct PlayerWindow {
    window: Retained<NSWindow>,
    player: Retained<AVPlayer>,
    _view: Retained<AVPlayerView>,
    delegate: Retained<WindowDelegate>,
}

/// One reusable player window. Main thread only.
#[derive(Default)]
pub struct SystemPlayer {
    current: Option<PlayerWindow>,
    /// The video in the window, until its final position is reported.
    video_id: Option<String>,
    /// Final positions of videos the window moved on from.
    finished: Vec<Position>,
}

impl SystemPlayer {
    pub fn open(&mut self, req: &PlayRequest) -> Result<()> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow!("player must open on the main thread"))?;
        let url = NSURL::URLWithString(&NSString::from_str(&req.url))
            .ok_or_else(|| anyhow!("invalid stream URL"))?;
        let item = unsafe { AVPlayerItem::playerItemWithURL(&url, mtm) };
        // Cap the HLS variant AVPlayer may pick. Width allows up to 2:1 frames.
        let tier = f64::from(req.max_tier);
        unsafe { item.setPreferredMaximumResolution(CGSize::new(tier * 2.0, tier)) };

        let title = NSString::from_str(&req.title);
        self.finish_current();
        self.video_id = Some(req.video_id.clone());
        if let Some(w) = &self.current {
            unsafe { w.player.replaceCurrentItemWithPlayerItem(Some(&item)) };
            seek(&w.player, req.start_secs);
            w.window.setTitle(&title);
            w.window.makeKeyAndOrderFront(None);
            unsafe { w.player.play() };
            return Ok(());
        }

        let player = unsafe { AVPlayer::playerWithPlayerItem(Some(&item), mtm) };
        let view = unsafe { AVPlayerView::new(mtm) };
        unsafe {
            view.setPlayer(Some(&player));
            view.setControlsStyle(AVPlayerViewControlsStyle::Floating);
            view.setAllowsPictureInPicturePlayback(true);
            view.setShowsFullScreenToggleButton(true);
        }

        let (w, h) = window_size(req.aspect);
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        let delegate = WindowDelegate::new(mtm, player.clone());
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&title);
        window.setContentView(Some(&view));
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        window.setContentAspectRatio(NSSize::new(w, h));
        window.center();
        window.makeKeyAndOrderFront(None);
        seek(&player, req.start_secs);
        unsafe { player.play() };

        self.current = Some(PlayerWindow {
            window,
            player,
            _view: view,
            delegate,
        });
        Ok(())
    }

    /// Where playback got to since the last call: the open video's position,
    /// and the final positions of videos that were closed or replaced.
    pub fn poll(&mut self) -> Vec<Position> {
        let mut out = std::mem::take(&mut self.finished);
        let (Some(w), Some(id)) = (&self.current, &self.video_id) else {
            return out;
        };
        if let Some((secs, duration)) = w.delegate.ivars().closed_at.take() {
            out.push(Position {
                video_id: id.clone(),
                secs,
                duration,
                done: true,
            });
            self.video_id = None;
        } else if let Some((secs, duration)) = times(&w.player) {
            out.push(Position {
                video_id: id.clone(),
                secs,
                duration,
                done: false,
            });
        }
        out
    }

    /// Keeps the final position of the video in the window before another
    /// replaces it.
    fn finish_current(&mut self) {
        let (Some(w), Some(video_id)) = (&self.current, self.video_id.take()) else {
            return;
        };
        let closed = w.delegate.ivars().closed_at.take();
        if let Some((secs, duration)) = closed.or_else(|| times(&w.player)) {
            self.finished.push(Position {
                video_id,
                secs,
                duration,
                done: true,
            });
        }
    }
}

/// Position and duration of the player's item once it's ready to play
/// (before that its time is 0, which would erase the resume point).
fn times(player: &AVPlayer) -> Option<Times> {
    let item = unsafe { player.currentItem() }?;
    if unsafe { item.status() } != AVPlayerItemStatus::ReadyToPlay {
        return None;
    }
    let secs = unsafe { player.currentTime().seconds() };
    let duration = unsafe { item.duration().seconds() };
    secs.is_finite().then_some((
        secs,
        (duration.is_finite() && duration > 0.0).then_some(duration),
    ))
}

fn seek(player: &AVPlayer, secs: f64) {
    if secs > 0.0 {
        unsafe { player.seekToTime(CMTime::with_seconds(secs, 600)) };
    }
}

/// 1280 wide at the video's aspect ratio (16:9 if unknown).
fn window_size(aspect: Option<f64>) -> (f64, f64) {
    let aspect = aspect
        .filter(|a| a.is_finite() && *a > 0.2 && *a < 5.0)
        .unwrap_or(16.0 / 9.0);
    if aspect >= 1.0 {
        (1280.0, (1280.0 / aspect).round())
    } else {
        ((800.0 * aspect).round(), 800.0)
    }
}
