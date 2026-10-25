package local.ytlite.ui

import android.content.Intent
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyGridScope
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.DownloadForOffline
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Download
import androidx.compose.material.icons.outlined.DownloadForOffline
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.Headphones
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.RemoveCircleOutline
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Replay
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.SmartDisplay
import androidx.compose.material.icons.outlined.Visibility
import androidx.compose.material.icons.outlined.VisibilityOff
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import coil3.compose.AsyncImage
import local.ytlite.core.AppModel
import local.ytlite.core.ageText
import local.ytlite.core.durationText
import local.ytlite.core.thumbnailUrl
import local.ytlite.core.watchUrl
import local.ytlite.core.watchedText
import uniffi.yt_lite_ffi.Offline
import uniffi.yt_lite_ffi.Video

/** Adaptive grid of video cards: one column on phones, more on tablets.
 *  `header` goes full width above the cards; `empty` shows when there are none.
 *  `inHistory`: cards say when they were watched and can be removed. */
@Composable
fun VideoGrid(
    model: AppModel,
    videos: List<Video>,
    modifier: Modifier = Modifier,
    inHistory: Boolean = false,
    header: LazyGridScope.() -> Unit = {},
    empty: (@Composable () -> Unit)? = null,
) {
    LazyVerticalGrid(
        columns = GridCells.Adaptive(300.dp),
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 24.dp),
        horizontalArrangement = Arrangement.spacedBy(16.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp),
    ) {
        header()
        if (videos.isEmpty() && empty != null) {
            item(span = { GridItemSpan(maxLineSpan) }) { empty() }
        }
        items(videos, key = { it.id }) { VideoCard(model, it, inHistory) }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun VideoCard(model: AppModel, video: Video, inHistory: Boolean = false) {
    var menu by remember { mutableStateOf(false) }
    val context = LocalContext.current
    Box {
        Column(
            Modifier
                .alpha(if (video.watched) 0.55f else 1f)
                .combinedClickable(
                    onClick = { model.play(video) },
                    onLongClick = { menu = true },
                    onLongClickLabel = "More options",
                )
        ) {
            Box(
                Modifier
                    .fillMaxWidth()
                    .aspectRatio(16f / 9f)
                    .clip(RoundedCornerShape(12.dp))
                    .background(MaterialTheme.colorScheme.surfaceVariant)
            ) {
                AsyncImage(
                    model = video.thumbnailUrl,
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier.fillMaxSize(),
                )
                video.durationText?.let {
                    Text(
                        it,
                        color = Color.White,
                        style = MaterialTheme.typography.labelSmall,
                        fontWeight = FontWeight.SemiBold,
                        modifier = Modifier
                            .align(Alignment.BottomEnd)
                            .padding(8.dp)
                            .background(
                                if (video.live) BrandRed else Color.Black.copy(alpha = 0.78f),
                                RoundedCornerShape(5.dp),
                            )
                            .padding(horizontal = 6.dp, vertical = 2.dp),
                    )
                }
                (video.progress ?: if (video.watched) 1.0 else null)?.let {
                    ProgressBar(it.toFloat(), Modifier.align(Alignment.BottomStart))
                }
                OfflineBadge(video.offline, Modifier.align(Alignment.TopStart).padding(8.dp))
            }
            Spacer(Modifier.height(8.dp))
            Text(
                video.title,
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.SemiBold,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                "${video.channel} · ${(if (inHistory) video.watchedText else null) ?: video.ageText}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
            MenuItem("Watch", Icons.Outlined.SmartDisplay) { menu = false; model.play(video, audioOnly = false) }
            MenuItem("Listen (audio only)", Icons.Outlined.Headphones) { menu = false; model.play(video, audioOnly = true) }
            if (video.progress != null) {
                MenuItem("Start over", Icons.Outlined.Replay) { menu = false; model.play(video, fromStart = true) }
            }
            if (video.watched) {
                MenuItem("Mark as unwatched", Icons.Outlined.VisibilityOff) { menu = false; model.setWatched(video, false) }
            } else {
                MenuItem("Mark as watched", Icons.Outlined.Visibility) { menu = false; model.setWatched(video, true) }
            }
            DownloadMenuItems(model, video) { menu = false }
            MenuItem("Share", Icons.Outlined.Share) {
                menu = false
                val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, video.watchUrl)
                context.startActivity(Intent.createChooser(send, null))
            }
            if (inHistory) {
                MenuItem("Remove from history", Icons.Outlined.RemoveCircleOutline) {
                    menu = false
                    model.removeFromHistory(video)
                }
            }
        }
    }
}

@Composable
internal fun MenuItem(label: String, icon: ImageVector, onClick: () -> Unit) {
    DropdownMenuItem(text = { Text(label) }, leadingIcon = { Icon(icon, null) }, onClick = onClick)
}

/** Download, cancel, keep or delete, depending on what's downloaded. */
@Composable
private fun DownloadMenuItems(model: AppModel, video: Video, close: () -> Unit) {
    fun then(action: () -> Unit): () -> Unit = { close(); action() }
    when (video.offline) {
        Offline.NO -> {
            MenuItem("Download", Icons.Outlined.Download, then { model.download(video) })
            MenuItem("Download audio only", Icons.Outlined.Headphones, then { model.download(video, audioOnly = true) })
        }
        Offline.QUEUED -> MenuItem("Cancel download", Icons.Outlined.Close, then { model.removeDownload(video) })
        Offline.FAILED -> {
            MenuItem("Try downloading again", Icons.Outlined.Refresh, then { model.download(video) })
            MenuItem("Cancel download", Icons.Outlined.Close, then { model.removeDownload(video) })
        }
        Offline.SAVED -> MenuItem("Delete download", Icons.Outlined.Delete, then { model.removeDownload(video) })
        Offline.SMART -> {
            MenuItem("Keep downloaded", Icons.Outlined.PushPin, then { model.keepDownload(video) })
            MenuItem("Delete download", Icons.Outlined.Delete, then { model.removeDownload(video) })
        }
    }
}

/** On a thumbnail: downloaded, downloading or failed. */
@Composable
fun OfflineBadge(offline: Offline, modifier: Modifier = Modifier) {
    val (icon, label) = when (offline) {
        Offline.SAVED, Offline.SMART -> Icons.Filled.DownloadForOffline to "Downloaded"
        Offline.QUEUED -> Icons.Outlined.DownloadForOffline to "Downloading"
        Offline.FAILED -> Icons.Outlined.ErrorOutline to "Download failed"
        Offline.NO -> return
    }
    Icon(
        icon,
        contentDescription = label,
        tint = Color.White,
        modifier = modifier
            .background(Color.Black.copy(alpha = 0.7f), RoundedCornerShape(50))
            .padding(4.dp)
            .size(18.dp),
    )
}

/** How much of a video was played, along the bottom of its thumbnail. */
@Composable
fun ProgressBar(fraction: Float, modifier: Modifier = Modifier) {
    Box(
        modifier
            .fillMaxWidth()
            .height(4.dp)
            .background(Color.White.copy(alpha = 0.3f))
    ) {
        Box(
            Modifier
                .fillMaxHeight()
                .fillMaxWidth(fraction.coerceIn(0f, 1f))
                .background(BrandRed)
        )
    }
}

/** Centered icon and text for empty and error states. */
@Composable
fun EmptyState(
    icon: ImageVector,
    title: String,
    message: String,
    action: (@Composable () -> Unit)? = null,
) {
    Column(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 32.dp, vertical = 96.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Icon(icon, null, Modifier.size(48.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(title, style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center)
        Text(
            message,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = TextAlign.Center,
        )
        if (action != null) {
            Spacer(Modifier.height(8.dp))
            action()
        }
    }
}
