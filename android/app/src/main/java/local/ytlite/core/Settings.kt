package local.ytlite.core

import android.content.Context
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.intPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

data class Settings(
    /** Highest video quality, e.g. 1080 for 1080p. */
    val maxHeight: Int = 1080,
    /** Video keeps playing as audio when the app is in the background. */
    val backgroundPlayback: Boolean = true,
    /** Play just the sound by default (podcasts). */
    val audioOnly: Boolean = false,
    val hideWatched: Boolean = false,
    /** Material You colors from the wallpaper instead of yt-lite red. */
    val dynamicColor: Boolean = false,
)

private val Context.dataStore by preferencesDataStore("settings")

class SettingsStore(context: Context) {
    private val store = context.applicationContext.dataStore

    val settings: Flow<Settings> = store.data.map { it.toSettings() }

    suspend fun update(change: (Settings) -> Settings) {
        store.edit { p ->
            val new = change(p.toSettings())
            p[MAX_HEIGHT] = new.maxHeight
            p[BACKGROUND_PLAYBACK] = new.backgroundPlayback
            p[AUDIO_ONLY] = new.audioOnly
            p[HIDE_WATCHED] = new.hideWatched
            p[DYNAMIC_COLOR] = new.dynamicColor
        }
    }

    private fun Preferences.toSettings(): Settings {
        val defaults = Settings()
        return Settings(
            maxHeight = this[MAX_HEIGHT] ?: defaults.maxHeight,
            backgroundPlayback = this[BACKGROUND_PLAYBACK] ?: defaults.backgroundPlayback,
            audioOnly = this[AUDIO_ONLY] ?: defaults.audioOnly,
            hideWatched = this[HIDE_WATCHED] ?: defaults.hideWatched,
            dynamicColor = this[DYNAMIC_COLOR] ?: defaults.dynamicColor,
        )
    }

    private companion object {
        val MAX_HEIGHT: Preferences.Key<Int> = intPreferencesKey("maxHeight")
        val BACKGROUND_PLAYBACK = booleanPreferencesKey("backgroundPlayback")
        val AUDIO_ONLY = booleanPreferencesKey("audioOnly")
        val HIDE_WATCHED = booleanPreferencesKey("hideWatched")
        val DYNAMIC_COLOR = booleanPreferencesKey("dynamicColor")
    }
}
