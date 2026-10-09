package local.ytlite.ui

import android.os.Build
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material.icons.outlined.DirectionsCar
import androidx.compose.material.icons.outlined.Warning
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import local.ytlite.core.AppModel
import local.ytlite.core.Settings

private val qualities = listOf(720 to "720p", 1080 to "1080p", 1440 to "1440p", 2160 to "4K")

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(model: AppModel) {
    val settings by model.settings.collectAsStateWithLifecycle()
    val signedIn by model.signedIn.collectAsStateWithLifecycle()
    val signingIn by model.signingIn.collectAsStateWithLifecycle()
    val credentialProblem by model.credentialProblem.collectAsStateWithLifecycle()
    val context = LocalContext.current
    fun update(change: (Settings) -> Settings) = model.updateSettings(change)

    ScreenScaffold("Settings") { modifier ->
        LazyColumn(modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = 24.dp)) {
            item { Section("YouTube account") }
            item {
                when {
                    signedIn -> ListItem(
                        headlineContent = { Text("Signed in") },
                        leadingContent = { Icon(Icons.Outlined.CheckCircle, null, tint = Color(0xFF22C55E)) },
                        trailingContent = { TextButton(onClick = model::signOut) { Text("Sign out") } },
                    )
                    signingIn -> ListItem(
                        headlineContent = { Text("Waiting for Google…") },
                        supportingContent = { Text("Finish signing in in the browser tab.") },
                        leadingContent = { CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 3.dp) },
                        trailingContent = { TextButton(onClick = model::cancelSignIn) { Text("Cancel") } },
                    )
                    model.hasGoogleClient -> ListItem(
                        headlineContent = { Text("Sign in with Google") },
                        supportingContent = { Text("Loads the channels you subscribe to.") },
                        leadingContent = { Icon(Icons.Outlined.AccountCircle, null) },
                        modifier = Modifier.clickable { model.signIn(context) },
                    )
                    else -> ListItem(
                        headlineContent = { Text("No Google OAuth client") },
                        supportingContent = {
                            Text("Run scripts/build-android.sh on a Mac whose desktop config has one.")
                        },
                    )
                }
            }
            credentialProblem?.let { problem ->
                item {
                    ListItem(
                        headlineContent = { Text("Secure storage unavailable") },
                        supportingContent = { Text(problem) },
                        leadingContent = { Icon(Icons.Outlined.Warning, null, tint = Color(0xFFF59E0B)) },
                    )
                }
            }

            item { Section("Playback") }
            item {
                ListItem(
                    headlineContent = { Text("Maximum quality") },
                    supportingContent = {
                        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(top = 8.dp)) {
                            qualities.forEachIndexed { index, (height, label) ->
                                SegmentedButton(
                                    selected = settings.maxHeight == height,
                                    onClick = { update { it.copy(maxHeight = height) } },
                                    shape = SegmentedButtonDefaults.itemShape(index, qualities.size),
                                ) { Text(label) }
                            }
                        }
                    },
                )
            }
            item {
                SwitchItem(
                    "Keep playing in background",
                    "Videos keep playing as audio when you leave the app, with controls in the notification.",
                    settings.backgroundPlayback,
                ) { on -> update { it.copy(backgroundPlayback = on) } }
            }
            item {
                SwitchItem(
                    "Audio only",
                    "Plays just the sound (great for podcasts), using far less data and battery. " +
                        "Long-press any video to choose per video.",
                    settings.audioOnly,
                ) { on -> update { it.copy(audioOnly = on) } }
            }

            item { Section("Feeds") }
            item {
                SwitchItem("Hide watched videos", null, settings.hideWatched) { on ->
                    update { it.copy(hideWatched = on) }
                }
            }

            if (Build.VERSION.SDK_INT >= 31) {
                item { Section("Appearance") }
                item {
                    SwitchItem(
                        "Dynamic color",
                        "Use colors from your wallpaper instead of yt-lite red.",
                        settings.dynamicColor,
                    ) { on -> update { it.copy(dynamicColor = on) } }
                }
            }

            item { Section("Android Auto") }
            item {
                ListItem(
                    headlineContent = { Text("Subscriptions, For you and Explore in the car") },
                    supportingContent = {
                        Text(
                            "Videos play as audio. Apps installed outside the Play Store show up " +
                                "after turning on Unknown sources in Android Auto's developer settings."
                        )
                    },
                    leadingContent = { Icon(Icons.Outlined.DirectionsCar, null) },
                )
            }

            item { Section("About") }
            item { ListItem(headlineContent = { Text("Shorts") }, trailingContent = { Text("Never shown") }) }
            item { ListItem(headlineContent = { Text("Ads") }, trailingContent = { Text("None") }) }
            item {
                Text(
                    "yt-lite resolves streams natively and plays them with Media3.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                )
            }
        }
    }
}

@Composable
private fun Section(title: String) {
    Text(
        title,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 24.dp, bottom = 4.dp),
    )
}

@Composable
private fun SwitchItem(title: String, description: String?, checked: Boolean, onChange: (Boolean) -> Unit) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = description?.let { { Text(it) } },
        trailingContent = { Switch(checked = checked, onCheckedChange = onChange) },
        modifier = Modifier.clickable { onChange(!checked) },
    )
}
