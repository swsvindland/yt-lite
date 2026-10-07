//! Playback via an external mpv process, using yt-dlp as its stream resolver.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, anyhow};

use crate::config::{Paths, PlayerConfig};

#[derive(Clone, Debug)]
pub struct Player {
    mpv: Option<PathBuf>,
    ytdlp: Option<PathBuf>,
    sponsorblock_script: Option<PathBuf>,
    max_height: u32,
    extra_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    Mpv,
    YtDlp,
    Both,
}

impl Missing {
    pub fn install_hint(&self) -> &'static str {
        match self {
            Missing::Mpv => {
                "mpv was not found. Install it with `winget install mpv` (or `scoop install mpv`), \
                 or set player.mpv_path in config.toml."
            }
            Missing::YtDlp => {
                "yt-dlp was not found. Install it with `winget install yt-dlp` (or `scoop install yt-dlp`), \
                 or set player.ytdlp_path in config.toml."
            }
            Missing::Both => {
                "mpv and yt-dlp were not found. Install them with `winget install mpv yt-dlp` \
                 (or `scoop install mpv yt-dlp`), or set player.mpv_path / player.ytdlp_path in config.toml."
            }
        }
    }
}

impl Player {
    pub fn detect(cfg: &PlayerConfig, paths: &Paths) -> Self {
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
            max_height: cfg.max_height,
            extra_args: cfg.extra_args.clone(),
        };
        log::info!(
            "player: mpv={:?} yt-dlp={:?} sponsorblock={:?}",
            me.mpv,
            me.ytdlp,
            me.sponsorblock_script
        );
        me
    }

    pub fn missing(&self) -> Option<Missing> {
        match (self.mpv.is_some(), self.ytdlp.is_some()) {
            (true, true) => None,
            (false, true) => Some(Missing::Mpv),
            (true, false) => Some(Missing::YtDlp),
            (false, false) => Some(Missing::Both),
        }
    }

    pub fn args(&self, url: &str, title: &str) -> Vec<String> {
        let mut args = vec![
            format!(
                "--ytdl-format=bestvideo[height<=?{h}]+bestaudio/best[height<=?{h}]/best",
                h = self.max_height
            ),
            "--force-window=immediate".into(),
            format!("--force-media-title={title}"),
        ];
        if let Some(ytdlp) = &self.ytdlp {
            args.push(format!(
                "--script-opts=ytdl_hook-ytdl_path={}",
                ytdlp.display()
            ));
        }
        if let Some(script) = &self.sponsorblock_script {
            args.push(format!("--script={}", script.display()));
        }
        args.extend(self.extra_args.iter().cloned());
        args.push(url.to_string());
        args
    }

    /// Spawns mpv detached; the app does not wait for it.
    pub fn play(&self, url: &str, title: &str) -> Result<()> {
        if let Some(missing) = self.missing() {
            return Err(anyhow!(missing.install_hint()));
        }
        let mpv = self.mpv.as_ref().expect("checked above");
        let mut cmd = Command::new(mpv);
        cmd.args(self.args(url, title))
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
        log::info!("mpv pid {} playing {url}", child.id());
        // Reap the child in the background so it doesn't linger as a zombie on Unix.
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });
        Ok(())
    }
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
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.COM;.BAT;.CMD".into());
        for ext in exts.split(';').filter(|e| !e.is_empty()) {
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

    #[test]
    fn args_include_quality_and_ytdl_path() {
        let player = Player {
            mpv: Some("mpv".into()),
            ytdlp: Some("/bin/yt-dlp".into()),
            sponsorblock_script: Some("/s/sponsorblock.lua".into()),
            max_height: 720,
            extra_args: vec!["--volume=50".into()],
        };
        let args = player.args("https://www.youtube.com/watch?v=x", "T");
        assert!(args[0].contains("height<=?720"));
        assert!(args.contains(&"--script-opts=ytdl_hook-ytdl_path=/bin/yt-dlp".to_string()));
        assert!(args.contains(&"--script=/s/sponsorblock.lua".to_string()));
        assert!(args.contains(&"--volume=50".to_string()));
        assert_eq!(args.last().unwrap(), "https://www.youtube.com/watch?v=x");
    }

    #[test]
    fn missing_reports_which_tool() {
        let mut p = Player {
            mpv: None,
            ytdlp: None,
            sponsorblock_script: None,
            max_height: 1080,
            extra_args: vec![],
        };
        assert_eq!(p.missing(), Some(Missing::Both));
        p.mpv = Some("mpv".into());
        assert_eq!(p.missing(), Some(Missing::YtDlp));
        p.ytdlp = Some("yt-dlp".into());
        assert_eq!(p.missing(), None);
    }
}
