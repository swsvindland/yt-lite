//! Status bar memory readout. Its own entity so the 2 s tick only re-renders
//! this small view.

use std::time::Duration;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::mem;

pub struct MemoryIndicator {
    text: SharedString,
    _tick: Task<()>,
}

impl MemoryIndicator {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let tick = cx.spawn(async move |this, cx| {
            let mut n: u32 = 0;
            loop {
                let Some(m) = mem::current() else { break };
                let text = format!("RSS {:.0} MB", mem::mb(m.physical));
                if this.update(cx, |v, cx| {
                    v.text = text.into();
                    cx.notify();
                })
                .is_err()
                {
                    break;
                }
                // Also log every 5 minutes for headless verification.
                if n % 150 == 0 {
                    mem::log_now("periodic");
                }
                n = n.wrapping_add(1);
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        });
        Self {
            text: "".into(),
            _tick: tick,
        }
    }
}

impl Render for MemoryIndicator {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(self.text.clone())
    }
}
