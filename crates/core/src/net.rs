//! HTTP client.
//!
//! GPUI has its own executors and is not tokio-based. Rather than run a tokio
//! runtime on a side thread for reqwest, we use `ureq` (blocking, small, rustls)
//! and run calls on smol's blocking thread pool via `smol::unblock`, which returns
//! a plain future that GPUI tasks can `.await`. No extra runtime, and threads
//! only exist while requests are in flight.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::de::DeserializeOwned;
use ureq::Agent;

use crate::shorts::{self, Verdict};


const USER_AGENT: &str = concat!("yt-lite/", env!("CARGO_PKG_VERSION"));
/// Browser-like UA for youtube.com pages (RSS, Shorts probe).
const BROWSER_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36";
/// Pre-accepts the EU consent interstitial so youtube.com doesn't redirect to
/// consent.youtube.com (same cookie yt-dlp uses).
const CONSENT_COOKIE: &str = "SOCS=CAI";

#[derive(Clone)]
pub struct Http {
    agent: Agent,
}

impl Http {
    pub fn new() -> Self {
        let agent: Agent = Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent }
    }

    /// GET a youtube.com page/feed as text.
    pub fn get_youtube_text(&self, url: &str) -> Result<String> {
        let mut resp = self
            .agent
            .get(url)
            .header("User-Agent", BROWSER_UA)
            .header("Cookie", CONSENT_COOKIE)
            .call()?;
        let status = resp.status().as_u16();
        if status != 200 {
            bail!("GET {url}: HTTP {status}");
        }
        Ok(resp.body_mut().read_to_string()?)
    }

    pub fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let mut resp = self.agent.get(url).call()?;
        let status = resp.status().as_u16();
        if status != 200 {
            bail!("GET {url}: HTTP {status}");
        }
        Ok(resp.body_mut().with_config().limit(2 * 1024 * 1024).read_to_vec()?)
    }

    /// GET a Google API endpoint with a bearer token and decode JSON.
    pub fn get_json<T: DeserializeOwned>(&self, url: &str, access_token: &str) -> Result<T> {
        let mut resp = self
            .agent
            .get(url)
            .header("Authorization", &format!("Bearer {access_token}"))
            .call()?;
        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(ApiError { status, body }.into());
        }
        Ok(resp.body_mut().read_json()?)
    }

    pub fn post_form<T: DeserializeOwned>(&self, url: &str, form: &[(&str, &str)]) -> Result<T> {
        let mut resp = self.agent.post(url).send_form(form.iter().copied())?;
        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(ApiError { status, body }.into());
        }
        Ok(resp.body_mut().read_json()?)
    }

    /// Requests `youtube.com/shorts/<id>` without following redirects. A Short
    /// answers 200; anything else redirects to `/watch`. `Ok(None)` means the
    /// answer was inconclusive.
    pub fn probe_short(&self, video_id: &str) -> Result<Option<Verdict>> {
        let url = format!("https://www.youtube.com/shorts/{video_id}");
        let resp = self
            .agent
            .head(&url)
            .header("User-Agent", BROWSER_UA)
            .header("Cookie", CONSENT_COOKIE)
            .config()
            .max_redirects(0)
            .max_redirects_will_error(false)
            .build()
            .call()?;
        let status = resp.status().as_u16();
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok());
        let verdict = shorts::interpret_probe(status, location);
        if verdict.is_none() {
            log::warn!("shorts probe {video_id}: inconclusive (HTTP {status}, location {location:?})");
        }
        Ok(verdict)
    }
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub body: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Google errors are JSON: {"error": {"message": ...}} or {"error": "...", "error_description": ...}
        let msg = serde_json::from_str::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| {
                let e = v.get("error")?;
                e.get("message")
                    .and_then(|m| m.as_str())
                    .or_else(|| v.get("error_description").and_then(|m| m.as_str()))
                    .or_else(|| e.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| self.body.chars().take(200).collect());
        write!(f, "HTTP {}: {msg}", self.status)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    pub fn is_unauthorized(err: &anyhow::Error) -> bool {
        err.downcast_ref::<ApiError>()
            .is_some_and(|e| e.status == 401)
    }
}

/// Runs `f` over `items` with at most `concurrency` scoped threads.
pub fn parallel_map<T: Sync, R: Send>(
    items: &[T],
    concurrency: usize,
    f: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let concurrency = concurrency.clamp(1, 32);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<Option<R>>> =
        std::sync::Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..concurrency.min(items.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                let r = f(item);
                results.lock().unwrap()[i] = Some(r);
            });
        }
    });
    results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|r| r.ok_or_else(|| anyhow!("worker panicked")).unwrap())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_map_preserves_order() {
        let items: Vec<u32> = (0..100).collect();
        let out = parallel_map(&items, 7, |x| x * 2);
        assert_eq!(out, items.iter().map(|x| x * 2).collect::<Vec<_>>());
    }

    #[test]
    fn api_error_message_extraction() {
        let e = ApiError {
            status: 403,
            body: r#"{"error":{"code":403,"message":"quotaExceeded"}}"#.into(),
        };
        assert_eq!(e.to_string(), "HTTP 403: quotaExceeded");
        let e = ApiError {
            status: 400,
            body: r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#.into(),
        };
        assert_eq!(e.to_string(), "HTTP 400: Token has been expired or revoked.");
    }
}
