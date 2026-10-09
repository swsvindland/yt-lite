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
import androidx.compose.material.icons.outlined.Headphones
import androidx.compose.material.icons.outlined.Replay
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.SmartDisplay
import androidx.compose.material.icons.outlined.Visibility
import androidx.compose.material.icons.outlined.VisibilityOff
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
import uniffi.yt_lite_ffi.Video

/** Adaptive grid of video cards: one column on phones, more on tablets.
 *  `header` goes full width above the cards; `empty` shows when there are none. */
@Composable
fun VideoGrid(
    model: AppModel,
    videos: List<Video>,
    modifier: Modifier = Modifier,
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
        items(videos, key = { it.id }) { VideoCard(model, it) }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun VideoCard(model: AppModel, video: Video) {
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
                "${video.channel} · ${video.ageText}",
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
            MenuItem("Share", Icons.Outlined.Share) {
                menu = false
                val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, video.watchUrl)
                context.startActivity(Intent.createChooser(send, null))
            }
        }
    }
}

@Composable
private fun MenuItem(label: String, icon: ImageVector, onClick: () -> Unit) {
    DropdownMenuItem(text = { Text(label) }, leadingIcon = { Icon(icon, null) }, onClick = onClick)
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
