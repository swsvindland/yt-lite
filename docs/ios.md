# iPhone app (SwiftUI on `yt-lite-core`)

Status: **built and running in the simulator** (2026-10-08). Explore, Search and playback were
checked there end to end. Sign-in and running on a real device haven't been tried yet.

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
- Pull to refresh; long-press a video to mark it watched/unwatched or share it.
- Playback: `AVPlayerViewController` presented modally with the resolver's HLS stream. You get
  native controls, Picture in Picture and AirPlay, capped with `preferredMaximumResolution`.
- **Keep playing in background** (Settings, on by default): when you lock the phone or switch
  apps, video keeps playing as audio. It uses `audiovisualBackgroundPlaybackPolicy` and detaches
  the player from its view in the background. Lock Screen / Control Center show title, channel
  and artwork, with play/pause, ±15 s and scrubbing (`NowPlaying.swift`).
- **Audio only** (Settings default, or long-press a video → *Listen*): plays just the AAC audio
  stream (`audio/mp4`, since AVPlayer can't decode Opus/WebM), about 130 kbps instead of several
  Mbps, with no video decoding. It plays in a mini player above the tab bar with the same
  lock-screen controls. AVPlayer misreports these files' duration (about 2×), so the listing's
  duration is used.
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
ios/Info.plist              background audio (other keys are generated from build settings)
```

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
