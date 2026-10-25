package local.ytlite.playback

import androidx.annotation.OptIn
import androidx.media3.common.C
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.DefaultLoadControl
import androidx.media3.exoplayer.LoadControl
import androidx.media3.exoplayer.analytics.PlayerId
import androidx.media3.exoplayer.source.TrackGroupArray
import androidx.media3.exoplayer.trackselection.ExoTrackSelection

/**
 * Loads audio in long bursts so the radio can sleep in between. ExoPlayer's
 * default keeps 50 s buffered by fetching the next few-second segment as soon
 * as one plays, which keeps the radio awake for as long as you listen. With
 * no video track selected (audio only, or a video in the background with its
 * video track off), this fills the buffer to [TARGET_US], then fetches nothing
 * until less than [RESUME_US] is left: one ~8 MB download every 8 minutes at
 * the ~130 kbps audio renditions. Video keeps ExoPlayer's defaults.
 *
 * A subclass rather than a delegating wrapper: Kotlin's `by` doesn't forward
 * LoadControl's Java default methods, whose fallbacks throw.
 */
@OptIn(UnstableApi::class)
class AudioBurstLoadControl : DefaultLoadControl() {
    /** Only the playback thread calls in. */
    private var audioOnly = false
    private var loading = false

    override fun onTracksSelected(
        parameters: LoadControl.Parameters,
        trackGroups: TrackGroupArray,
        trackSelections: Array<out ExoTrackSelection?>,
    ) {
        super.onTracksSelected(parameters, trackGroups, trackSelections)
        audioOnly = trackSelections.none { it?.trackGroup?.type == C.TRACK_TYPE_VIDEO }
    }

    override fun shouldContinueLoading(parameters: LoadControl.Parameters): Boolean {
        if (!audioOnly) return super.shouldContinueLoading(parameters)
        val buffered = parameters.bufferedDurationUs
        // Between the two thresholds, keep doing what we were doing.
        loading = when {
            buffered < RESUME_US -> true
            buffered >= TARGET_US -> false
            else -> loading
        }
        return loading
    }

    override fun onStopped(playerId: PlayerId) {
        loading = false
        super.onStopped(playerId)
    }

    override fun onReleased(playerId: PlayerId) {
        loading = false
        super.onReleased(playerId)
    }

    private companion object {
        const val TARGET_US = 10 * 60 * 1_000_000L
        /** Slack for a patchy connection (tunnels, the car). */
        const val RESUME_US = 2 * 60 * 1_000_000L
    }
}
