//! Refresh pipeline shared by every [`FeedSource`]:
//! fetch → cache → enrich durations (`videos.list`, 50 ids per call) →
//! classify Shorts (duration first, `/shorts/<id>` probe for borderline cases).
//!
//! All of it is blocking and runs off the UI thread.

use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;

use super::{FeedSource, Progress};
use crate::auth::{Auth, NotSignedIn};
use crate::config::Config;
use crate::db::{Db, MAX_ENRICH_ATTEMPTS};
use crate::net::{Http, parallel_map};
use crate::shorts::{self, Decision, LinkHint, Verdict};
use crate::youtube::api::Api;

/// Shared handles for sources and the pipeline. Cheap to share via `Arc`.
pub struct Services {
    pub http: Http,
    pub auth: Auth,
    pub db: Db,
    pub config: Config,
}

/// Bounds per refresh so a first run with many channels can't hammer YouTube.
const MAX_DETAILS_PER_REFRESH: u32 = 5000;
const MAX_PROBES_PER_REFRESH: usize = 400;
const PROBE_CONCURRENCY: usize = 6;

#[derive(Debug, Default, Clone)]
pub struct RefreshStats {
    pub fetched: usize,
    pub new: usize,
    pub enriched: usize,
    pub shorts: usize,
    pub probed: usize,
    pub inconclusive: usize,
}

impl std::fmt::Display for RefreshStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} fetched, {} new, {} enriched, {} shorts hidden, {} probed",
            self.fetched, self.new, self.enriched, self.shorts, self.probed
        )?;
        if self.inconclusive > 0 {
            write!(f, ", {} pending", self.inconclusive)?;
        }
        Ok(())
    }
}

pub fn refresh(source: &dyn FeedSource, s: &Services, progress: Progress) -> Result<RefreshStats> {
    let mut stats = RefreshStats::default();
    let fetched = source.fetch(s, progress)?;
    stats.fetched = fetched.items.len();
    let linked_shorts = fetched
        .items
        .iter()
        .filter(|i| i.link_hint == LinkHint::ShortsLink)
        .count();
    stats.new =
        s.db.upsert_items(source.kind(), &fetched.items, &fetched.retain)?;
    drop(fetched);

    let signed_in = match enrich(s, progress) {
        Ok(n) => {
            stats.enriched = n;
            true
        }
        Err(e) if e.downcast_ref::<NotSignedIn>().is_some() => false,
        Err(e) => {
            // Quota or network trouble: classification still works for videos
            // that already have durations; the rest stay hidden until next time.
            log::warn!("duration enrichment failed: {e:#}");
            true
        }
    };
    classify(s, signed_in, &mut stats, progress)?;
    // `/shorts/` links are marked at insert time; count them too.
    stats.shorts += linked_shorts;
    s.db.meta_set(
        &format!("last_refresh.{}", source.kind().as_str()),
        &chrono::Utc::now().timestamp().to_string(),
    )?;
    log::info!("refresh {}: {stats}", source.kind().as_str());
    Ok(stats)
}

fn enrich(s: &Services, progress: Progress) -> Result<usize> {
    let ids = s.db.ids_needing_details(MAX_DETAILS_PER_REFRESH)?;
    if ids.is_empty() {
        return Ok(0);
    }
    let api = Api {
        http: &s.http,
        auth: &s.auth,
    };
    let mut done = 0;
    for chunk in ids.chunks(50) {
        progress(format!("Fetching durations {done}/{}", ids.len()));
        let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
        let details = api.video_details(&refs)?;
        s.db.store_details(&refs, &details)?;
        done += chunk.len();
    }
    Ok(done)
}

fn classify(
    s: &Services,
    signed_in: bool,
    stats: &mut RefreshStats,
    progress: Progress,
) -> Result<()> {
    let pending = s.db.unclassified(20_000)?;
    let mut verdicts = Vec::new();
    let mut to_probe = Vec::new();
    for v in &pending {
        match shorts::classify(v.link_hint, v.duration) {
            Decision::Final(verdict) => verdicts.push((v.id.clone(), verdict)),
            Decision::NeedsProbe => to_probe.push(v.id.clone()),
            // Without API access, or once the API has given up on the id,
            // the probe is the only signal left.
            Decision::NeedsDuration if !signed_in || v.enrich_attempts >= MAX_ENRICH_ATTEMPTS => {
                to_probe.push(v.id.clone())
            }
            Decision::NeedsDuration => {}
        }
    }
    to_probe.truncate(MAX_PROBES_PER_REFRESH);

    if !to_probe.is_empty() {
        let total = to_probe.len();
        let done = AtomicUsize::new(0);
        let results = parallel_map(&to_probe, PROBE_CONCURRENCY, |id| {
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(10) || n == total {
                progress(format!("Checking for Shorts {n}/{total}"));
            }
            s.http.probe_short(id)
        });
        stats.probed = total;
        for (id, r) in to_probe.into_iter().zip(results) {
            match r {
                Ok(Some(v)) => verdicts.push((id, v)),
                Ok(None) => stats.inconclusive += 1,
                Err(e) => {
                    stats.inconclusive += 1;
                    log::warn!("shorts probe {id} failed: {e:#}");
                }
            }
        }
    }
    stats.shorts = verdicts
        .iter()
        .filter(|(_, v)| *v == Verdict::Short)
        .count();
    s.db.set_verdicts(&verdicts)?;
    Ok(())
}
