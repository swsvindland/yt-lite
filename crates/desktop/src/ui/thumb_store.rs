//! One thumbnail cache for every view, so the memory cap is global.
//!
//! Views call [`ThumbStore::get`] while rendering; missing thumbnails load in
//! the background (disk cache, else network) and the store notifies observers
//! when they arrive. Evicted images are also released from GPUI's sprite
//! atlas. After the window has been inactive for a minute, everything decoded
//! is released.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;
use smol::lock::Semaphore;

use crate::thumbs::{self, Thumbnails};
use yt_lite_core::net::Http;

const MAX_PARALLEL: usize = 8;
const IDLE_TRIM_AFTER: Duration = Duration::from_secs(60);

pub struct ThumbStore {
    thumbs: Thumbnails,
    http: Http,
    permits: Arc<Semaphore>,
    idle_trim: Option<Task<()>>,
    _activation: Subscription,
}

impl ThumbStore {
    pub fn new(
        thumbs: Thumbnails,
        http: Http,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.idle_trim = None;
            } else {
                this.idle_trim = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(IDLE_TRIM_AFTER).await;
                    this.update(cx, |t, cx| t.trim(cx)).ok();
                }));
            }
        });
        Self {
            thumbs,
            http,
            permits: Arc::new(Semaphore::new(MAX_PARALLEL)),
            idle_trim: None,
            _activation: activation,
        }
    }

    /// The decoded thumbnail, or `None` while it loads. `scale` is the window
    /// scale factor; images are decoded at the physical display size.
    pub fn get(
        &mut self,
        id: &SharedString,
        (w, h): (f32, f32),
        scale: f32,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        if let Some(img) = self.thumbs.get(id) {
            return Some(img);
        }
        if self.thumbs.begin_load(id) {
            let http = self.http.clone();
            let dir = self.thumbs.dir().to_path_buf();
            let permits = self.permits.clone();
            let (pw, ph) = ((w * scale) as u32, (h * scale) as u32);
            let id = id.clone();
            cx.spawn(async move |this, cx| {
                let _permit = permits.acquire_arc().await;
                let video_id = id.to_string();
                let result =
                    smol::unblock(move || thumbs::load(&http, &dir, &video_id, pw, ph)).await;
                this.update(cx, |t, cx| {
                    for img in t.thumbs.finish(id, result) {
                        cx.drop_image(img, None);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        None
    }

    pub fn clear_failures(&mut self) {
        self.thumbs.clear_failures();
    }

    pub fn len(&self) -> usize {
        self.thumbs.len()
    }

    pub fn memory_bytes(&self) -> usize {
        self.thumbs.memory_bytes()
    }

    /// Releases every decoded thumbnail (CPU buffers and GPU atlas tiles).
    pub fn trim(&mut self, cx: &mut Context<Self>) {
        let images = self.thumbs.clear();
        let n = images.len();
        for img in images {
            cx.drop_image(img, None);
        }
        log::info!("idle: released {n} thumbnails");
        cx.notify();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            crate::mem::log_now("after idle trim");
        })
        .detach();
    }
}
