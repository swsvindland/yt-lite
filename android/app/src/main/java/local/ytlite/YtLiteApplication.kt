package local.ytlite

import android.app.Application
import android.content.ComponentCallbacks2
import android.content.Context
import coil3.ImageLoader
import coil3.PlatformContext
import coil3.SingletonImageLoader
import coil3.memory.MemoryCache
import local.ytlite.core.AppModel
import local.ytlite.core.KeystoreSecretStore
import uniffi.yt_lite_ffi.setSecretStore

class YtLiteApplication : Application(), SingletonImageLoader.Factory {
    lateinit var model: AppModel
        private set

    override fun onCreate() {
        super.onCreate()
        // Before the core starts: the sign-in token lives in Keystore-encrypted storage.
        setSecretStore(KeystoreSecretStore(this))
        model = AppModel(this)
    }

    /** Thumbnails decode to 320×180 (~230 KB): this keeps about 140, a few
     *  dozen screens' worth. Coil's default is a fifth of the app's heap. */
    override fun newImageLoader(context: PlatformContext): ImageLoader =
        ImageLoader.Builder(context)
            .memoryCache { MemoryCache.Builder().maxSizeBytes(32L * 1024 * 1024).build() }
            .build()

    /** The UI is gone (screen off, another app): drop decoded thumbnails while
     *  playback carries on for hours. Coil only does this once the app is in
     *  line to be killed, which a playing app never is. */
    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        if (level >= ComponentCallbacks2.TRIM_MEMORY_UI_HIDDEN) {
            SingletonImageLoader.get(this).memoryCache?.clear()
        }
    }
}

/** The app-wide model, shared by the UI and PlaybackService (Android Auto). */
val Context.model: AppModel
    get() = (applicationContext as YtLiteApplication).model
