package local.ytlite.playback

import android.app.PendingIntent
import android.content.Intent
import androidx.core.content.ContextCompat
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MimeTypes
import androidx.media3.common.Player
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.session.CommandButton
import androidx.media3.session.DefaultMediaNotificationProvider
import androidx.media3.session.LibraryResult
import androidx.media3.session.MediaLibraryService
import androidx.media3.session.MediaSession
import androidx.media3.session.SessionError
import com.google.common.collect.ImmutableList
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import com.google.common.util.concurrent.MoreExecutors
import java.util.concurrent.Executors
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.launch
import local.ytlite.R
import local.ytlite.core.describe
import local.ytlite.model
import local.ytlite.ui.MainActivity
import uniffi.yt_lite_ffi.Video

/**
 * Plays everything: video and audio only from the phone UI, and Android
 * Auto, whose browse tree ([LibraryTree]) it serves. Media3 provides the
 * notification, lock-screen and Bluetooth controls. Saves where playback got
 * to, so videos resume there and count as watched once finished.
 */
class PlaybackService : MediaLibraryService() {
    private lateinit var session: MediaLibrarySession
    private lateinit var tree: LibraryTree
    private val scope = MainScope()
    /** Core calls block (network, SQLite). */
    private val background = MoreExecutors.listeningDecorator(Executors.newCachedThreadPool())

    override fun onCreate() {
        super.onCreate()
        val player = ExoPlayer.Builder(this)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(C.USAGE_MEDIA)
                    .setContentType(C.AUDIO_CONTENT_TYPE_MOVIE)
                    .build(),
                /* handleAudioFocus = */ true,
            )
            .setHandleAudioBecomingNoisy(true)
            .setWakeMode(C.WAKE_MODE_NETWORK)
            .setSeekBackIncrementMs(10_000)
            .setSeekForwardIncrementMs(30_000)
            .build()
        player.addListener(object : Player.Listener {
            // Paused (by the user, a call, unplugged headphones) or finished.
            override fun onIsPlayingChanged(isPlaying: Boolean) {
                if (!isPlaying) saveProgress(refresh = true)
            }

            override fun onPlaybackStateChanged(state: Int) {
                if (state == Player.STATE_ENDED) saveProgress(refresh = true)
            }
        })
        tree = LibraryTree(model) { parentId -> session.notifyChildrenChanged(parentId, Int.MAX_VALUE, null) }
        session = MediaLibrarySession.Builder(this, player, Callback())
            .setSessionActivity(
                PendingIntent.getActivity(
                    this, 0, Intent(this, MainActivity::class.java),
                    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                )
            )
            // Skip buttons instead of previous/next (notification, Android Auto).
            .setMediaButtonPreferences(
                listOf(
                    skipButton(CommandButton.ICON_SKIP_BACK_10, Player.COMMAND_SEEK_BACK, "Back 10 seconds", CommandButton.SLOT_BACK),
                    skipButton(CommandButton.ICON_SKIP_FORWARD_30, Player.COMMAND_SEEK_FORWARD, "Forward 30 seconds", CommandButton.SLOT_FORWARD),
                )
            )
            .build()
        setMediaNotificationProvider(
            DefaultMediaNotificationProvider.Builder(this).build().apply {
                setSmallIcon(R.drawable.ic_notification)
            }
        )
        // Often enough that little is lost if Android ends the app.
        scope.launch {
            while (true) {
                delay(10_000)
                if (player.isPlaying) saveProgress(refresh = false)
            }
        }
        // Android Auto's lists show progress; refresh them when it changes.
        scope.launch {
            model.libraryVersion.drop(1).collect {
                for (id in listOf(LibraryTree.SUBSCRIPTIONS, LibraryTree.FOR_YOU, LibraryTree.HISTORY)) {
                    session.notifyChildrenChanged(id, Int.MAX_VALUE, null)
                }
            }
        }
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaLibrarySession = session

    override fun onTaskRemoved(rootIntent: Intent?) {
        val player = session.player
        if (!player.playWhenReady || player.mediaItemCount == 0) stopSelf()
    }

    override fun onDestroy() {
        saveProgress(refresh = true)
        scope.cancel()
        session.player.release()
        session.release()
        background.shutdown()
        super.onDestroy()
    }

    /** Saves the current video's position: a resume point, or watched once
     *  it's finished. `refresh` reloads the lists so their progress catches up. */
    private fun saveProgress(refresh: Boolean) {
        val player = session.player
        val item = player.currentMediaItem ?: return
        // Live streams have no position to come back to. (ExoPlayer reports
        // the start position before anything has loaded, and keeps the
        // position after stop(), so every state is safe to save.)
        if (VideoItems.isLive(item)) return
        val duration = player.duration.takeIf { it != C.TIME_UNSET && it > 0 }?.div(1000.0)
        val position = if (player.playbackState == Player.STATE_ENDED) {
            duration ?: (player.currentPosition / 1000.0)
        } else {
            player.currentPosition / 1000.0
        }
        model.saveProgress(item.mediaId, position, duration, refresh)
    }

    /**
     * A stream for an item a controller wants played, and where to start.
     * The phone UI sends items it already resolved; Android Auto sends
     * browse items, which play audio only from their resume point.
     */
    private fun resolve(item: MediaItem): Pair<MediaItem, Long> {
        val uri = item.localConfiguration?.uri ?: item.requestMetadata.mediaUri
        if (uri != null) {
            return item.buildUpon().setUri(uri).setMimeType(MimeTypes.APPLICATION_M3U8).build() to C.TIME_UNSET
        }
        val playable = model.resolveBlocking(item.mediaId, audioOnly = true)
        val video = tree.video(item.mediaId) ?: Video(
            id = item.mediaId,
            title = item.mediaMetadata.title?.toString() ?: playable.title,
            channel = item.mediaMetadata.artist?.toString().orEmpty(),
            published = 0,
            durationSecs = null,
            live = playable.isLive,
            watched = false,
            progress = null,
            lastPlayed = null,
        )
        return VideoItems.playable(video, playable) to (playable.startSecs * 1000).toLong()
    }

    private inner class Callback : MediaLibrarySession.Callback {
        override fun onGetLibraryRoot(
            session: MediaLibrarySession,
            browser: MediaSession.ControllerInfo,
            params: LibraryParams?,
        ): ListenableFuture<LibraryResult<MediaItem>> = Futures.immediateFuture(
            LibraryResult.ofItem(LibraryTree.root(), LibraryParams.Builder().setExtras(LibraryTree.rootExtras()).build())
        )

        override fun onGetChildren(
            session: MediaLibrarySession,
            browser: MediaSession.ControllerInfo,
            parentId: String,
            page: Int,
            pageSize: Int,
            params: LibraryParams?,
        ): ListenableFuture<LibraryResult<ImmutableList<MediaItem>>> = background.submit<LibraryResult<ImmutableList<MediaItem>>> {
            val children = try {
                tree.children(parentId)
            } catch (e: Exception) {
                return@submit LibraryResult.ofError(SessionError(SessionError.ERROR_UNKNOWN, describe(e)))
            } ?: return@submit LibraryResult.ofError(SessionError.ERROR_BAD_VALUE)
            LibraryResult.ofItemList(children.page(page, pageSize), params)
        }

        override fun onGetItem(
            session: MediaLibrarySession,
            browser: MediaSession.ControllerInfo,
            mediaId: String,
        ): ListenableFuture<LibraryResult<MediaItem>> = background.submit<LibraryResult<MediaItem>> {
            tree.item(mediaId)?.let { LibraryResult.ofItem(it, null) }
                ?: LibraryResult.ofError(SessionError.ERROR_BAD_VALUE)
        }

        override fun onSearch(
            session: MediaLibrarySession,
            browser: MediaSession.ControllerInfo,
            query: String,
            params: LibraryParams?,
        ): ListenableFuture<LibraryResult<Void>> {
            background.execute {
                val count = runCatching { tree.search(query).size }.getOrDefault(0)
                ContextCompat.getMainExecutor(this@PlaybackService).execute {
                    session.notifySearchResultChanged(browser, query, count, params)
                }
            }
            return Futures.immediateFuture(LibraryResult.ofVoid())
        }

        override fun onGetSearchResult(
            session: MediaLibrarySession,
            browser: MediaSession.ControllerInfo,
            query: String,
            page: Int,
            pageSize: Int,
            params: LibraryParams?,
        ): ListenableFuture<LibraryResult<ImmutableList<MediaItem>>> = background.submit<LibraryResult<ImmutableList<MediaItem>>> {
            LibraryResult.ofItemList(tree.search(query).page(page, pageSize), params)
        }

        override fun onAddMediaItems(
            mediaSession: MediaSession,
            controller: MediaSession.ControllerInfo,
            mediaItems: MutableList<MediaItem>,
        ): ListenableFuture<MutableList<MediaItem>> = background.submit<MutableList<MediaItem>> {
            resolveOrReport(mediaSession, controller, mediaItems).map { it.first }.toMutableList()
        }

        override fun onSetMediaItems(
            mediaSession: MediaSession,
            controller: MediaSession.ControllerInfo,
            mediaItems: MutableList<MediaItem>,
            startIndex: Int,
            startPositionMs: Long,
        ): ListenableFuture<MediaSession.MediaItemsWithStartPosition> {
            // The video being replaced: its final position.
            saveProgress(refresh = true)
            return background.submit<MediaSession.MediaItemsWithStartPosition> {
                val resolved = resolveOrReport(mediaSession, controller, mediaItems)
                val index = if (startIndex == C.INDEX_UNSET) 0 else startIndex
                val start = if (startPositionMs != C.TIME_UNSET) startPositionMs else resolved.getOrNull(index)?.second ?: 0
                MediaSession.MediaItemsWithStartPosition(resolved.map { it.first }, index, start)
            }
        }

        /** Android Auto shows the error (e.g. YouTube refusing a video). */
        private fun resolveOrReport(
            session: MediaSession,
            controller: MediaSession.ControllerInfo,
            items: List<MediaItem>,
        ): List<Pair<MediaItem, Long>> =
            try {
                items.map(::resolve)
            } catch (e: Exception) {
                session.sendError(controller, SessionError(SessionError.ERROR_UNKNOWN, describe(e)))
                throw e
            }
    }

    private fun skipButton(icon: Int, command: Int, name: String, slot: Int) =
        CommandButton.Builder(icon)
            .setPlayerCommand(command)
            .setDisplayName(name)
            .setSlots(slot)
            .build()
}

/** One page of a browse list (Android Auto may ask for pages). */
private fun List<MediaItem>.page(page: Int, pageSize: Int): ImmutableList<MediaItem> {
    if (page < 0 || pageSize <= 0 || pageSize == Int.MAX_VALUE) return ImmutableList.copyOf(this)
    val from = page.toLong() * pageSize
    if (from >= size) return ImmutableList.of()
    return ImmutableList.copyOf(subList(from.toInt(), minOf(size, from.toInt() + pageSize)))
}
