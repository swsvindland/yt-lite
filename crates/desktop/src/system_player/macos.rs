//! AVPlayer in an AVKit `AVPlayerView` window: native controls, fullscreen,
//! Picture in Picture, hardware decoding, and HLS out of the box.

use anyhow::{Result, anyhow};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowDelegate, NSWindowStyleMask};
use objc2_av_foundation::{AVPlayer, AVPlayerItem};
use objc2_av_kit::{AVPlayerView, AVPlayerViewControlsStyle};
use objc2_core_foundation::CGSize;
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};

use super::PlayRequest;

define_class!(
    // Stops playback and frees the stream buffers when the window closes.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "YtLitePlayerWindowDelegate"]
    #[ivars = Retained<AVPlayer>]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            let player = self.ivars();
            unsafe {
                player.pause();
                player.replaceCurrentItemWithPlayerItem(None);
            }
            log::info!("player window closed");
        }
    }
);

impl WindowDelegate {
    fn new(mtm: MainThreadMarker, player: Retained<AVPlayer>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(player);
        unsafe { msg_send![super(this), init] }
    }
}

struct PlayerWindow {
    window: Retained<NSWindow>,
    player: Retained<AVPlayer>,
    _view: Retained<AVPlayerView>,
    _delegate: Retained<WindowDelegate>,
}

/// One reusable player window. Main thread only.
#[derive(Default)]
pub struct SystemPlayer {
    current: Option<PlayerWindow>,
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
        if let Some(w) = &self.current {
            unsafe { w.player.replaceCurrentItemWithPlayerItem(Some(&item)) };
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
        unsafe { player.play() };

        self.current = Some(PlayerWindow {
            window,
            player,
            _view: view,
            _delegate: delegate,
        });
        Ok(())
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
