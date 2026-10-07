//! Playback via an external mpv process.
//!
//! Streams are resolved natively (`yt_lite_core::resolve`, ~0.2 s) and handed
//! to mpv as direct URLs. If that fails (YouTube changed something, age gate,
//! ...), mpv is started on the watch URL with yt-dlp as its resolver instead.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use yt_lite_core::resolve::{Prefs, Resolved, StreamResolver};
use yt_lite_core::youtube::watch_url;

use crate::config::{Paths, PlayerConfig};

/// googlevideo throttles open-ended requests to roughly real-time after a
/// short burst; bounded 10 MiB range requests run at full speed. FFmpeg's
/// `request_size` makes mpv fetch in such chunks (mpv's ytdl_hook does the same).
const HTTP_CHUNK_SIZE: u64 = 10 * 1024 * 1024;

#[derive(Clone)]
pub struct Player {
    mpv: Option<PathBuf>,
    ytdlp: Option<PathBuf>,
    sponsorblock_script: Option<PathBuf>,
    prefs: Prefs,
    extra_args: Vec<String>,
    native: Option<Arc<dyn StreamResolver>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Native,
    YtDlp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    Mpv,
    YtDlp,
}

impl Missing {
    pub fn install_hint(&self) -> &'static str {
        match self {
            Missing::Mpv => {
                "mpv was not found. Install it with `winget install shinchiro.mpv` \
                 (or `scoop bucket add extras; scoop install extras/mpv`; on macOS \
                 `brew install mpv`), or set player.mpv_path in config.toml."
            }
            Missing::YtDlp => {
                "yt-dlp was not found. Install it with `winget install yt-dlp.yt-dlp` \
                 (or `scoop install yt-dlp`; on macOS `brew install yt-dlp`), \
                 or set player.ytdlp_path in config.toml."
            }
        }
    }
}

impl Player {
    /// `native` is `None` when `player.resolver = "yt-dlp"`.
    pub fn detect(cfg: &PlayerConfig, paths: &Paths, native: Option<Arc<dyn StreamResolver>>) -> Self {
        let sponsorblock_script = if cfg.sponsorblock {
            let p = if cfg.sponsorblock_script.trim().is_empty() {
                paths.default_sponsorblock_script()
            } else {
                PathBuf::from(cfg.sponsorblock_script.trim())
            };
            if p.is_file() {
                Some(p)
            } else {
                log::warn!("sponsorblock enabled but script not found at {}", p.display());
                None
            }
        } else {
            None
        };
        let me = Self {
            mpv: find_executable(&cfg.mpv_path),
            ytdlp: find_executable(&cfg.ytdlp_path),
            sponsorblock_script,
            prefs: Prefs {
                max_tier: cfg.max_height,
                codecs: cfg.codecs.clone(),
            },
            extra_args: cfg.extra_args.clone(),
            native,
        };
        log::info!(
            "player: mpv={:?} yt-dlp={:?} native={:?} sponsorblock={:?}",
            me.mpv,
            me.ytdlp,
            me.native.as_ref().map(|n| n.name()),
            me.sponsorblock_script
        );
        me
    }

    fn common_args(&self, title: &str) -> Vec<String> {
        let mut args = vec![
            "--force-window=immediate".into(),
            format!("--force-media-title={title}"),
        ];
        if let Some(script) = &self.sponsorblock_script {
            args.push(format!("--script={}", script.display()));
        }
        args
    }

    /// mpv arguments for natively resolved streams.
    pub fn native_args(&self, r: &Resolved, title: &str) -> Vec<String> {
        let mut args = self.common_args(title);
        args.extend([
            // Direct URLs: don't let the ytdl hook re-resolve them.
            "--ytdl=no".into(),
            format!("--stream-lavf-o-append=request_size={HTTP_CHUNK_SIZE}"),
            // Lets the SponsorBlock script (which looks for a YouTube URL in
            // the path or Referer) find the video id.
            format!("--http-header-fields=Referer: {}", watch_url(&r.video_id)),
        ]);
        args.extend(self.extra_args.iter().cloned());
        match (&r.video, &r.hls) {
            (Some(video), _) if !r.is_live => {
                if let Some(audio) = &r.audio {
                    args.push(format!("--audio-file={}", audio.url));
                }
                args.push(video.url.clone());
            }
            (_, Some(hls)) => {
                // Live: the HLS master playlist; mpv picks the top variant.
                args.push("--hls-bitrate=max".into());
                args.push(hls.clone());
            }
            _ => {}
        }
        args
    }

    /// mpv arguments for the yt-dlp path.
    pub fn ytdlp_args(&self, video_id: &str, title: &str) -> Vec<String> {
        let mut args = self.common_args(title);
        args.push(format!(
            "--ytdl-format=bestvideo[height<=?{h}]+bestaudio/best[height<=?{h}]/best",
            h = self.prefs.max_tier
        ));
        if let Some(ytdlp) = &self.ytdlp {
            args.push(format!("--script-opts=ytdl_hook-ytdl_path={}", ytdlp.display()));
        }
        args.extend(self.extra_args.iter().cloned());
        args.push(watch_url(video_id));
        args
    }

    /// Blocking (the native resolve is a network call). Starts mpv detached.
    pub fn play(&self, video_id: &str, title: &str) -> Result<Method> {
        let mpv = self
            .mpv
            .as_ref()
            .ok_or_else(|| anyhow!(Missing::Mpv.install_hint()))?;
        let mut native_err = None;
        if let Some(native) = &self.native {
            let t = std::time::Instant::now();
            match native.resolve(video_id, &self.prefs) {
                Ok(r) => {
                    log::info!(
                        "resolved {video_id} natively in {:?}: video {:?} audio {:?} live {}",
                        t.elapsed(),
                        r.video.as_ref().map(|v| (v.itag, v.tier, v.codecs.as_str())),
                        r.audio.as_ref().map(|a| (a.itag, a.codecs.as_str())),
                        r.is_live
                    );
                    spawn(mpv, self.native_args(&r, title))?;
                    return Ok(Method::Native);
                }
                Err(e) => {
                    log::warn!("native resolve failed for {video_id}, falling back to yt-dlp: {e:#}");
                    native_err = Some(e);
                }
            }
        }
        if self.ytdlp.is_none() {
            return Err(match native_err {
                Some(e) => anyhow!("{e:#}\n\n{}", Missing::YtDlp.install_hint()),
                None => anyhow!(Missing::YtDlp.install_hint()),
            });
        }
        spawn(mpv, self.ytdlp_args(video_id, title))?;
        Ok(Method::YtDlp)
    }
}

fn spawn(mpv: &Path, args: Vec<String>) -> Result<()> {
    let mut cmd = Command::new(mpv);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let child = cmd
        .spawn()
        .map_err(|e| anyhow!("failed to start {}: {e}", mpv.display()))?;
    log::info!("mpv pid {}", child.id());
    // Reap the child in the background so it doesn't linger as a zombie on Unix.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// Resolves `name_or_path` to an existing executable: an explicit path is
/// checked directly; a bare name is searched for on PATH (with PATHEXT on
/// Windows). Also checks common Windows install locations (scoop, winget).
pub fn find_executable(name_or_path: &str) -> Option<PathBuf> {
    let name_or_path = name_or_path.trim();
    if name_or_path.is_empty() {
        return None;
    }
    let p = Path::new(name_or_path);
    if p.components().count() > 1 || p.is_absolute() {
        return with_exe_variants(p).into_iter().find(|c| c.is_file());
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default();
    dirs.extend(extra_search_dirs());
    dirs.iter()
        .flat_map(|d| with_exe_variants(&d.join(name_or_path)))
        .find(|c| c.is_file())
}

fn with_exe_variants(p: &Path) -> Vec<PathBuf> {
    let mut out = vec![p.to_path_buf()];
    if cfg!(windows) && p.extension().is_none() {
        // `.exe` first: mpv ships both mpv.exe (GUI) and mpv.com (console
        // wrapper), and the default PATHEXT lists .COM before .EXE.
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        let exts = std::iter::once(".exe").chain(
            exts.split(';')
                .filter(|e| !e.is_empty() && !e.eq_ignore_ascii_case(".exe")),
        );
        for ext in exts {
            let mut s = p.as_os_str().to_owned();
            s.push(ext.to_ascii_lowercase());
            out.push(PathBuf::from(s));
        }
    }
    out
}

fn extra_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            dirs.push(PathBuf::from(&home).join("scoop").join("shims"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(&local).join("Microsoft").join("WinGet").join("Links"));
        }
    } else {
        dirs.push("/opt/homebrew/bin".into());
        dirs.push("/usr/local/bin".into());
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use yt_lite_core::resolve::Stream;

    fn player() -> Player {
        Player {
            mpv: Some("mpv".into()),
            ytdlp: Some("/bin/yt-dlp".into()),
            sponsorblock_script: Some("/s/sponsorblock.lua".into()),
            prefs: Prefs {
                max_tier: 720,
                ..Prefs::default()
            },
            extra_args: vec!["--volume=50".into()],
            native: None,
        }
    }

    fn stream(url: &str) -> Stream {
        Stream {
            itag: 1,
            url: url.into(),
            mime: "video/mp4".into(),
            codecs: "avc1".into(),
            width: None,
            height: None,
            tier: Some(720),
            fps: None,
            bitrate: 0,
            content_length: None,
        }
    }

    #[test]
    fn ytdlp_args_include_quality_and_ytdl_path() {
        let args = player().ytdlp_args("abcdefghijk", "T");
        assert!(args.iter().any(|a| a.contains("height<=?720")));
        assert!(args.contains(&"--script-opts=ytdl_hook-ytdl_path=/bin/yt-dlp".to_string()));
        assert!(args.contains(&"--script=/s/sponsorblock.lua".to_string()));
        assert!(args.contains(&"--volume=50".to_string()));
        assert_eq!(args.last().unwrap(), "https://www.youtube.com/watch?v=abcdefghijk");
    }

    #[test]
    fn native_args_use_direct_urls_chunking_and_referer() {
        let r = Resolved {
            video_id: "abcdefghijk".into(),
            title: "T".into(),
            is_live: false,
            video: Some(stream("https://v.example/video")),
            audio: Some(stream("https://v.example/audio")),
            hls: Some("https://m.example/hls.m3u8".into()),
            expires_in: None,
        };
        let args = player().native_args(&r, "T");
        assert!(args.contains(&"--ytdl=no".to_string()));
        assert!(args.contains(&"--stream-lavf-o-append=request_size=10485760".to_string()));
        assert!(args.contains(
            &"--http-header-fields=Referer: https://www.youtube.com/watch?v=abcdefghijk".to_string()
        ));
        assert!(args.contains(&"--audio-file=https://v.example/audio".to_string()));
        assert_eq!(args.last().unwrap(), "https://v.example/video");
        assert!(!args.iter().any(|a| a.contains("hls")));
    }

    #[test]
    fn live_uses_hls() {
        let r = Resolved {
            video_id: "abcdefghijk".into(),
            title: "T".into(),
            is_live: true,
            video: None,
            audio: None,
            hls: Some("https://m.example/hls.m3u8".into()),
            expires_in: None,
        };
        let args = player().native_args(&r, "T");
        assert_eq!(args.last().unwrap(), "https://m.example/hls.m3u8");
        assert!(!args.iter().any(|a| a.starts_with("--audio-file")));
    }

    #[test]
    fn missing_mpv_is_reported_first() {
        let mut p = player();
        p.mpv = None;
        let err = p.play("abcdefghijk", "T").unwrap_err().to_string();
        assert!(err.contains("mpv was not found"));
    }

    #[test]
    fn no_native_and_no_ytdlp_reports_ytdlp() {
        let mut p = player();
        p.ytdlp = None;
        let err = p.play("abcdefghijk", "T").unwrap_err().to_string();
        assert!(err.contains("yt-dlp was not found"));
    }
}
