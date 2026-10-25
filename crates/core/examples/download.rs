//! Downloads a video the way the apps do (H.264 + AAC, remuxed into one
//! MP4), without touching the app's database. `audio` instead of a height
//! downloads just the sound (an .m4a).
//!
//!     cargo run -p yt-lite-core --example download -- <video id or URL> [max_height|audio] [out dir]

use std::path::PathBuf;

use yt_lite_core::net::Http;
use yt_lite_core::offline::{fetch, mux};
use yt_lite_core::resolve::innertube::{InnertubeResolver, VISIONOS};
use yt_lite_core::resolve::{Prefs, StreamResolver};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .expect("usage: download <video id or URL> [max_height|audio] [out dir]");
    let id = yt_lite_core::youtube::parse_video_id(&input).unwrap_or(input);
    let quality = args.next().unwrap_or_else(|| "720".into());
    let audio_only = quality == "audio";
    let max_height: u32 = if audio_only { 720 } else { quality.parse()? };
    let dir = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&dir)?;
    let http = Http::new();
    let prefs = Prefs {
        max_tier: max_height,
        codecs: vec!["avc1".into()],
        only_codecs: true,
        audio_mime: Some("audio/mp4".into()),
    };
    let r = InnertubeResolver::new(http.clone(), None).resolve(&id, &prefs)?;
    println!("{} ({id})", r.title);
    let mut parts = vec![];
    let video = if audio_only { None } else { Some(r.video) };
    for (kind, stream) in video
        .map(|v| ("video", v))
        .into_iter()
        .chain([("audio", r.audio)])
    {
        let s = stream.ok_or_else(|| anyhow::anyhow!("no {kind} stream"))?;
        let len = s
            .content_length
            .ok_or_else(|| anyhow::anyhow!("no length"))?;
        println!(
            "{kind}: itag {} {} {:?}p {} bytes",
            s.itag, s.codecs, s.tier, len
        );
        let path = dir.join(format!("{id}.{kind}.part"));
        let t = std::time::Instant::now();
        fetch::fetch(
            &http,
            &s.url,
            VISIONOS.user_agent,
            &path,
            len,
            &|| false,
            &mut |_| {},
        )?;
        let secs = t.elapsed().as_secs_f64();
        println!("  {:.1} s, {:.1} MB/s", secs, len as f64 / secs / 1e6);
        parts.push(path);
    }
    let out = dir.join(format!("{id}.{}", if audio_only { "m4a" } else { "mp4" }));
    let t = std::time::Instant::now();
    let refs: Vec<&std::path::Path> = parts.iter().map(|p| p.as_path()).collect();
    let muxed = mux::mux(&refs, &out)?;
    println!(
        "muxed {} bytes, {:.1} s long, in {:?}: {}",
        muxed.bytes,
        muxed.duration_secs,
        t.elapsed(),
        out.display()
    );
    Ok(())
}
