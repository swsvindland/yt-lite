# iPhone app plan (SwiftUI on `yt-lite-core`)

Status: design only. What already exists and is verified is marked ✅.

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

## Shape

```
crates/core/        (exists) blocking Rust API
crates/ffi/         new: UniFFI wrapper, a thin facade over core
  YtLite object:    new(root_dir, client_id) · refresh() -> Stats · feed(query) -> [Video]
                    set_watched(id, bool) · resolve(id, max_tier) -> Resolved
                    begin_sign_in(redirect) -> AuthRequest · complete_sign_in(req, code)
                    thumbnail(id) -> Data (JPEG bytes from the disk cache)
ios/YtLite.xcodeproj  new: SwiftUI app
scripts/build-ios.sh  new: cargo build for aarch64-apple-ios + aarch64-apple-ios-sim,
                      lipo/xcodebuild -create-xcframework, uniffi-bindgen → Swift
```

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
