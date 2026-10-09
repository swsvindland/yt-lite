package local.ytlite.core

import android.icu.text.RelativeDateTimeFormatter
import android.icu.text.RelativeDateTimeFormatter.Direction
import android.icu.text.RelativeDateTimeFormatter.RelativeUnit
import uniffi.yt_lite_ffi.Video

val Video.thumbnailUrl: String get() = "https://i.ytimg.com/vi/$id/mqdefault.jpg"

val Video.watchUrl: String get() = "https://www.youtube.com/watch?v=$id"

/** "12:34", "1:02:03", or "LIVE". */
val Video.durationText: String?
    get() {
        if (live) return "LIVE"
        val secs = durationSecs?.toLong()?.takeIf { it > 0 } ?: return null
        return clock(secs)
    }

/** "3 days ago", "2 weeks ago" (localized). */
val Video.ageText: String get() = relativeTime(published)

/** "watched 2 hours ago" (History). */
val Video.watchedText: String? get() = lastPlayed?.let { "watched ${relativeTime(it)}" }

private fun relativeTime(unix: Long): String {
    val secs = (System.currentTimeMillis() / 1000 - unix).coerceAtLeast(0).toDouble()
    val (amount, unit) = when {
        secs < 3600 -> secs / 60 to RelativeUnit.MINUTES
        secs < 86_400 -> secs / 3600 to RelativeUnit.HOURS
        secs < 7 * 86_400 -> secs / 86_400 to RelativeUnit.DAYS
        secs < 30 * 86_400 -> secs / (7 * 86_400) to RelativeUnit.WEEKS
        secs < 365 * 86_400 -> secs / (30 * 86_400) to RelativeUnit.MONTHS
        else -> secs / (365 * 86_400) to RelativeUnit.YEARS
    }
    return RelativeDateTimeFormatter.getInstance().format(maxOf(1, amount.toInt()).toDouble(), Direction.LAST, unit)
}

fun clock(secs: Long): String {
    val s = secs.coerceAtLeast(0)
    val (h, m, rest) = Triple(s / 3600, (s % 3600) / 60, s % 60)
    return if (h > 0) "%d:%02d:%02d".format(h, m, rest) else "%d:%02d".format(m, rest)
}
