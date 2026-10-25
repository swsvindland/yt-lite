package local.ytlite.downloads

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.workDataOf
import java.util.concurrent.TimeUnit

/**
 * When downloads run. The Rust core does the work ([DownloadWorker]);
 * WorkManager decides when:
 *
 * - Downloads you ask for: right away on any connection, as a foreground
 *   service with a progress notification. Stopped ones continue when the
 *   connection is back.
 * - Smart downloads: every few hours while the phone is charging on an
 *   unmetered network (Wi-Fi), quietly. WorkManager stops them when the
 *   charger or Wi-Fi goes away, and the next run picks up where that one left
 *   off. "Update now" (Settings) runs them at once on Wi-Fi.
 */
object Downloads {
    private const val SAVED = "downloads"
    private const val SMART = "smart-downloads"
    private const val SMART_NOW = "smart-downloads-now"

    /** Runs the downloads you asked for. A run picks up anything asked for
     *  meanwhile; appending covers one that's just finishing. */
    fun runSaved(context: Context) {
        val request = OneTimeWorkRequestBuilder<DownloadWorker>()
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .setInputData(workDataOf(DownloadWorker.SMART to false))
            .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 1, TimeUnit.MINUTES)
            .build()
        WorkManager.getInstance(context).enqueueUniqueWork(SAVED, ExistingWorkPolicy.APPEND_OR_REPLACE, request)
    }

    /** Smart downloads now, on Wi-Fi (no charger needed). False if not on Wi-Fi. */
    fun updateSmartNow(context: Context): Boolean {
        if (!onUnmeteredNetwork(context)) return false
        val request = OneTimeWorkRequestBuilder<DownloadWorker>()
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.UNMETERED).build())
            .setInputData(workDataOf(DownloadWorker.SMART to true, DownloadWorker.NOW to true))
            .build()
        WorkManager.getInstance(context).enqueueUniqueWork(SMART_NOW, ExistingWorkPolicy.KEEP, request)
        return true
    }

    /** Smart downloads on or off (it's kept across restarts by WorkManager). */
    fun scheduleSmart(context: Context, enabled: Boolean) {
        val work = WorkManager.getInstance(context)
        if (!enabled) {
            work.cancelUniqueWork(SMART)
            work.cancelUniqueWork(SMART_NOW)
            return
        }
        val request = PeriodicWorkRequestBuilder<DownloadWorker>(3, TimeUnit.HOURS)
            .setConstraints(
                Constraints.Builder()
                    .setRequiredNetworkType(NetworkType.UNMETERED)
                    .setRequiresCharging(true)
                    .setRequiresStorageNotLow(true)
                    .build()
            )
            .setInputData(workDataOf(DownloadWorker.SMART to true))
            .build()
        work.enqueueUniquePeriodicWork(SMART, ExistingPeriodicWorkPolicy.UPDATE, request)
    }

    private fun onUnmeteredNetwork(context: Context): Boolean {
        val connectivity = context.getSystemService(ConnectivityManager::class.java)
        val capabilities = connectivity.getNetworkCapabilities(connectivity.activeNetwork) ?: return false
        return capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
            capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
    }
}
