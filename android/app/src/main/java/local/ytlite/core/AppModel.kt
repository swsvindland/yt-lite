package local.ytlite.core

import android.app.Application
import android.content.Context
import android.net.Uri
import android.util.Log
import androidx.browser.customtabs.CustomTabsIntent
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import local.ytlite.BuildConfig
import local.ytlite.playback.PlayerConnection
import uniffi.yt_lite_ffi.Feed
import uniffi.yt_lite_ffi.FfiException
import uniffi.yt_lite_ffi.Playable
import uniffi.yt_lite_ffi.SignIn
import uniffi.yt_lite_ffi.Video
import uniffi.yt_lite_ffi.YtLite

/**
 * Owns the Rust core (`YtLite`). Every core call blocks (network or SQLite),
 * so they run on Dispatchers.IO via [io]. Shared by the UI and
 * PlaybackService (Android Auto), like the iOS app's AppModel.
 */
class AppModel(private val app: Application) {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val settingsStore = SettingsStore(app)

    val core: YtLite?
    /** Set if the core couldn't start. */
    val startupError: String?

    val settings: StateFlow<Settings> =
        settingsStore.settings.stateIn(scope, SharingStarted.Eagerly, Settings())

    private val _signedIn = MutableStateFlow(false)
    val signedIn = _signedIn.asStateFlow()
    private val _signingIn = MutableStateFlow(false)
    val signingIn = _signingIn.asStateFlow()
    /** Set if secure storage can't hold the sign-in token. */
    private val _credentialProblem = MutableStateFlow<String?>(null)
    val credentialProblem = _credentialProblem.asStateFlow()
    private var pendingSignIn: SignIn? = null

    private val _resolving = MutableStateFlow(false)
    val resolving = _resolving.asStateFlow()

    /** Bumped when watched state or progress changes so lists reload. */
    private val _libraryVersion = MutableStateFlow(0)
    val libraryVersion = _libraryVersion.asStateFlow()

    /** Errors for the snackbar. */
    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 8)
    val messages = _messages.asSharedFlow()

    /** The UI's connection to PlaybackService (created on first use). */
    val player: PlayerConnection by lazy { PlayerConnection(app) { _messages.tryEmit(it) } }

    /** When each feed was last refreshed from YouTube (elapsed realtime, ms). */
    private val refreshedAt = java.util.concurrent.ConcurrentHashMap<Feed, Long>()

    init {
        var core: YtLite? = null
        var error: String? = null
        try {
            val root = File(app.filesDir, "yt-lite").apply { mkdirs() }
            core = YtLite(root.path, BuildConfig.GOOGLE_CLIENT_ID, BuildConfig.GOOGLE_CLIENT_SECRET)
        } catch (e: Exception) {
            error = "Couldn't start: ${describe(e)}"
            Log.e(TAG, "starting the core", e)
        }
        this.core = core
        startupError = error
        scope.launch { checkSignIn() }
    }

    val hasGoogleClient: Boolean get() = core?.hasGoogleClient() ?: false
    val exploreTopics: List<String> by lazy { core?.exploreTopics() ?: emptyList() }

    fun updateSettings(change: (Settings) -> Settings) {
        scope.launch { settingsStore.update(change) }
    }

    suspend fun checkSignIn() {
        val core = core ?: return
        if (_signingIn.value) return
        _signedIn.value = io { core.isSignedIn() }
        _credentialProblem.value = io { core.credentialStoreProblem() }
    }

    // Feeds

    suspend fun videos(feed: Feed, hideWatched: Boolean = settings.value.hideWatched): List<Video> =
        core?.let { core -> io { core.videos(feed, hideWatched) } } ?: emptyList()

    /** Fetches from YouTube unless it was done in the last 15 minutes (or `force`). */
    suspend fun refresh(feed: Feed, force: Boolean = true) {
        val core = core ?: return
        val last = refreshedAt[feed]
        if (!force && last != null && now() - last < STALE_AFTER_MS) return
        io { core.refresh(feed) }
        refreshedAt[feed] = now()
    }

    fun isStale(feed: Feed): Boolean = refreshedAt[feed]?.let { now() - it >= STALE_AFTER_MS } ?: true

    suspend fun search(query: String): List<Video> =
        core?.let { core -> io { core.search(query) } } ?: emptyList()

    suspend fun explore(topic: Int): List<Video> =
        core?.let { core -> io { core.explore(topic.toUInt()) } } ?: emptyList()

    /** `videos` with their watched state and progress re-read from the cache,
     *  in the same order (Explore and Search keep their own lists). */
    suspend fun refreshed(videos: List<Video>, feed: Feed): List<Video> {
        if (videos.isEmpty()) return videos
        val fresh = runCatching { videos(feed, hideWatched = false) }.getOrNull() ?: return videos
        val byId = fresh.associateBy { it.id }
        return videos.map { byId[it.id] ?: it }
    }

    /** Also forgets the resume point. */
    fun setWatched(video: Video, watched: Boolean) {
        val core = core ?: return
        scope.launch {
            runCatching { io { core.setWatched(video.id, watched) } }
                .onFailure { _messages.tryEmit(describe(it)) }
            _libraryVersion.update { it + 1 }
        }
    }

    // Playback

    /** `audioOnly: null` uses the Settings default. Resumes where the video
     *  was stopped unless `fromStart`; it counts as watched once it has
     *  played to the end (PlaybackService saves the position). */
    fun play(video: Video, audioOnly: Boolean? = null, fromStart: Boolean = false) {
        val core = core ?: return
        if (_resolving.value) return
        _resolving.value = true
        val wantAudio = audioOnly ?: settings.value.audioOnly
        scope.launch {
            try {
                val playable = resolve(core, video.id, wantAudio, fromStart)
                player.play(video, playable)
            } catch (e: Exception) {
                _messages.tryEmit(describe(e))
            } finally {
                _resolving.value = false
            }
        }
    }

    /** Blocking: for PlaybackService's background threads. */
    fun resolveBlocking(videoId: String, audioOnly: Boolean, fromStart: Boolean = false): Playable {
        val core = core ?: throw FfiException.Failed(startupError ?: "the core didn't start")
        return core.play(videoId, settings.value.maxHeight.toUInt(), audioOnly, fromStart)
    }

    private suspend fun resolve(core: YtLite, id: String, audioOnly: Boolean, fromStart: Boolean) =
        io { core.play(id, settings.value.maxHeight.toUInt(), audioOnly, fromStart) }

    /** Saves a resume point, or marks the video watched once it's finished.
     *  `refresh` reloads the lists so their progress bars catch up. */
    fun saveProgress(videoId: String, positionSecs: Double, durationSecs: Double?, refresh: Boolean) {
        val core = core ?: return
        scope.launch {
            runCatching { io { core.saveProgress(videoId, positionSecs, durationSecs) } }
                .onFailure { Log.w(TAG, "saving progress of $videoId", it) }
            if (refresh) _libraryVersion.update { it + 1 }
        }
    }

    // Account

    /** Opens Google's consent page in a Custom Tab. The core listens on a
     *  loopback port for the redirect, then sends the tab to RETURN_URL,
     *  which brings MainActivity back over it. */
    fun signIn(context: Context) {
        val core = core ?: return
        if (_signingIn.value) return
        _credentialProblem.value?.let {
            _messages.tryEmit("Can't save the sign-in: $it")
            return
        }
        _signingIn.value = true
        scope.launch {
            try {
                val pending = io { core.startSignIn(RETURN_URL) }
                pendingSignIn = pending
                CustomTabsIntent.Builder().setShowTitle(true).build()
                    .launchUrl(context, Uri.parse(pending.url()))
                io { core.finishSignIn(pending) }
                _signedIn.value = true
            } catch (e: FfiException.Failed) {
                if ("cancelled" !in e.reason) _messages.tryEmit(e.reason)
            } catch (e: Exception) {
                _messages.tryEmit(describe(e))
            } finally {
                pendingSignIn = null
                _signingIn.value = false
            }
        }
    }

    fun cancelSignIn() {
        pendingSignIn?.cancel()
    }

    fun signOut() {
        val core = core ?: return
        scope.launch {
            runCatching { io { core.signOut() } }
            _signedIn.value = false
        }
    }

    private fun now() = android.os.SystemClock.elapsedRealtime()

    companion object {
        private const val TAG = "AppModel"
        const val RETURN_URL = "ytlite://signed-in"
        private const val STALE_AFTER_MS = 15 * 60 * 1000L
    }
}

/** Runs blocking core work off the main thread. */
suspend fun <T> io(block: () -> T): T = withContext(Dispatchers.IO) { block() }

/** A readable message for errors from the Rust core. */
fun describe(e: Throwable): String = when (e) {
    is FfiException.NotSignedIn -> "Sign in with Google in Settings to load your subscriptions."
    is FfiException.Failed -> e.reason
    else -> e.message ?: e.javaClass.simpleName
}
