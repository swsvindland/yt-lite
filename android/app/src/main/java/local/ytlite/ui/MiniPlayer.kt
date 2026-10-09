package local.ytlite.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material.icons.rounded.Forward30
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.Replay10
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.common.C
import coil3.compose.AsyncImage
import kotlinx.coroutines.delay
import local.ytlite.core.AppModel
import local.ytlite.core.clock
import local.ytlite.playback.PlayerConnection

/** Audio-only playback, above the navigation bar. */
@Composable
fun MiniPlayer(model: AppModel, now: PlayerConnection.NowPlaying) {
    val controller by model.player.controller.collectAsStateWithLifecycle()
    var position by remember { mutableLongStateOf(0) }
    var duration by remember { mutableLongStateOf(0) }
    LaunchedEffect(controller, now.videoId) {
        while (true) {
            controller?.let {
                position = it.currentPosition
                duration = it.duration.takeIf { d -> d != C.TIME_UNSET } ?: 0
            }
            delay(500)
        }
    }

    Surface(
        shape = RoundedCornerShape(16.dp),
        tonalElevation = 3.dp,
        shadowElevation = 3.dp,
        modifier = Modifier.padding(horizontal = 12.dp, vertical = 6.dp),
    ) {
        Column {
            Row(
                Modifier.padding(start = 10.dp, end = 4.dp, top = 8.dp, bottom = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                AsyncImage(
                    model = "https://i.ytimg.com/vi/${now.videoId}/mqdefault.jpg",
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier.size(64.dp, 36.dp).clip(RoundedCornerShape(6.dp)),
                )
                Column(Modifier.weight(1f).padding(horizontal = 12.dp)) {
                    Text(
                        now.title,
                        style = MaterialTheme.typography.labelLarge,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(
                        if (now.live) "LIVE · ${now.channel}"
                        else "${clock(position / 1000)} / ${clock(duration / 1000)} · ${now.channel}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                IconButton(onClick = model.player::skipBack) { Icon(Icons.Rounded.Replay10, "Back 10 seconds") }
                IconButton(onClick = model.player::togglePlay) {
                    Icon(
                        if (now.isPlaying) Icons.Rounded.Pause else Icons.Rounded.PlayArrow,
                        if (now.isPlaying) "Pause" else "Play",
                    )
                }
                IconButton(onClick = model.player::skipForward) { Icon(Icons.Rounded.Forward30, "Forward 30 seconds") }
                IconButton(onClick = model.player::stop) { Icon(Icons.Rounded.Close, "Close") }
            }
            LinearProgressIndicator(
                progress = { if (!now.live && duration > 0) (position.toFloat() / duration).coerceIn(0f, 1f) else 0f },
                modifier = Modifier.fillMaxWidth(),
                drawStopIndicator = {},
            )
        }
    }
}
