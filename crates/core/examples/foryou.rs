//! Checks For you's ranking against your own history: it learns from what
//! you watched before the last few days, then sees where the channels and
//! videos you went on to watch land, next to the old order (a round-robin of
//! the 10 latest videos' related lists). Point it at a copy of an app
//! database; it only reads it, and fetches related videos from YouTube.
//!
//!     cargo run -p yt-lite-core --example foryou -- <copy of cache.sqlite3> [holdout days]

use std::collections::HashSet;
use std::path::PathBuf;

use yt_lite_core::db::Db;
use yt_lite_core::feed::FeedItem;
use yt_lite_core::feed::interest::{self, Related, Taste, Watched};
use yt_lite_core::net::{Http, parallel_map};
use yt_lite_core::youtube::related;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .expect("usage: foryou <cache.sqlite3> [holdout days]"),
    );
    let days: f64 = args.next().map(|d| d.parse()).transpose()?.unwrap_or(3.0);
    let db = Db::open(&path)?;
    let cutoff = chrono::Utc::now().timestamp() - (days * 86_400.0) as i64;
    let history = db.watch_history(5000)?;
    let (after, before): (Vec<Watched>, Vec<Watched>) =
        history.into_iter().partition(|w| w.at >= cutoff);
    let after: Vec<&Watched> = after.iter().filter(|w| w.played).collect();
    let watched_ids: HashSet<&str> = after.iter().map(|w| w.id.as_str()).collect();
    let watched_channels: HashSet<&str> = after.iter().map(|w| w.channel_id.as_str()).collect();
    println!(
        "learning from {} videos; {} played in the last {days} days, from {} channels",
        before.len(),
        after.len(),
        watched_channels.len()
    );

    let http = Http::new();
    let fetch = |ids: Vec<(String, f64)>| -> Vec<Related> {
        parallel_map(&ids, 4, |(id, weight)| Related {
            weight: *weight,
            items: related::fetch(&http, id).unwrap_or_default(),
        })
    };
    let mut skip: HashSet<String> = before.iter().map(|w| w.id.clone()).collect();

    let seeds = interest::seeds(&before, 10, cutoff);
    skip.extend(seeds.iter().map(|s| s.0.clone()));
    let subscribed = db
        .subscribed_channels()?
        .into_iter()
        .map(|c| c.channel_id)
        .collect();
    let taste = Taste::new(&before, subscribed, cutoff);
    let ranked = interest::rank(fetch(seeds.clone()), &skip, &taste, cutoff, 150);

    let old_seeds: Vec<(String, f64)> = before
        .iter()
        .filter(|w| w.played)
        .take(10)
        .map(|w| (w.id.clone(), 1.0))
        .collect();
    skip.extend(old_seeds.iter().map(|s| s.0.clone()));
    let old = round_robin(fetch(old_seeds), &skip, 150);

    for (name, list) in [("before (round-robin)", &old), ("ranked", &ranked)] {
        let share = |k: usize| {
            let top = &list[..k.min(list.len())];
            let hits = top
                .iter()
                .filter(|i| watched_channels.contains(i.channel_id.as_str()))
                .count();
            format!("{hits}/{}", top.len())
        };
        let videos: Vec<usize> = list
            .iter()
            .enumerate()
            .filter(|(_, i)| watched_ids.contains(i.video_id.as_str()))
            .map(|(p, _)| p + 1)
            .collect();
        let channels: HashSet<&str> = list[..25.min(list.len())]
            .iter()
            .map(|i| i.channel_id.as_str())
            .collect();
        println!(
            "\n{name}: channels you then watched in the top 10 {}, 25 {}, 50 {}; \
             videos you then watched at {videos:?}; {} channels in the top 25",
            share(10),
            share(25),
            share(50),
            channels.len()
        );
        for (p, i) in list.iter().take(10).enumerate() {
            let mark = if watched_channels.contains(i.channel_id.as_str()) {
                "*"
            } else {
                " "
            };
            println!(
                "  {:>2}{mark} {} — {}",
                p + 1,
                i.channel_title,
                i.title.chars().take(60).collect::<String>()
            );
        }
    }
    Ok(())
}

/// The order For you had before ranking.
fn round_robin(lists: Vec<Related>, skip: &HashSet<String>, max: usize) -> Vec<FeedItem> {
    let mut seen = HashSet::new();
    let mut iters: Vec<_> = lists.into_iter().map(|l| l.items.into_iter()).collect();
    let mut out = Vec::new();
    loop {
        let mut progressed = false;
        for it in iters.iter_mut() {
            if let Some(item) = it.next() {
                progressed = true;
                if !skip.contains(&item.video_id) && seen.insert(item.video_id.clone()) {
                    out.push(item);
                    if out.len() >= max {
                        return out;
                    }
                }
            }
        }
        if !progressed {
            return out;
        }
    }
}
