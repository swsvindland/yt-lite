package local.ytlite.playback

import android.os.Bundle
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.session.MediaConstants
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.launch
import local.ytlite.core.AppModel
import uniffi.yt_lite_ffi.Feed
import uniffi.yt_lite_ffi.Video

/**
 * Android Auto's browse tree: Subscriptions, For you and Explore (topic →
 * videos), shown as tabs, plus search. Blocking (core calls); PlaybackService
 * runs it on background threads. `onChanged(parentId)` fires when a feed
 * finished refreshing in the background.
 */
class LibraryTree(private val model: AppModel, private val onChanged: (String) -> Unit) {
    /** Videos listed so far, to fill in what Android Auto asks to play. */
    private val known = ConcurrentHashMap<String, Video>()

    fun video(id: String): Video? = known[id]

    fun item(id: String): MediaItem? = when (id) {
        ROOT -> root()
        SUBSCRIPTIONS -> folder(SUBSCRIPTIONS, "Subscriptions")
        FOR_YOU -> folder(FOR_YOU, "For you")
        EXPLORE -> folder(EXPLORE, "Explore")
        else -> known[id]?.let(VideoItems::browsable)
    }

    /** `null` for an unknown parent. */
    fun children(parentId: String): List<MediaItem>? = when {
        parentId == ROOT -> listOf(
            folder(SUBSCRIPTIONS, "Subscriptions"),
            folder(FOR_YOU, "For you"),
            folder(EXPLORE, "Explore"),
        )
        parentId == SUBSCRIPTIONS -> feed(Feed.SUBSCRIPTIONS, parentId)
        parentId == FOR_YOU -> feed(Feed.FOR_YOU, parentId)
        parentId == EXPLORE -> model.exploreTopics.mapIndexed { i, name -> folder("$TOPIC$i", name) }
        parentId.startsWith(TOPIC) -> parentId.removePrefix(TOPIC).toIntOrNull()
            ?.let { topic -> videos(model.core?.explore(topic.toUInt()).orEmpty()) }
        else -> null
    }

    fun search(query: String): List<MediaItem> = videos(model.core?.search(query).orEmpty())

    /** The cached feed now; if it's stale, a refresh from YouTube follows. */
    private fun feed(feed: Feed, parentId: String): List<MediaItem> {
        val core = model.core ?: return emptyList()
        if (model.isStale(feed)) {
            model.scope.launch {
                runCatching { model.refresh(feed, force = false) }.onSuccess { onChanged(parentId) }
            }
        }
        return videos(core.videos(feed, model.settings.value.hideWatched))
    }

    private fun videos(list: List<Video>): List<MediaItem> =
        list.take(MAX_ITEMS).map { video ->
            known[video.id] = video
            VideoItems.browsable(video)
        }

    companion object {
        const val ROOT = "root"
        const val SUBSCRIPTIONS = "subscriptions"
        const val FOR_YOU = "for_you"
        const val EXPLORE = "explore"
        private const val TOPIC = "topic:"
        private const val MAX_ITEMS = 100

        fun root(): MediaItem = folder(ROOT, "yt-lite")

        /** Lists with thumbnails; search in the car. */
        fun rootExtras(): Bundle = Bundle().apply {
            putInt(MediaConstants.EXTRAS_KEY_CONTENT_STYLE_BROWSABLE, MediaConstants.EXTRAS_VALUE_CONTENT_STYLE_LIST_ITEM)
            putInt(MediaConstants.EXTRAS_KEY_CONTENT_STYLE_PLAYABLE, MediaConstants.EXTRAS_VALUE_CONTENT_STYLE_LIST_ITEM)
            // The MediaBrowserService root hint Android Auto checks for search.
            putBoolean("android.media.browse.SEARCH_SUPPORTED", true)
        }

        private fun folder(id: String, title: String): MediaItem =
            MediaItem.Builder()
                .setMediaId(id)
                .setMediaMetadata(
                    MediaMetadata.Builder()
                        .setTitle(title)
                        .setIsBrowsable(true)
                        .setIsPlayable(false)
                        .setMediaType(MediaMetadata.MEDIA_TYPE_FOLDER_VIDEOS)
                        .build()
                )
                .build()
    }
}
