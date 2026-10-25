package local.ytlite.ui

import android.text.format.Formatter
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.DownloadForOffline
import androidx.compose.material.icons.outlined.Headphones
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.SmartDisplay
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import coil3.compose.AsyncImage
import kotlinx.coroutines.launch
import local.ytlite.core.AppModel
import local.ytlite.core.thumbnailUrl
import uniffi.yt_lite_ffi.Download
import uniffi.yt_lite_ffi.DownloadProgress
import uniffi.yt_lite_ffi.DownloadState
import uniffi.yt_lite_ffi.OfflineStorage

/** Reloads when downloads come, go, finish or fail; progress comes from the model. */
class DownloadsViewModel(private val model: AppModel) : ViewModel() {
    var items by mutableStateOf(emptyList<Download>())
        private set
    var storage by mutableStateOf<OfflineStorage?>(null)
        private set
    var loaded by mutableStateOf(false)
        private set

    init {
        viewModelScope.launch {
            model.downloadsVersion.collect {
                runCatching { model.downloads() }.onSuccess { items = it }
                storage = model.offlineStorage()
                loaded = true
            }
        }
    }
}

/** Opened from You: what's downloading, videos you downloaded, smart
 *  downloads, and the space they take. Back returns to You. */
@Composable
fun DownloadsScreen(model: AppModel, onBack: () -> Unit) {
    val vm = viewModel { DownloadsViewModel(model) }
    val progress by model.downloadProgress.collectAsStateWithLifecycle()
    val settings by model.settings.collectAsStateWithLifecycle()
    val context = LocalContext.current
    fun size(bytes: ULong) = Formatter.formatShortFileSize(context, bytes.toLong())
    BackHandler(onBack = onBack)

    ScreenScaffold(
        "Downloads",
        navigationIcon = {
            IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back") }
        },
    ) { modifier ->
        val pending = vm.items.filter { it.state != DownloadState.DONE }
        val saved = vm.items.filter { it.state == DownloadState.DONE && !it.smart }
        val smart = vm.items.filter { it.state == DownloadState.DONE && it.smart }
        LazyColumn(modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 24.dp)) {
            if (vm.loaded && vm.items.isEmpty()) {
                item {
                    EmptyState(
                        Icons.Outlined.DownloadForOffline,
                        "No downloads",
                        "Long-press a video and choose Download to watch it offline. Smart downloads " +
                            "(in Settings) save new videos for you while your phone charges.",
                    )
                }
            }
            if (pending.isNotEmpty()) {
                item { Section("Downloading") }
                items(pending, key = { "pending:" + it.video.id }) { DownloadRow(model, it, progress, ::size) }
            }
            if (saved.isNotEmpty()) {
                item { Section("Downloaded") }
                items(saved, key = { "saved:" + it.video.id }) { DownloadRow(model, it, progress, ::size) }
            }
            if (smart.isNotEmpty()) {
                item { Section("Smart downloads") }
                items(smart, key = { "smart:" + it.video.id }) { DownloadRow(model, it, progress, ::size) }
                item {
                    Text(
                        "Picked from your subscriptions and For you. Each is replaced once you've watched " +
                            "it. Long-press one to keep it.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                    )
                }
            }
            val storage = vm.storage
            if (storage != null && vm.items.isNotEmpty()) {
                item { Section("Storage") }
                item { ListItem(headlineContent = { Text("Downloaded") }, trailingContent = { Text(size(storage.savedBytes)) }) }
                if (settings.smartDownloads || storage.smartBytes > 0u) {
                    item {
                        ListItem(
                            headlineContent = { Text("Smart downloads") },
                            trailingContent = { Text("${size(storage.smartBytes)} of ${settings.smartGigabytes} GB") },
                        )
                    }
                }
                storage.freeBytes?.let { free ->
                    item { ListItem(headlineContent = { Text("Free on this device") }, trailingContent = { Text(size(free)) }) }
                }
            }
        }
    }
}

/** Tap to play; long-press for more; the trailing button deletes or cancels. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun DownloadRow(model: AppModel, download: Download, progress: DownloadProgress?, size: (ULong) -> String) {
    var menu by remember { mutableStateOf(false) }
    val video = download.video
    val done = download.state == DownloadState.DONE
    val active = progress?.takeIf { it.videoId == video.id }
    val audio = if (download.audioOnly) " · Audio only" else ""
    val detail = when {
        done -> "${video.channel} · ${size(download.bytes)}$audio"
        download.state == DownloadState.FAILED -> download.error ?: "Couldn't download"
        active != null && active.totalBytes > 0u -> "${size(active.doneBytes)} of ${size(active.totalBytes)}$audio"
        active != null -> "Starting…"
        else -> download.error?.let { "Waiting · $it" } ?: "Waiting$audio"
    }
    val fraction = when {
        done || download.state == DownloadState.FAILED -> null
        active != null && active.totalBytes > 0u -> active.doneBytes.toFloat() / active.totalBytes.toFloat()
        else -> null
    }
    Box {
        ListItem(
            headlineContent = {
                Text(video.title, maxLines = 2, overflow = TextOverflow.Ellipsis, fontWeight = FontWeight.SemiBold)
            },
            supportingContent = {
                Column {
                    Text(
                        detail,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        color = if (download.state == DownloadState.FAILED) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                    if (fraction != null) {
                        LinearProgressIndicator(progress = { fraction }, Modifier.fillMaxWidth().padding(top = 6.dp))
                    } else if (active != null) {
                        LinearProgressIndicator(Modifier.fillMaxWidth().padding(top = 6.dp))
                    }
                }
            },
            leadingContent = {
                Box(
                    Modifier
                        .width(112.dp)
                        .aspectRatio(16f / 9f)
                        .clip(RoundedCornerShape(8.dp))
                        .background(MaterialTheme.colorScheme.surfaceVariant)
                ) {
                    AsyncImage(
                        model = video.thumbnailUrl,
                        contentDescription = null,
                        contentScale = ContentScale.Crop,
                        modifier = Modifier.fillMaxSize(),
                    )
                }
            },
            trailingContent = {
                IconButton(onClick = { model.removeDownload(video) }) {
                    if (done) Icon(Icons.Outlined.Delete, "Delete download") else Icon(Icons.Outlined.Close, "Cancel download")
                }
            },
            modifier = Modifier
                .alpha(if (done && video.watched) 0.55f else 1f)
                .combinedClickable(
                    onClick = { if (done) model.play(video) },
                    onLongClick = { menu = true },
                    onLongClickLabel = "More options",
                ),
        )
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            if (done) {
                MenuItem("Watch", Icons.Outlined.SmartDisplay) { menu = false; model.play(video, audioOnly = false) }
                MenuItem("Listen (audio only)", Icons.Outlined.Headphones) { menu = false; model.play(video, audioOnly = true) }
            }
            if (download.smart) {
                MenuItem("Keep downloaded", Icons.Outlined.PushPin) { menu = false; model.keepDownload(video) }
            }
            if (download.state == DownloadState.FAILED) {
                MenuItem("Try again", Icons.Outlined.Refresh) { menu = false; model.download(video, download.audioOnly) }
            }
            MenuItem(if (done) "Delete download" else "Cancel download", Icons.Outlined.Delete) {
                menu = false
                model.removeDownload(video)
            }
        }
    }
}
