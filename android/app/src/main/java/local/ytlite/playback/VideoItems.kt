package local.ytlite.playback

import android.net.Uri
import android.os.Bundle
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.MimeTypes
import androidx.media3.session.MediaConstants
import uniffi.yt_lite_ffi.Playable
import uniffi.yt_lite_ffi.Video

/** Media3 items for videos: Android Auto's browse lists, and what plays. */
object VideoItems {
    const val EXTRA_AUDIO_ONLY = "local.ytlite.AUDIO_ONLY"
    const val EXTRA_LIVE = "local.ytlite.LIVE"

    fun metadata(video: Video, audioOnly: Boolean): MediaMetadata =
        MediaMetadata.Builder()
            .setTitle(video.title)
            .setArtist(video.channel)
            .setArtworkUri(ArtworkProvider.uri(video.id))
            .setDurationMs(video.durationSecs?.toLong()?.takeIf { it > 0 }?.times(1000))
            .setIsPlayable(true)
            .setIsBrowsable(false)
            .setMediaType(MediaMetadata.MEDIA_TYPE_VIDEO)
            .setExtras(Bundle().apply {
                putBoolean(EXTRA_AUDIO_ONLY, audioOnly)
                putBoolean(EXTRA_LIVE, video.live)
                // Android Auto shows how much of each video was played.
                val progress = video.progress
                when {
                    progress != null -> {
                        putInt(MediaConstants.EXTRAS_KEY_COMPLETION_STATUS, MediaConstants.EXTRAS_VALUE_COMPLETION_STATUS_PARTIALLY_PLAYED)
                        putDouble(MediaConstants.EXTRAS_KEY_COMPLETION_PERCENTAGE, progress)
                    }
                    video.watched -> putInt(MediaConstants.EXTRAS_KEY_COMPLETION_STATUS, MediaConstants.EXTRAS_VALUE_COMPLETION_STATUS_FULLY_PLAYED)
                    else -> putInt(MediaConstants.EXTRAS_KEY_COMPLETION_STATUS, MediaConstants.EXTRAS_VALUE_COMPLETION_STATUS_NOT_PLAYED)
                }
            })
            .build()

    /** A video in a browse list (Android Auto plays it audio only). */
    fun browsable(video: Video): MediaItem =
        MediaItem.Builder()
            .setMediaId(video.id)
            .setMediaMetadata(metadata(video, audioOnly = true))
            .build()

    /** A video with its resolved stream (HLS). */
    fun playable(video: Video, playable: Playable): MediaItem =
        MediaItem.Builder()
            .setMediaId(video.id)
            .setUri(playable.url)
            .setMimeType(MimeTypes.APPLICATION_M3U8)
            .setMediaMetadata(metadata(video, playable.audioOnly))
            // Passed on to PlaybackService even where the URI itself isn't.
            .setRequestMetadata(
                MediaItem.RequestMetadata.Builder().setMediaUri(Uri.parse(playable.url)).build()
            )
            .build()

    fun isAudioOnly(item: MediaItem?): Boolean =
        item?.mediaMetadata?.extras?.getBoolean(EXTRA_AUDIO_ONLY, true) ?: true

    fun isLive(item: MediaItem?): Boolean =
        item?.mediaMetadata?.extras?.getBoolean(EXTRA_LIVE, false) ?: false
}
