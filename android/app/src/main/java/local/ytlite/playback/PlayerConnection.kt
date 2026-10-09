package local.ytlite.playback

import android.content.ComponentName
import android.content.Context
import androidx.core.content.ContextCompat
import androidx.media3.common.C
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.session.MediaController
import androidx.media3.session.SessionToken
import com.google.common.util.concurrent.ListenableFuture
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.guava.await
import uniffi.yt_lite_ffi.Playable
import uniffi.yt_lite_ffi.Video

/**
 * The UI's connection to [PlaybackService]: a MediaController (PlayerView
 * renders video through it) and what's playing, for the mini player.
 */
class PlayerConnection(context: Context, private val onError: (String) -> Unit) {
    private val app = context.applicationContext
    private var future: ListenableFuture<MediaController>? = null

    private val _controller = MutableStateFlow<MediaController?>(null)
    val controller = _controller.asStateFlow()

    private val _nowPlaying = MutableStateFlow<NowPlaying?>(null)
    val nowPlaying = _nowPlaying.asStateFlow()

    data class NowPlaying(
        val videoId: String,
        val title: String,
        val channel: String,
        val audioOnly: Boolean,
        val live: Boolean,
        val isPlaying: Boolean,
    )

    private val listener = object : Player.Listener {
        override fun onEvents(player: Player, events: Player.Events) = update(player)

        override fun onPlayerError(error: PlaybackException) {
            onError("Playback failed: ${error.message ?: error.errorCodeName}")
        }
    }

    /** Connects once; MainActivity calls it at startup. */
    fun connect(): ListenableFuture<MediaController> {
        future?.let { return it }
        val token = SessionToken(app, ComponentName(app, PlaybackService::class.java))
        return MediaController.Builder(app, token).buildAsync().also { f ->
            future = f
            f.addListener({
                runCatching { f.get() }.onSuccess { c ->
                    c.addListener(listener)
                    _controller.value = c
                    update(c)
                }
            }, ContextCompat.getMainExecutor(app))
        }
    }

    suspend fun play(video: Video, playable: Playable) {
        val c = connect().await()
        c.setMediaItem(VideoItems.playable(video, playable), (playable.startSecs * 1000).toLong())
        c.prepare()
        c.play()
    }

    fun togglePlay() {
        val c = _controller.value ?: return
        if (c.isPlaying) c.pause() else c.play()
    }

    fun skipBack() {
        _controller.value?.seekBack()
    }

    fun skipForward() {
        _controller.value?.seekForward()
    }

    /** Stops and closes the player (the position is saved first). */
    fun stop() {
        val c = _controller.value ?: return
        c.stop()
        c.clearMediaItems()
    }

    /**
     * The app left the screen (and isn't in Picture in Picture). A playing
     * video keeps going as audio only (`keepPlaying`, Settings) or pauses.
     */
    fun onAppHidden(keepPlaying: Boolean) {
        val c = _controller.value ?: return
        val now = _nowPlaying.value ?: return
        if (now.audioOnly || !c.isPlaying) return
        if (keepPlaying) setVideoTrack(c, enabled = false) else c.pause()
    }

    fun onAppVisible() {
        _controller.value?.let { setVideoTrack(it, enabled = true) }
    }

    /** No video decoding (or downloading) while nobody can see it. */
    private fun setVideoTrack(c: MediaController, enabled: Boolean) {
        val params = c.trackSelectionParameters
        if ((C.TRACK_TYPE_VIDEO in params.disabledTrackTypes) == !enabled) return
        c.trackSelectionParameters = params.buildUpon().setTrackTypeDisabled(C.TRACK_TYPE_VIDEO, !enabled).build()
    }

    private fun update(player: Player) {
        val item = player.currentMediaItem
        _nowPlaying.value = if (item == null || player.playbackState == Player.STATE_IDLE) {
            null
        } else {
            NowPlaying(
                videoId = item.mediaId,
                title = item.mediaMetadata.title?.toString().orEmpty(),
                channel = item.mediaMetadata.artist?.toString().orEmpty(),
                audioOnly = VideoItems.isAudioOnly(item),
                live = VideoItems.isLive(item),
                isPlaying = player.isPlaying,
            )
        }
    }
}
