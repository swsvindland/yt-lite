# iPhone app (SwiftUI on `yt-lite-core`)

Status: **built and running in the simulator** (2026-10-08). Explore, Search and playback were
checked there end to end. Sign-in and running on a real device haven't been tried yet. Audio only
was reworked to use HLS (2026-10-09; a 3.8-hour video plays and seeks). CarPlay builds and is
wired up, but hasn't been tried on a CarPlay screen yet.

## Run it on your iPhone

```sh
scripts/build-ios.sh          # Rust core -> ios/Generated/YtLiteCore.xcframework + Swift bindings
open ios/YtLite.xcodeproj
```

1. Put your Apple Developer Team ID in `ios/Local.xcconfig` (gitignored, so it stays out of the
   repo): `DEVELOPMENT_TEAM = ABCDE12345`. Find it in Xcode → Settings → Accounts. Choosing the
   team in Xcode's Signing tab works too, but writes it into the committed project file. If Xcode
   says the bundle id is taken, change `local.ytlite.ios` to anything unique.
2. Plug in the iPhone, select it as the run destination, and press **Run** (⌘R).
3. On the phone: enable **Settings → Privacy & Security → Developer Mode** (it restarts), then
   trust your certificate under **Settings → General → VPN & Device Management**.
4. With a free Apple ID the install expires after **7 days**; press Run again to renew.

Re-run `scripts/build-ios.sh` after changing Rust code. It also copies the Google OAuth client
from your desktop `config.toml` into the gitignored `ios/YtLite/Secrets.swift`, so the phone uses
the same "Desktop app" client. Sign-in shows Google's page in an in-app browser that redirects to
a loopback port the Rust core listens on, so there's nothing new to set up in Google Cloud.

## What's in it

- Tabs: **Subscriptions** (sign in), **For you**, **Explore** (topic chips), **Search**, **Settings**
  (account, max quality, hide watched).
- Pull to refresh; long-press a video to mark it watched/unwatched, start it over, or share it.
- **Progress:** videos resume where you stopped them, and count as watched only once they've
  played to the end (within the last 5%, 10 s to 1 min). A red bar on the thumbnail shows how far
  you got. `PlaybackProgress.swift` saves the position on pause, stop and backgrounding, and every
  10 s; the rules live in `yt-lite-core` (`progress.rs`), shared with the desktop players.
- Playback: `AVPlayerViewController` presented modally with the resolver's HLS stream. You get
  native controls, Picture in Picture and AirPlay, capped with `preferredMaximumResolution`.
- **Keep playing in background** (Settings, on by default): when you lock the phone or switch
  apps, video keeps playing as audio. It uses `audiovisualBackgroundPlaybackPolicy` and detaches
  the player from its view in the background. Lock Screen / Control Center show title, channel
  and artwork, with play/pause, ±15 s and scrubbing (`NowPlaying.swift`).
- **Audio only** (Settings default, or long-press a video → *Listen*): plays the HLS master's
  audio-only rendition (AAC, about 130 kbps instead of several Mbps), with no video decoding. It
  plays in a mini player above the tab bar with the same lock-screen controls. It doesn't use the
  single AAC file from `adaptiveFormats`. googlevideo throttles requests for more than about
  10 MB to ~32 KB/s, and AVPlayer fetches a progressive file in one request, so anything longer
  than ~10 minutes never started (verified 2026-10-09). HLS segments are a few seconds each. If a
  master has no audio rendition, the master itself is played as audio, never as video.
- **CarPlay** (`CarPlay.swift`): tabs for Subscriptions, For you and Explore (topic → videos).
  Choosing a video plays it audio only and opens the system Now Playing screen (the same
  `NowPlaying` metadata and ±15 s controls). See [CarPlay](#carplay) for the entitlement.
- Thumbnails load straight from `i.ytimg.com` via `AsyncImage`/`URLCache`.
- Data lives in the app container (`Application Support/yt-lite`), and the refresh token in the
  iOS Keychain.

## Layout

```
crates/ffi/                 UniFFI wrapper (YtLite object: refresh / videos / search / explore /
                            play / sign-in); builds staticlib (iOS) + cdylib (bindgen)
scripts/build-ios.sh        cargo (aarch64-apple-ios, aarch64-apple-ios-sim) -> xcframework,
                            uniffi-bindgen -> Swift, Secrets.swift
ios/YtLite.xcodeproj        app target; YtLite/ is a synchronized folder (new files are picked up)
ios/YtLite/*.swift          SwiftUI app
ios/Info.plist              background audio, scene manifest with the CarPlay scene (other keys
                            are generated from build settings)
ios/CarPlay.entitlements    com.apple.developer.carplay-audio (see Config.xcconfig)
```

## CarPlay

CarPlay only lists apps that have the `com.apple.developer.carplay-audio` entitlement.

- **Simulator:** always included. Run the app, then attach a CarPlay display to the simulator
  (Simulator.app: I/O → External Displays → CarPlay).
- **Your iPhone:** the provisioning profile has to include the entitlement, and Apple grants it
  only on request: <https://developer.apple.com/carplay> (needs the paid developer program; a
  free Personal Team can't get it). Once your team has it, put `YTLITE_CARPLAY = YES` in
  `ios/Local.xcconfig`. Until then, device builds leave it out so signing keeps working. CarPlay
  then still shows yt-lite's audio on its Now Playing screen with play/pause/skip, but not the
  app's own lists.

With the phone locked, the Keychain can't be read, so Subscriptions refreshes from the cached
channel list (RSS needs no sign-in) and the subscription list itself updates the next time the
phone is unlocked.

## Design notes

## Why it fits

- ✅ `yt-lite-core` has no UI dependencies and **builds for `aarch64-apple-ios`**, including
  bundled SQLite, rustls, and the Keychain (`keyring` with the data-protection keychain store).
- ✅ The native resolver returns an **HLS master playlist** (`Resolved::hls`). AVPlayer can't
  combine separate DASH audio/video, but it plays HLS natively, with Picture in Picture,
  AirPlay and background audio. Verified 2026-10-07: 15 variants up to 1080p with audio muxed
  in; segments are bounded requests, so the open-ended-request throttling doesn't apply.
- ✅ Auth is split into `begin_sign_in(redirect_uri)` / `complete_sign_in(request, code)`, so iOS
  can do the browser step with `ASWebAuthenticationSession`. The core omits `client_secret` when
  it's empty, as iOS OAuth clients require.
- The Shorts filter, feed pipeline, SQLite cache and watched state are shared unchanged.

The app can't use mpv or yt-dlp on iOS, so the native resolver is required there, not
optional. If YouTube breaks the `visionos` client, the iPhone app breaks until the constants
are updated.


- **FFI:** [UniFFI](https://mozilla.github.io/uniffi-rs/) generates Swift bindings from
  `#[uniffi::export]` annotations. Calls are blocking; Swift wraps them in
  `Task.detached { }` (or UniFFI async with a small executor). No tokio is needed, just as on
  desktop.
- **Paths:** `Paths::in_dir(<Application Support>)` puts the config, DB and cache in the app
  container. The OAuth client id is entered on a settings screen and stored with
  `@AppStorage`.
- **UI:** `NavigationStack` + `List`/`LazyVGrid` (already virtualized), thumbnails via a small
  `AsyncImage`-like view that gets bytes from `thumbnail(id)` (the shared disk cache), pull to
  refresh, swipe to mark watched.
- **Playback:** `AVPlayerViewController` with `Resolved.hls`. Cap quality with
  `preferredMaximumResolution`. Enable the audio background mode for PiP and lock-screen audio.
- **Refresh:** on foreground plus pull to refresh; `BGAppRefreshTask` is optional.

## Google OAuth on iOS

1. In the same Google Cloud project: **Credentials → Create OAuth client ID → iOS**, with the
   app's bundle id. There is no client secret.
2. Redirect URI: the reversed client id scheme,
   `com.googleusercontent.apps.<CLIENT_ID_PREFIX>:/oauth2redirect`. Register that URL scheme in
   Info.plist.
3. Call `begin_sign_in(redirect)`, open `ASWebAuthenticationSession(url:callbackURLScheme:)`,
   read `code` and `state` from the callback URL (check `state`), then call
   `complete_sign_in(request, code)`. The refresh token lands in the iOS Keychain through the
   same core code.

## Installing on your own iPhone

- Xcode → Signing & Capabilities → Team: your Apple ID ("Personal Team"). Enable Developer Mode
  on the phone (Settings → Privacy & Security).
- With a **free** Apple ID the provisioning profile expires after **7 days**; re-run from Xcode
  to renew it. A paid developer account ($99/yr) makes it 1 year.

## Rough effort

1. `crates/ffi` + build script + XCFramework: half a day, mostly build plumbing.
2. SwiftUI feed + thumbnails + watched: a day.
3. AVPlayer + PiP/background audio: half a day.
4. OAuth via ASWebAuthenticationSession: half a day.

## Open questions

- Should playback state (resume position) sync between phone and desktop? Not designed; it
  would need a shared store (iCloud, or a file in a synced folder).
- Whether the `visionos` client keeps working is outside our control. The desktop falls back to
  yt-dlp; the phone has no fallback.
