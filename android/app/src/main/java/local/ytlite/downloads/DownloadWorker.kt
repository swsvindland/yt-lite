package local.ytlite.downloads

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Build
import android.text.format.Formatter
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import androidx.work.ForegroundInfo
import androidx.work.Worker
import androidx.work.WorkerParameters
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.runBlocking
import local.ytlite.R
import local.ytlite.model
import local.ytlite.ui.MainActivity
import uniffi.yt_lite_ffi.CancelToken
import uniffi.yt_lite_ffi.DownloadProgress
import uniffi.yt_lite_ffi.RunOutcome
import uniffi.yt_lite_ffi.SmartDownloads

/**
 * Runs the core's downloads (`runDownloads`, which blocks) for WorkManager:
 * the ones you asked for, then smart downloads if [SMART]. Shows progress in
 * a notification (as a foreground service), except for smart downloads in
 * the background. When WorkManager stops the work (the charger or Wi-Fi went
 * away, time's up), the run stops; partial downloads continue next time.
 */
class DownloadWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
    private val cancel = CancelToken()

    override fun doWork(): Result {
        val model = applicationContext.model
        val core = model.core ?: return Result.failure()
        // Straight from storage: this may be a fresh process WorkManager started.
        val settings = runBlocking { model.storedSettings() }
        val smart = inputData.getBoolean(SMART, false)
        if (smart && !settings.smartDownloads) return Result.success()
        val policy = if (smart) {
            SmartDownloads(
                budgetBytes = settings.smartGigabytes.toULong() shl 30,
                maxHeight = settings.downloadQuality.toUInt(),
                // Listeners get the sound only: far smaller.
                audioOnly = settings.audioOnly,
            )
        } else {
            null
        }
        val notification = if (!smart || inputData.getBoolean(NOW, false)) ProgressNotification(smart) else null
        val ticker = notification?.let { n ->
            try {
                setForegroundAsync(n.foregroundInfo(null)).get()
            } catch (e: Exception) {
                // Started in the background on Android 12+: no foreground service.
                Log.w(TAG, "downloading without a notification", e)
            }
            Executors.newSingleThreadScheduledExecutor().apply {
                scheduleWithFixedDelay({ n.show(core.downloadProgress()) }, 1, 1, TimeUnit.SECONDS)
            }
        }
        return try {
            val result = core.runDownloads(policy, cancel)
            when (result.outcome) {
                // Yielded: a download you asked for took over; carry on after it.
                RunOutcome.OFFLINE, RunOutcome.STOPPED, RunOutcome.YIELDED -> Result.retry()
                else -> Result.success()
            }
        } catch (e: Exception) {
            Log.w(TAG, "downloads failed", e)
            Result.retry()
        } finally {
            ticker?.shutdownNow()
        }
    }

    override fun onStopped() {
        cancel.cancel()
    }

    private inner class ProgressNotification(private val smart: Boolean) {
        private val context = applicationContext
        private val manager = context.getSystemService(NotificationManager::class.java)

        init {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL, "Downloads", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "Progress of videos downloading for offline playback"
                }
            )
        }

        fun foregroundInfo(progress: DownloadProgress?): ForegroundInfo =
            if (Build.VERSION.SDK_INT >= 29) {
                ForegroundInfo(NOTIFICATION_ID, build(progress), ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
            } else {
                ForegroundInfo(NOTIFICATION_ID, build(progress))
            }

        fun show(progress: DownloadProgress?) {
            val allowed = Build.VERSION.SDK_INT < 33 ||
                ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
                PackageManager.PERMISSION_GRANTED
            if (allowed && !isStopped) manager.notify(NOTIFICATION_ID, build(progress))
        }

        private fun build(progress: DownloadProgress?): Notification {
            val title = progress?.title?.takeIf { it.isNotEmpty() }
                ?: if (smart) "Updating smart downloads" else "Downloading videos"
            val total = progress?.totalBytes?.toLong() ?: 0
            val done = progress?.doneBytes?.toLong() ?: 0
            val more = (progress?.waiting?.toInt() ?: 0) - 1
            val text = buildString {
                append(
                    if (total > 0) {
                        "${Formatter.formatShortFileSize(context, done)} of ${Formatter.formatShortFileSize(context, total)}"
                    } else {
                        "Starting…"
                    }
                )
                if (more > 0) append(" · $more more")
            }
            val open = PendingIntent.getActivity(
                context, 0, Intent(context, MainActivity::class.java),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            return NotificationCompat.Builder(context, CHANNEL)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(title)
                .setContentText(text)
                .setProgress(1000, if (total > 0) (done * 1000 / total).toInt() else 0, total == 0L)
                .setOngoing(true)
                .setSilent(true)
                .setCategory(NotificationCompat.CATEGORY_PROGRESS)
                .setContentIntent(open)
                .build()
        }
    }

    companion object {
        const val SMART = "smart"
        /** Smart downloads from "Update now": shown, and on Wi-Fi without the charger. */
        const val NOW = "now"
        private const val CHANNEL = "downloads"
        private const val NOTIFICATION_ID = 42
        private const val TAG = "DownloadWorker"
    }
}
