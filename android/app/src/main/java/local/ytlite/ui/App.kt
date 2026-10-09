package local.ytlite.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Explore
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.AccountCircle
import androidx.compose.material.icons.filled.Star
import androidx.compose.material.icons.filled.Subscriptions
import androidx.compose.material.icons.outlined.Explore
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.StarOutline
import androidx.compose.material.icons.outlined.Subscriptions
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.adaptive.currentWindowAdaptiveInfo
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteScaffold
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteType
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.window.core.layout.WindowSizeClass
import local.ytlite.core.AppModel
import uniffi.yt_lite_ffi.Feed

enum class Tab(val label: String, val icon: ImageVector, val selectedIcon: ImageVector) {
    Subscriptions("Subs", Icons.Outlined.Subscriptions, Icons.Filled.Subscriptions),
    ForYou("For you", Icons.Outlined.StarOutline, Icons.Filled.Star),
    Explore("Explore", Icons.Outlined.Explore, Icons.Filled.Explore),
    Search("Search", Icons.Outlined.Search, Icons.Filled.Search),
    You("You", Icons.Outlined.AccountCircle, Icons.Filled.AccountCircle),
}

/** Tabs (a bottom bar on phones, a rail on tablets and in landscape), the
 *  audio mini player, and the full-screen video player over everything. */
@Composable
fun App(model: AppModel) {
    var tab by rememberSaveable { mutableStateOf(Tab.Subscriptions) }
    /** Settings, opened from You. */
    var settingsOpen by rememberSaveable { mutableStateOf(false) }
    fun openSettings() {
        tab = Tab.You
        settingsOpen = true
    }
    val snackbar = remember { SnackbarHostState() }
    val nowPlaying by model.player.nowPlaying.collectAsStateWithLifecycle()
    val controller by model.player.controller.collectAsStateWithLifecycle()
    val resolving by model.resolving.collectAsStateWithLifecycle()

    LaunchedEffect(model) {
        model.startupError?.let { snackbar.showSnackbar(it) }
        model.messages.collect { snackbar.showSnackbar(it) }
    }

    // A rail when the window is wide, or too short to give up height to a bar.
    val sizeClass = currentWindowAdaptiveInfo().windowSizeClass
    val layout = if (
        sizeClass.isWidthAtLeastBreakpoint(WindowSizeClass.WIDTH_DP_MEDIUM_LOWER_BOUND) ||
        !sizeClass.isHeightAtLeastBreakpoint(WindowSizeClass.HEIGHT_DP_MEDIUM_LOWER_BOUND)
    ) {
        NavigationSuiteType.NavigationRail
    } else {
        NavigationSuiteType.NavigationBar
    }

    NavigationSuiteScaffold(
        layoutType = layout,
        navigationSuiteItems = {
            Tab.entries.forEach { t ->
                item(
                    selected = t == tab,
                    onClick = { tab = t },
                    icon = { Icon(if (t == tab) t.selectedIcon else t.icon, contentDescription = null) },
                    label = { Text(t.label) },
                )
            }
        },
    ) {
        Box(Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize()) {
                Box(Modifier.weight(1f)) {
                    when (tab) {
                        Tab.Subscriptions -> FeedScreen(model, Feed.SUBSCRIPTIONS, "Subscriptions", ::openSettings)
                        Tab.ForYou -> FeedScreen(model, Feed.FOR_YOU, "For you", ::openSettings)
                        Tab.Explore -> ExploreScreen(model)
                        Tab.Search -> SearchScreen(model)
                        Tab.You -> if (settingsOpen) {
                            SettingsScreen(model) { settingsOpen = false }
                        } else {
                            YouScreen(model, ::openSettings)
                        }
                    }
                }
                nowPlaying?.takeIf { it.audioOnly }?.let { MiniPlayer(model, it) }
            }
            SnackbarHost(snackbar, Modifier.align(Alignment.BottomCenter))
            if (resolving) {
                Surface(
                    Modifier.align(Alignment.Center),
                    shape = MaterialTheme.shapes.large,
                    tonalElevation = 6.dp,
                    shadowElevation = 6.dp,
                ) {
                    Row(Modifier.padding(20.dp), verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 3.dp)
                        Text("Opening…", Modifier.padding(start = 16.dp))
                    }
                }
            }
        }
    }

    val video = nowPlaying?.takeIf { !it.audioOnly }
    val c = controller
    if (video != null && c != null) {
        VideoPlayer(c, onClose = model.player::stop)
    }
}
