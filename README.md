# yt-lite

A small native desktop client for your YouTube subscriptions, built with Rust and
[GPUI Kit](https://github.com/longbridge/gpui-kit) (GPUI + gpui-component). Stream URLs are
resolved natively in Rust (~0.2 s) and played by the operating system's own media player
(AVPlayer on macOS, the WinRT MediaPlayer on Windows), so there is nothing else to install.
[mpv](https://mpv.io) is available as an alternative backend, and
[yt-dlp](https://github.com/yt-dlp/yt-dlp), if installed, is a fallback resolver. The app
never loads YouTube's web player or a web view.

> **Unofficial personal project.** yt-lite is not affiliated with, endorsed by, or connected to
> YouTube or Google. It uses YouTube's public RSS feeds, the YouTube Data API with your own OAuth
> client, and YouTube's undocumented internal API, which may change or break at any time. Using
> third-party clients may conflict with YouTube's Terms of Service; use it at your own discretion.
> No credentials, tokens or Google OAuth clients are included in this repository.

- Sidebar with **Subscriptions** (chronological), **For you** (recommendations), **Explore**
  (popular this week by topic), **Search** and **Settings**. Everything except Subscriptions
  works without signing in.
- Responsive grid: thumbnail, title, channel, duration, age
- **No Shorts, anywhere.** Filtered centrally for every feed source; the filter fails closed
- Click a video to play it in a native player window (1080p max by default); watched state is
  kept locally
- Native stream resolver (a Rust port of the relevant part of yt-dlp), yt-dlp as fallback
- Optional SponsorBlock via the mpv script (mpv backend)
- Low memory: virtualized grid, byte-capped thumbnail cache, idle trimming, and a live RSS readout

Primary target is Windows 11; macOS is supported too (menu bar, ⌘ shortcuts, `.app` bundle).
There's also a SwiftUI **iPhone app** on the same Rust core: `scripts/build-ios.sh`, then open
`ios/YtLite.xcodeproj`. See [docs/ios.md](docs/ios.md).

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

### 2. Optional: yt-dlp (fallback) and mpv (alternative player)

Nothing is required to play videos. Optionally:

```powershell
winget install yt-dlp.yt-dlp     # fallback when the native resolver fails
winget install shinchiro.mpv     # only if you set player.backend = "mpv"
```

or with scoop:

```powershell
scoop bucket add extras
scoop install extras/mpv yt-dlp
```

The app finds them on `PATH`, in scoop's `shims` folder, and in winget's `Links` folder. If it
can't, clicking a video shows an error with install instructions; set `player.mpv_path` /
`player.ytdlp_path` in the config to full paths. yt-dlp is only used when the native resolver
fails, but keep it installed and up to date (`yt-dlp -U` or `winget upgrade yt-dlp.yt-dlp`):
YouTube changes often break old versions of either.

## Setup (macOS)

```sh
scripts/bundle-macos.sh --install     # builds target/release/yt-lite.app, copies to /Applications
brew install yt-dlp                   # optional fallback resolver (and mpv, for the mpv backend)
```

Any Xcode Command Line Tools install is enough to build. Config lives in
`~/Library/Application Support/yt-lite/config.toml` (⌘, opens the folder), the refresh token in
the login Keychain.

**Keychain prompts:** macOS ties Keychain access to the app's code signature. `cargo run` (via the
runner in `.cargo/config.toml`) and `scripts/bundle-macos.sh` sign yt-lite with your first
"Apple Development" certificate (Xcode creates one when you add your Apple ID; override with
`YT_LITE_SIGN_IDENTITY`). The signature then stays the same across rebuilds, so click
**Always Allow** once. Without a certificate it falls back to ad-hoc signing and macOS asks after
every rebuild. The token is read once per run. Logs go to
`~/Library/Application Support/yt-lite/data/yt-lite.log` when launched from the bundle.

Shortcuts: ⌘K / ⌘F search, ⌘R refresh, ⇧⌘H hide/show watched, ⌘, settings, ⌘W / ⌘Q quit
(Ctrl on Windows; F5 also refreshes).

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
opens its folder). Restart the app after editing. A commented template is in `config.example.toml` in the repository.

```toml
[google]
client_id = "1234-abc.apps.googleusercontent.com"
client_secret = "GOCSPX-..."

[player]
backend = "system"        # "system" (built-in OS player, nothing to install) or "mpv"
mpv_path = "mpv"          # full path, or a name found on PATH
ytdlp_path = "yt-dlp"     # passed to mpv as ytdl_hook-ytdl_path
max_height = 1080         # yt-dlp format: bestvideo[height<=?1080]+bestaudio
sponsorblock = false      # load the SponsorBlock mpv script if present
# sponsorblock_script = 'C:\Tools\mpv\sponsorblock.lua'
extra_args = []           # appended to the mpv command line, e.g. ["--volume=60"]
resolver = "native"       # "native" (falls back to yt-dlp if it fails) or "yt-dlp"
codecs = ["avc1", "vp9", "av01"]   # native resolver preference at equal quality

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
| `YT_LITE_HOME` | Portable mode: config, data, cache, and its own Keychain/Credential Manager entry under this folder |
| `YT_LITE_SCROLL_TEST=1` | Memory self-test: scroll the whole grid twice and log RSS |
| `YT_LITE_AUTOPLAY=<id>` | Play this video on startup (testing aid) |
| `YT_LITE_PAGE=foryou\|explore\|search\|settings` | Start on that page (testing aid) |
| `YT_LITE_SEARCH=<query>` | Start with this search (testing aid) |
| `RUST_LOG` | Log filter, e.g. `info,yt_lite=debug` |

### SponsorBlock (mpv backend only)

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

### Playback backends

**`system` (default)** plays the HLS stream from the native resolver in the OS's own player:

- **macOS:** AVPlayer in an AVKit window. Native controls, fullscreen, Picture in Picture,
  AirPlay, hardware decoding. `crates/desktop/src/system_player/macos.rs`.
- **Windows:** the built-in WinRT `MediaPlayer`, rendered through Windows.UI.Composition into a
  Win32 window (`crates/winplayer`). Media keys and the Windows media flyout work. Controls:
  Space/K or click = pause, ←/→ = 5 s, J/L = 10 s, ↑/↓ = volume, M = mute, F or double-click =
  fullscreen, Esc = leave fullscreen. The title bar shows the time; click the bar at the bottom
  to seek.

The player window is reused for the next video. Closing it stops playback and releases the
stream. If native resolution fails and yt-dlp is installed, yt-dlp supplies the URL instead.

**`mpv`** starts an external mpv with direct URLs (or with yt-dlp as its resolver as a
fallback). It supports the SponsorBlock script; see below.

`YT_LITE_AUTOPLAY=<video id>` plays a video on startup, which is handy for testing playback.

### Native stream resolver (`crates/core/src/resolve`)

A port of the part of yt-dlp's YouTube extractor that matters here (yt-dlp is public domain):

1. One InnerTube `POST /youtubei/v1/player` request as the **`visionos`** client. As of
   2026-10 it is the only client yt-dlp uses whose stream URLs need neither a JS runtime
   (signature/`n` deciphering) nor a PO token. `android_vr` started getting 403s in 2026-08.
2. `visitorData` (a logged-out session id) is required; without it YouTube answers
   `LOGIN_REQUIRED` "confirm you're not a bot". That response carries a fresh one, so the
   resolver retries once with it and caches it in SQLite. YouTube flags sessions after a
   while; when the cached one gets `LOGIN_REQUIRED`, the resolver starts a new session the
   same way. If a brand-new session is rejected too, YouTube is blocking the IP address.
3. Format selection: highest quality tier ≤ `max_height` (from YouTube's `qualityLabel`, so
   2:1 and vertical videos are handled), then fps, then `codecs` order; audio is the
   original-language track (auto-dubbed videos list several), non-DRC, best bitrate. Formats
   with a signature cipher, an `n` parameter, DRM or OTF/live fragments are never used.
4. The system player gets the HLS master playlist (all qualities, audio included). For the mpv
   backend, mpv gets the two adaptive URLs directly (`--audio-file`), with `--ytdl=no`. Two
   details:
   - googlevideo **throttles open-ended requests** to about real time (measured: 150 KB/s for a
     4 Mbps stream) but serves bounded range requests at full speed (40 MB/s). mpv is told to
     fetch in 10 MiB chunks (`--stream-lavf-o-append=request_size=10485760`), as mpv's own
     ytdl_hook does.
   - A `Referer: https://www.youtube.com/watch?v=<id>` header lets the SponsorBlock script
     find the video id.
5. Live streams use the HLS manifest.

If anything fails (YouTube changed something, age-restricted, made-for-kids), playback falls
back to mpv + yt-dlp automatically. To debug the native path:

```sh
cargo run -p yt-lite-core --example resolve -- <video id or URL> [max_height]
```

When YouTube breaks it, the fix is usually updating the `VISIONOS` constants in
`crates/core/src/resolve/innertube.rs` from yt-dlp's `INNERTUBE_CLIENTS`.

### For you (`crates/core/src/feed/foryou.rs`)

Recommendations without signing in to YouTube. Logged out, YouTube's own home feed is empty
("Try searching to get started"), so yt-lite builds one:

1. Seeds are the 10 videos you most recently played in yt-lite, topped up with your newest
   subscription uploads until you've watched a few.
2. For each seed, the InnerTube `next` endpoint (WEB client, logged out) returns the related
   videos youtube.com shows next to it.
3. The lists are interleaved round-robin, so no single seed dominates. Duplicates, the seeds and
   anything already watched are dropped, up to 150 videos.
4. Listings include the duration, so the shared pipeline's Shorts filter rarely needs the Data
   API. Shorts shelves are skipped outright.

It refreshes when stale (15 min) and whenever you open the tab after playing something.

### Search and Explore (`crates/core/src/feed/query.rs`)

Both use InnerTube `search` as the logged-out WEB client (YouTube removed the logged-out
Trending/Explore pages in 2025; their browse ids now return "invalid argument").

- **Search** (⌘K / ⌘F): videos only, by relevance, first page (~20 results).
- **Explore**: topic chips (Popular, Music, Gaming, Tech, …) run "uploaded this week, most
  viewed" searches. Popularity is global, so results skew toward the largest audiences.

Results go through the shared pipeline, so Shorts are filtered the same way as every other feed.

### Settings

Settings (sidebar, or ⌘,) edits `config.toml` in place with comments preserved. Quality, hide
watched, feed age and theme apply immediately; the player backend, Google client, refresh
interval and thumbnail memory apply after a restart.

### Feed sources

`crates/core/src/feed/mod.rs` defines the `FeedSource` trait; Subscriptions and For you are the two
implementations. A source only *discovers* videos and returns
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

With the `system` backend the player runs inside the yt-lite process, so its memory counts too:
on macOS, RSS was 212 MB while playing a 1080p video, versus about 89 MB for the feed alone.
The stream is released when you close the player window. With `backend = "mpv"`, playback
memory is in mpv's process instead.

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
cargo test          # Shorts logic, RSS + player-response parsing (saved fixtures), format
                    # selection, LRU eviction, DB, config, mpv args, …
cargo run           # debug build (dependencies are optimized even in dev)
```

Workspace layout:

```
crates/core/         yt-lite-core: no UI; also builds for iOS (aarch64-apple-ios)
  src/config.rs        config.toml + platform dirs
  src/net.rs           ureq client, Shorts probe, bounded parallel map
  src/auth.rs          OAuth + PKCE (loopback flow behind the `loopback-auth` feature)
  src/db.rs            SQLite cache
  src/shorts.rs        pure Shorts classification
  src/resolve/         native stream resolver (InnerTube player + format selection)
  src/thumbcache.rs    thumbnail disk cache
  src/lru.rs           byte-capped LRU
  src/youtube/         Data API, RSS, durations, URL parsing
  src/feed/            FeedSource trait, pipeline, subscriptions source
  examples/resolve.rs  CLI for the native resolver
  tests/fixtures/      RSS feeds and player responses captured 2026-10-07 (URLs scrubbed)
crates/desktop/      yt-lite: GPUI app
  src/main.rs          bootstrap, menus, shortcuts, logging, window
  src/player.rs        playback planning: backend choice, native resolve / yt-dlp, mpv launch
  src/system_player/   OS player: AVPlayer (macOS), winplayer (Windows)
  src/thumbs.rs        decode to display size + in-memory LRU
  src/mem.rs           RSS readout
  src/app_state.rs     shared config / services / account state (GPUI globals)
  src/ui/              app shell + sidebar, feed grid, settings, shared thumbnail store
crates/winplayer/     Windows MediaPlayer window (pure-Rust bindings; type-checks from any host
                     with `cargo check -p yt-lite-winplayer --target x86_64-pc-windows-msvc`)
crates/ffi/          UniFFI bindings for the iPhone app
ios/                 SwiftUI iPhone app (see docs/ios.md)
scripts/bundle-macos.sh, scripts/build-ios.sh
docs/ios.md          plan for a SwiftUI iPhone app on the same core
```

### Phase 2 (designed for, not built)

- **Real YouTube home feed** (cookie-authenticated InnerTube), as another `FeedSource` next to
  For you.
- Search pagination (continuations) and channel pages.
- **Embedded libmpv** inside the GPUI window.

- **Search.**

Non-goals: comments, uploading, live chat, notifications, Shorts, multiple accounts, mobile.

## License

MIT, see [LICENSE](LICENSE).
