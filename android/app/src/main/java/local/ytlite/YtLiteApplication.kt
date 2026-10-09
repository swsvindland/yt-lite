package local.ytlite

import android.app.Application
import android.content.Context
import local.ytlite.core.AppModel
import local.ytlite.core.KeystoreSecretStore
import uniffi.yt_lite_ffi.setSecretStore

class YtLiteApplication : Application() {
    lateinit var model: AppModel
        private set

    override fun onCreate() {
        super.onCreate()
        // Before the core starts: the sign-in token lives in Keystore-encrypted storage.
        setSecretStore(KeystoreSecretStore(this))
        model = AppModel(this)
    }
}

/** The app-wide model, shared by the UI and PlaybackService (Android Auto). */
val Context.model: AppModel
    get() = (applicationContext as YtLiteApplication).model
