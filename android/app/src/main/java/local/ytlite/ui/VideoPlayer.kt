package local.ytlite.ui

import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.LocalActivity
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Close
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.app.PictureInPictureModeChangedInfo
import androidx.core.util.Consumer
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.media3.common.Player
import androidx.media3.ui.PlayerView

/**
 * Full-screen video over the app: Media3's PlayerView on the service's
 * player, immersive, and Picture in Picture when you leave the app. Back or
 * ✕ stops the video, like the iOS app's ✕.
 */
@Composable
fun VideoPlayer(player: Player, onClose: () -> Unit) {
    val activity = LocalActivity.current as MainActivity
    val inPip = rememberIsInPipMode(activity)
    var controlsVisible by remember { mutableStateOf(true) }
    BackHandler(onBack = onClose)

    DisposableEffect(activity) {
        activity.setVideoShowing(true)
        val window = activity.window
        val bars = WindowCompat.getInsetsController(window, window.decorView)
        bars.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        bars.hide(WindowInsetsCompat.Type.systemBars())
        onDispose {
            bars.show(WindowInsetsCompat.Type.systemBars())
            activity.setVideoShowing(false)
        }
    }

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        AndroidView(
            factory = { context ->
                PlayerView(context).apply {
                    this.player = player
                    setShowNextButton(false)
                    setShowPreviousButton(false)
                    keepScreenOn = true
                    setControllerVisibilityListener(
                        PlayerView.ControllerVisibilityListener { visibility ->
                            controlsVisible = visibility == android.view.View.VISIBLE
                        }
                    )
                }
            },
            update = { view ->
                view.player = player
                view.useController = !inPip
            },
            onRelease = { it.player = null },
            modifier = Modifier.fillMaxSize(),
        )
        AnimatedVisibility(
            visible = controlsVisible && !inPip,
            enter = fadeIn(),
            exit = fadeOut(),
            modifier = Modifier.align(Alignment.TopStart).safeDrawingPadding().padding(8.dp),
        ) {
            IconButton(onClick = onClose) { Icon(Icons.Rounded.Close, "Close", tint = Color.White) }
        }
    }
}

@Composable
private fun rememberIsInPipMode(activity: ComponentActivity): Boolean {
    var inPip by remember { mutableStateOf(activity.isInPictureInPictureMode) }
    DisposableEffect(activity) {
        val listener = Consumer<PictureInPictureModeChangedInfo> { inPip = it.isInPictureInPictureMode }
        activity.addOnPictureInPictureModeChangedListener(listener)
        onDispose { activity.removeOnPictureInPictureModeChangedListener(listener) }
    }
    return inPip
}
