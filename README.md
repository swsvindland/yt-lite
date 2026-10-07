# yt-lite

A small native desktop client for your YouTube subscriptions, built with Rust and
[GPUI Kit](https://github.com/longbridge/gpui-kit) (GPUI + gpui-component). Videos play
in [mpv](https://mpv.io) through [yt-dlp](https://github.com/yt-dlp/yt-dlp). The app never
loads YouTube's web player or a web view.

- Chronological subscriptions grid: thumbnail, title, channel, duration, age
- **No Shorts, anywhere.** Filtered centrally for every feed source; the filter fails closed
- Click a video to open it in mpv (1080p max by default); watched state is kept locally
- Optional SponsorBlock via the mpv script
- Low memory: virtualized grid, byte-capped thumbnail cache, idle trimming, and a live RSS readout

Primary target is Windows 11; it also runs on macOS (where it was developed and tested).

---

## Setup (Windows 11)

### 1. Build tools

1. Install Rust with the MSVC toolchain: <https://rustup.rs> (`rustup default stable-msvc`).
2. Install **Visual Studio 2022 Build Tools** with the **"Desktop development with C++"**
   workload. It must include a **Windows 10/11 SDK**: GPUI's release build compiles its
   HLSL shaders with `fxc.exe` from the SDK, and SQLite is compiled from source.

```powershell
cargo build --release
.\target\release\yt-lite.exe
```

Release builds run without a console window and log to `%LOCALAPPDATA%\yt-lite\data\yt-lite.log`.
Debug builds (`cargo run`) log to the console.

### 2. mpv and yt-dlp

```powershell
winget install shinchiro.mpv
winget install yt-dlp.yt-dlp
```

or with scoop:

```powershell
scoop bucket add extras
scoop install extras/mpv yt-dlp
```

The app finds them on `PATH`, in scoop's `shims` folder, and in winget's `Links` folder. If it
can't, clicking a video shows an error with install instructions; set `player.mpv_path` /
`player.ytdlp_path` in the config to full paths. Keep yt-dlp up to date (`yt-dlp -U` or
`winget upgrade yt-dlp.yt-dlp`): YouTube changes often break old versions.

### 3. Google Cloud OAuth client (for your subscription list)

The subscription list comes from the YouTube Data API, which needs your own OAuth client.
This is free; the app uses roughly 10 quota units per refresh out of 10,000/day.

1. Go to <https://console.cloud.google.com/> and create a project (e.g. "yt-lite").
2. **APIs & Services → Library**: enable **YouTube Data API v3**.
3. **APIs & Services → OAuth consent screen** (also called "Google Auth Platform"):
   - User type **External**. Fill in the app name and your email.
   - Scopes: add `https://www.googleapis.com/auth/youtube.readonly`.
   - Audience / test users: add your own Google account.
   - **Publishing status:** while the app is in **Testing**, Google expires refresh tokens
     after **7 days**, so you'd have to sign in again every week. To avoid that, click
     **Publish app** ("In production"). You don't need verification for personal use. You'll
     see a "Google hasn't verified this app" warning at sign-in; click *Advanced → Go to
     yt-lite*.
4. **APIs & Services → Credentials → Create credentials → OAuth client ID**,
   application type **Desktop app**. Copy the client ID and client secret into the config
   file (below).
5. Start yt-lite and click **Sign in with Google**. Your browser opens Google's consent page
   and redirects back to a temporary `http://127.0.0.1:<port>` listener in the app.

The refresh token is stored in **Windows Credential Manager** (service `yt-lite`), never in a
file. Access tokens are kept only in memory. **Sign out** deletes the stored token.

You can skip OAuth entirely and list channel ids in `feed.extra_channels`; see below.

---

## Configuration

`%APPDATA%\yt-lite\config\config.toml` (created with defaults on first run; the gear button
opens its folder). Restart the app after editing.

```toml
[google]
client_id = "1234-abc.apps.googleusercontent.com"
client_secret = "GOCSPX-..."

[player]
mpv_path = "mpv"          # full path, or a name found on PATH
ytdlp_path = "yt-dlp"     # passed to mpv as ytdl_hook-ytdl_path
max_height = 1080         # yt-dlp format: bestvideo[height<=?1080]+bestaudio
sponsorblock = false      # load the SponsorBlock mpv script if present
# sponsorblock_script = 'C:\Tools\mpv\sponsorblock.lua'
extra_args = []           # appended to the mpv command line, e.g. ["--volume=60"]

[feed]
refresh_interval_minutes = 15   # automatic refresh; values below 15 are raised to 15
max_age_days = 60               # hide uploads older than this
max_items = 1000                # rows loaded into the grid
rss_concurrency = 8             # parallel RSS requests
hide_watched = false
extra_channels = []             # channel ids (UC...) followed via RSS, no sign-in needed

[cache]
thumb_memory_mb = 50     # hard cap on decoded thumbnails in RAM (minimum 16)
thumb_disk_mb = 300      # on-disk JPEG cache, pruned least-recently-used on startup

[ui]
show_memory = true       # RSS readout in the status bar
dark = true
```

Other locations:

| What | Where |
|---|---|
| SQLite cache (videos, durations, Shorts verdicts, watched) | `%LOCALAPPDATA%\yt-lite\data\cache.sqlite3` |
| Thumbnail disk cache | `%LOCALAPPDATA%\yt-lite\cache\thumbs\` |

Environment variables:

| Variable | Effect |
|---|---|
| `YT_LITE_CONFIG` | Use this config file instead |
| `YT_LITE_HOME` | Portable mode: config, data, and cache all under this folder |
| `YT_LITE_SCROLL_TEST=1` | Memory self-test: scroll the whole grid twice and log RSS |
| `RUST_LOG` | Log filter, e.g. `info,yt_lite=debug` |

### SponsorBlock

Download `sponsorblock.lua` from <https://github.com/po5/mpv_sponsorblock> and save it as
`%APPDATA%\yt-lite\config\mpv-scripts\sponsorblock.lua` (or point `player.sponsorblock_script`
at it), then set `sponsorblock = true`. Keep it **out of** mpv's own `scripts` folder, or mpv
will autoload it regardless of this setting.

---

## How it works

### Subscriptions feed

1. `subscriptions.list` (50 per page, 1 unit/page) gives the channel list. It's cached, so a
   failed call (offline, quota) falls back to the last list.
2. Each channel's uploads come from the free RSS feed
   `https://www.youtube.com/feeds/videos.xml?channel_id=…` (latest 15, no quota), 8 at a time.
   If RSS fails for a channel, its uploads playlist (`UC…` → `UU…`) is read via
   `playlistItems.list` instead (1 unit).
3. Durations come from `videos.list`, 50 ids per call, only for videos that don't have one
   yet. Each video is fetched once and cached in SQLite.

### Shorts filter (fails closed)

A video is shown only after it has a cached **not-a-Short** verdict:

1. **RSS link:** YouTube's RSS now links Shorts as `/shorts/<id>` instead of `/watch?v=`. That
   is a definitive Short.
2. **Duration:** longer than 3 minutes (181 s with rounding slack), or live/upcoming, means not
   a Short.
3. **Probe** for anything ≤ 3 minutes: a non-following `HEAD https://www.youtube.com/shorts/<id>`.
   `200` means Short; a redirect to `/watch` means not a Short. Anything else (e.g. a consent
   wall) is inconclusive: the video stays hidden and is retried on the next refresh. The probe
   sends the `SOCS=CAI` consent cookie so EU users don't get the consent redirect.

Videos with no duration yet stay hidden until enrichment fills one in. Without sign-in, the
probe alone decides. At most 400 probes run per refresh.

### Feed sources (Phase 2 ready)

`src/feed/mod.rs` defines the `FeedSource` trait. A source only *discovers* videos and returns
them with a retention policy. `feed::pipeline::refresh` then caches, enriches, and
Shorts-filters the output of **every** source, so a new source can't bypass the filter. The
planned InnerTube home/recommended feed is another `FeedSource` (`FeedKind::Home`, using
`Retain::OnlyFetched`). If InnerTube breaks, that one source gets fixed or disabled without
touching the UI.

### HTTP without tokio

GPUI has its own executors and isn't tokio-based. Rather than running a tokio runtime on a side
thread for `reqwest`, yt-lite uses blocking **`ureq`** (rustls) on **smol's blocking thread
pool** (`smol::unblock`), which hands back an ordinary future that GPUI tasks `.await`. No
second async runtime, and threads exist only while requests are in flight.

---

## Memory

The status bar shows the process RSS (Windows: working set) plus the thumbnail cache size, and
the log records RSS at startup, after each refresh, and every 5 minutes.

What keeps it low:

- **Virtualized grid** (`uniform_list`): only visible rows are built, and only visible cards
  request thumbnails.
- **Thumbnails** are `mqdefault.jpg` (320×180, no letterboxing) decoded once at display size
  into a byte-capped LRU (`cache.thumb_memory_mb`). Evicted images are also removed from
  GPUI's GPU sprite atlas (`cx.drop_image`), otherwise the atlas would grow without bound.
- **Idle trim:** 60 s after the window loses focus, all decoded thumbnails are released. They
  reload from the disk cache in milliseconds when you come back.
- SQLite page cache is limited to about 2 MB; refresh runs at most every 15 minutes.

Measured on macOS (Apple Silicon, Retina, 1400×900 window), release build, 23 channels
→ 345 videos, 121 Shorts hidden, 224 shown:

| State | RSS |
|---|---|
| Window open, idle | 75–105 MB |
| After scrolling all 224 cards, thumbnail LRU full (49 MB) | 229 MB, stable across passes (no leak) |
| Thumbnail cap 20 MB, same scroll | 177–184 MB |
| 60 s after backgrounding (idle trim) | 181 MB |

On Apple Silicon each cached thumbnail counts about twice: the CPU buffer plus its GPU-atlas
copy share unified memory. On Windows with a discrete GPU the atlas lives in VRAM, and a
non-Retina window has much smaller swapchain buffers. **The Windows numbers have not been
measured yet.** To check against the 150 MB target:

```powershell
$env:YT_LITE_SCROLL_TEST=1; $env:RUST_LOG="info"; cargo run --release
```

The output includes `memory [scroll test pass 2]`. If a full cache goes over budget, lower
`cache.thumb_memory_mb` (24–32 still holds several screens of thumbnails).

---

## Development

```sh
cargo test          # Shorts logic, RSS parsing (saved fixtures), LRU eviction, DB, config, …
cargo run           # debug build (dependencies are optimized even in dev)
```

Module layout:

```
src/
  main.rs              bootstrap, logging, window
  config.rs            config.toml + platform dirs
  net.rs               ureq client, Shorts probe, bounded parallel map
  auth.rs              OAuth loopback + PKCE, refresh token in the OS keyring
  db.rs                SQLite cache
  shorts.rs            pure Shorts classification
  player.rs            mpv/yt-dlp detection and launch
  lru.rs, thumbs.rs    byte-capped LRU, thumbnail load/decode/disk cache
  mem.rs               RSS readout
  youtube/{api,rss,duration}.rs
  feed/{mod,pipeline,subscriptions}.rs
  ui/{feed_view,status}.rs
tests/fixtures/        real RSS feeds captured 2026-10-07
```

### Phase 2 (designed for, not built)

- **Home/recommended feed** via InnerTube: implement `FeedSource` for `FeedKind::Home` and add
  a source switcher to the toolbar.
- **Embedded libmpv** inside the GPUI window.
- **Search.**

Non-goals: comments, uploading, live chat, notifications, Shorts, multiple accounts, mobile.
