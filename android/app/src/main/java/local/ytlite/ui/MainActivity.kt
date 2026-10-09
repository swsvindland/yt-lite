package local.ytlite.ui

import android.app.PictureInPictureParams
import android.os.Build
import android.os.Bundle
import android.util.Rational
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.getValue
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.launch
import local.ytlite.model

class MainActivity : ComponentActivity() {
    /** The video player is on screen: leaving the app shrinks it into Picture in Picture. */
    private var videoShowing = false

    override fun onCreate(savedInstanceState: Bundle?) {
        installSplashScreen()
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        model.player.connect()
        setContent {
            val settings by model.settings.collectAsStateWithLifecycle()
            YtLiteTheme(dynamicColor = settings.dynamicColor) {
                App(model)
            }
        }
        // Closing the Picture in Picture window stops the video.
        addOnPictureInPictureModeChangedListener { info ->
            if (!info.isInPictureInPictureMode && lifecycle.currentState == Lifecycle.State.CREATED) {
                model.player.stop()
            }
        }
    }

    override fun onStart() {
        super.onStart()
        model.player.onAppVisible()
        lifecycleScope.launch { model.checkSignIn() }
    }

    override fun onStop() {
        super.onStop()
        if (!isInPictureInPictureMode) {
            model.player.onAppHidden(keepPlaying = model.settings.value.backgroundPlayback)
        }
    }

    fun setVideoShowing(showing: Boolean) {
        videoShowing = showing
        setPictureInPictureParams(pipParams())
    }

    /** Android 12+ enters Picture in Picture by itself (auto-enter). */
    override fun onUserLeaveHint() {
        super.onUserLeaveHint()
        if (videoShowing && Build.VERSION.SDK_INT < 31) enterPictureInPictureMode(pipParams())
    }

    private fun pipParams(): PictureInPictureParams =
        PictureInPictureParams.Builder()
            .setAspectRatio(Rational(16, 9))
            .apply {
                if (Build.VERSION.SDK_INT >= 31) {
                    setAutoEnterEnabled(videoShowing)
                    setSeamlessResizeEnabled(true)
                }
            }
            .build()
}
