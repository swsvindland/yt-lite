//! Resolve a video with the native resolver and print the chosen streams.
//!
//!     cargo run -p yt-lite-core --example resolve -- <video id or URL> [max_tier]
//!
//! Useful when YouTube changes something: if this fails, playback falls back
//! to yt-dlp.

use yt_lite_core::net::Http;
use yt_lite_core::resolve::innertube::InnertubeResolver;
use yt_lite_core::resolve::{Prefs, StreamResolver};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("usage: resolve <video id or URL> [max_tier]");
    let id = yt_lite_core::youtube::parse_video_id(&input).unwrap_or(input);
    let prefs = Prefs {
        max_tier: args.next().map(|s| s.parse()).transpose()?.unwrap_or(1080),
        ..Prefs::default()
    };
    let resolver = InnertubeResolver::new(Http::new(), None);
    let t = std::time::Instant::now();
    let r = resolver.resolve(&id, &prefs)?;
    println!("{} ({}) resolved in {:?}", r.title, r.video_id, t.elapsed());
    println!("live: {}  expires_in: {:?}", r.is_live, r.expires_in);
    for (kind, s) in [("video", &r.video), ("audio", &r.audio)] {
        match s {
            Some(s) => println!(
                "{kind}: itag {} {} {} {:?}p {:?}fps {} kbps\n  {}",
                s.itag, s.mime, s.codecs, s.tier, s.fps, s.bitrate / 1000, s.url
            ),
            None => println!("{kind}: none"),
        }
    }
    if let Some(h) = &r.hls {
        println!("hls: {h}");
    }
    Ok(())
}
