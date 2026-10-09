# Android app (Jetpack Compose on `yt-lite-core`)

Status: **built and running in the emulator** (2026-10-09, Android 16). Explore, playback (video,
audio only, background, Picture in Picture), progress and the Android Auto browse tree were checked
there; sign-in got as far as Google's page. Running on a phone, signing in, and Android Auto in a
real car haven't been tried yet.

## Run it on your phone

```sh
scripts/build-android.sh      # Rust core -> android/app/src/main/jniLibs + Kotlin bindings + secrets
```

Then open `android/` in Android Studio and press **Run**, or from the command line:

```sh
cd android && ./gradlew installDebug
```

On the phone, enable **Developer options → USB debugging** first. Re-run `scripts/build-android.sh`
after changing Rust code. Like the iOS script, it copies the Google OAuth client from your desktop
`config.toml` (into the gitignored `android/secrets.properties`).

`./gradlew assembleRelease` makes a minified APK (about 12 MB), signed with the debug key since
this is a personal sideloaded app.

## What's in it

- **Material 3** with yt-lite red, light or dark with the system; **Dynamic color** (Material You)
  is a setting. Edge to edge, predictive back, the Android 12+ splash screen, a themed (monochrome)
  launcher icon.
- **Adaptive layout**: a bottom bar on phones, a navigation rail on tablets, foldables and in
  landscape (`NavigationSuiteScaffold`); the video grid adds columns as it gets wider.
- Tabs: **Subscriptions** (sign in), **For you**, **Explore** (topic chips), **Search**, **You**:
  your **History** (everything played or marked watched, most recent first, with progress;
  remove videos or clear it) and, behind the gear, **Settings**. Pull to refresh; long-press a
  video for Watch / Listen / Start over / Mark watched / Share (and Remove from history).
- **Playback** runs in a Media3 `MediaLibraryService` (`PlaybackService`) with ExoPlayer, so it
  survives leaving the app: the system media controls (notification, lock screen, Bluetooth)
  come with it, with back 10 s / forward 30 s.
  - Video: full screen and immersive, Media3's `PlayerView`, and **Picture in Picture**
    automatically when you leave the app (Android 12+; earlier versions on Home).
  - **Keep playing in background** (Settings): leaving the app (not into Picture in Picture)
    keeps a video going as audio, with its video track turned off so nothing is decoded or
    downloaded for it. Off: it pauses.
  - **Audio only** (Settings default, or long-press → Listen): the HLS audio-only rendition,
    in a mini player above the tabs.
- **Progress**: resumes where you stopped; watched once played to the end (the shared rules in
  `yt-lite-core`'s `progress.rs`). The service saves on pause, close, the end, and every 10 s.
- **Android Auto**: Subscriptions, For you, Explore (topic → videos) and History as tabs, search,
  progress on every video, and the Now Playing screen. Videos play audio only. See below.
- Sign-in: Google's page in a **Custom Tab**. The core listens on a loopback port for Google's
  redirect, then sends the tab to `ytlite://signed-in`, which brings the app back over it.
- The refresh token is stored encrypted with an **Android Keystore** key (`KeystoreSecretStore`),
  registered with the core as keyring-core's default store. The key works while the phone is
  locked, so Subscriptions refreshes in the car too.

## Android Auto

Nothing to request from Google for personal use: Android Auto lists any app that declares a media
browse service (`automotive_app_desc.xml`), but only Play Store installs unless you allow others:
on the phone, open **Android Auto settings → tap Version repeatedly → Developer settings →
Unknown sources**.

To try it without a car, Android Studio's SDK Manager has the **Android Auto Desktop Head Unit
emulator** (SDK Tools); it connects to a phone running Android Auto.

`app/src/androidTest/.../AndroidAutoTest.kt` checks the service the way Android Auto uses it: the
platform `MediaBrowser` sees the four tabs and search, and a browse item from Explore resolves and
plays audio only (`./gradlew connectedDebugAndroidTest`, needs a device or emulator and network).

## Layout

```
crates/ffi/                     UniFFI wrapper (shared with iOS); Kotlin bindings via JNA
  src/secret_store.rs           SecretStore callback interface -> keyring-core store
scripts/build-android.sh        cargo (NDK clang, 16 KB pages) -> jniLibs, uniffi-bindgen -> Kotlin
android/app/src/main/java/local/ytlite/
  YtLiteApplication.kt          registers the secret store, creates the AppModel
  core/                         AppModel (the core, settings, sign-in), Keystore store, formatting
  playback/                     PlaybackService (ExoPlayer + session + progress), LibraryTree
                                (Android Auto), ArtworkProvider (content:// thumbnails),
                                PlayerConnection (the UI's MediaController), VideoItems
  ui/                           MainActivity (PiP), App (tabs), screens, grid, mini/video player
```

Versions: AGP 9.3, Kotlin 2.4, Compose BOM 2026.09, Media3 1.11; compileSdk 37.1, targetSdk 36,
minSdk 28. `arm64-v8a` only by default (phones and Apple silicon emulators); set
`YT_LITE_ANDROID_ABIS="arm64-v8a x86_64"` for an Intel emulator.

## Notes

- The core is the same as iOS and desktop: the `visionos` InnerTube client, the Shorts filter,
  the SQLite cache (`files/yt-lite` in the app's private storage).
- Backups are off (`allowBackup="false"`): the Keystore key can't move to another device, and
  everything else is a cache.
- Phone, iPhone and desktop each keep their own progress; nothing syncs.
