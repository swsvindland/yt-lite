package local.ytlite.ui

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

/** The brand red of the logo and app icons. */
val BrandRed = Color(0xFFFF0033)

private val LightColors = lightColorScheme(
    primary = Color(0xFFC00027),
    onPrimary = Color.White,
    primaryContainer = Color(0xFFFFDAD9),
    onPrimaryContainer = Color(0xFF410007),
    secondaryContainer = Color(0xFFFFDAD9),
    onSecondaryContainer = Color(0xFF410007),
)

private val DarkColors = darkColorScheme(
    primary = Color(0xFFFF5370),
    onPrimary = Color(0xFF5F0014),
    primaryContainer = Color(0xFF8C0021),
    onPrimaryContainer = Color(0xFFFFDAD9),
    secondaryContainer = Color(0xFF5C1A22),
    onSecondaryContainer = Color(0xFFFFDAD9),
    background = Color(0xFF0F0F0F),
    surface = Color(0xFF0F0F0F),
)

/** yt-lite red, light or dark with the system; Material You colors if `dynamicColor`. */
@Composable
fun YtLiteTheme(dynamicColor: Boolean, content: @Composable () -> Unit) {
    val dark = isSystemInDarkTheme()
    val context = LocalContext.current
    val colors = when {
        dynamicColor && Build.VERSION.SDK_INT >= 31 ->
            if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        dark -> DarkColors
        else -> LightColors
    }
    MaterialTheme(colorScheme = colors, content = content)
}
