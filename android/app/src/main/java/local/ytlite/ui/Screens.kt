package local.ytlite.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.Search
import androidx.compose.material.icons.outlined.StarOutline
import androidx.compose.material.icons.outlined.Warning
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LargeTopAppBar
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.adaptive.currentWindowAdaptiveInfo
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.window.core.layout.WindowSizeClass
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import local.ytlite.core.AppModel
import local.ytlite.core.describe
import uniffi.yt_lite_ffi.Feed
import uniffi.yt_lite_ffi.FfiException
import uniffi.yt_lite_ffi.Video

/** A screen with a top app bar: a large, collapsing title unless `large` is
 *  false or the window is short (landscape phones). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ScreenScaffold(
    title: String,
    large: Boolean = true,
    actions: @Composable RowScope.() -> Unit = {},
    content: @Composable (Modifier) -> Unit,
) {
    val tall = currentWindowAdaptiveInfo().windowSizeClass
        .isHeightAtLeastBreakpoint(WindowSizeClass.HEIGHT_DP_MEDIUM_LOWER_BOUND)
    @Suppress("NAME_SHADOWING") val large = large && tall
    val scroll = if (large) {
        TopAppBarDefaults.exitUntilCollapsedScrollBehavior()
    } else {
        TopAppBarDefaults.pinnedScrollBehavior()
    }
    Scaffold(
        modifier = Modifier.nestedScroll(scroll.nestedScrollConnection),
        topBar = {
            if (large) {
                LargeTopAppBar(title = { Text(title) }, actions = actions, scrollBehavior = scroll)
            } else {
                TopAppBar(title = { Text(title) }, actions = actions, scrollBehavior = scroll)
            }
        },
        // The navigation bar or rail handles the system insets below and beside.
        contentWindowInsets = WindowInsets(0),
    ) { padding -> content(Modifier.padding(padding)) }
}

// Subscriptions and For you

/** A cached feed, refreshed from YouTube when it's over 15 minutes old. */
class FeedViewModel(private val model: AppModel, private val feed: Feed) : ViewModel() {
    var videos by mutableStateOf(emptyList<Video>())
        private set
    var loaded by mutableStateOf(false)
        private set
    var refreshing by mutableStateOf(false)
        private set
    var failure by mutableStateOf<String?>(null)
        private set

    init {
        // The cache again whenever watched state, progress or "hide watched" change.
        viewModelScope.launch {
            combine(
                model.libraryVersion,
                model.settings.map { it.hideWatched }.distinctUntilChanged(),
            ) { _, _ -> }.collect { reload() }
        }
        if (feed == Feed.SUBSCRIPTIONS) {
            viewModelScope.launch { model.signedIn.drop(1).filter { it }.collect { refresh() } }
        }
    }

    fun onShown() {
        if (model.isStale(feed)) refresh()
    }

    fun refresh() {
        if (refreshing) return
        refreshing = true
        viewModelScope.launch {
            failure = try {
                model.refresh(feed)
                null
            } catch (e: FfiException.NotSignedIn) {
                null
            } catch (e: Exception) {
                describe(e)
            }
            reload()
            refreshing = false
        }
    }

    private suspend fun reload() {
        runCatching { model.videos(feed) }.onSuccess { videos = it }
        loaded = true
    }
}

@Composable
fun FeedScreen(model: AppModel, feed: Feed, title: String, onOpenSettings: () -> Unit) {
    val vm = viewModel(key = feed.name) { FeedViewModel(model, feed) }
    val signedIn by model.signedIn.collectAsStateWithLifecycle()
    val signingIn by model.signingIn.collectAsStateWithLifecycle()
    val context = LocalContext.current
    LaunchedEffect(vm) { vm.onShown() }

    ScreenScaffold(title) { modifier ->
        PullToRefreshBox(isRefreshing = vm.refreshing, onRefresh = vm::refresh, modifier = modifier) {
            VideoGrid(model, vm.videos, empty = {
                val failure = vm.failure
                when {
                    failure != null -> EmptyState(Icons.Outlined.Warning, "Couldn't load", failure)
                    vm.refreshing || !vm.loaded -> {}
                    feed == Feed.SUBSCRIPTIONS && !signedIn -> EmptyState(
                        Icons.Outlined.AccountCircle,
                        "Connect your YouTube account",
                        "Sign in to load the channels you subscribe to.",
                    ) {
                        if (model.hasGoogleClient) {
                            Button(onClick = { model.signIn(context) }, enabled = !signingIn) {
                                Text("Sign in with Google")
                            }
                        } else {
                            OutlinedButton(onClick = onOpenSettings) { Text("Settings") }
                        }
                    }
                    feed == Feed.FOR_YOU -> EmptyState(
                        Icons.Outlined.StarOutline,
                        "Nothing here yet",
                        "Watch a few videos and yt-lite will recommend more like them.",
                    )
                    else -> EmptyState(Icons.Outlined.Inbox, "No videos yet", "Pull down to refresh.")
                }
            })
        }
    }
}

// Explore

/** Popular this week, by topic. */
class ExploreViewModel(private val model: AppModel) : ViewModel() {
    var topic by mutableIntStateOf(0)
        private set
    var videos by mutableStateOf(emptyList<Video>())
        private set
    var loading by mutableStateOf(false)
        private set
    var failure by mutableStateOf<String?>(null)
        private set
    private var job: Job? = null

    init {
        load()
        viewModelScope.launch {
            model.libraryVersion.drop(1).collect { videos = model.refreshed(videos, Feed.EXPLORE) }
        }
    }

    fun select(index: Int) {
        topic = index
        videos = emptyList()
        load()
    }

    fun load() {
        job?.cancel()
        loading = true
        job = viewModelScope.launch {
            try {
                videos = model.explore(topic)
                failure = null
            } catch (e: Exception) {
                failure = describe(e)
            } finally {
                loading = false
            }
        }
    }
}

@Composable
fun ExploreScreen(model: AppModel) {
    val vm = viewModel { ExploreViewModel(model) }
    ScreenScaffold("Explore") { modifier ->
        PullToRefreshBox(isRefreshing = vm.loading, onRefresh = vm::load, modifier = modifier) {
            VideoGrid(
                model,
                vm.videos,
                header = {
                    item(span = { GridItemSpan(maxLineSpan) }) {
                        LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            itemsIndexed(model.exploreTopics) { index, name ->
                                FilterChip(
                                    selected = index == vm.topic,
                                    onClick = { vm.select(index) },
                                    label = { Text(name) },
                                )
                            }
                        }
                    }
                },
                empty = {
                    vm.failure?.takeIf { !vm.loading }?.let {
                        EmptyState(Icons.Outlined.Warning, "Couldn't load", it)
                    }
                },
            )
        }
    }
}

// Search

class SearchViewModel(private val model: AppModel) : ViewModel() {
    var query by mutableStateOf("")
    var videos by mutableStateOf(emptyList<Video>())
        private set
    var loading by mutableStateOf(false)
        private set
    var failure by mutableStateOf<String?>(null)
        private set
    var searched by mutableStateOf(false)
        private set
    private var job: Job? = null

    init {
        viewModelScope.launch {
            model.libraryVersion.drop(1).collect { videos = model.refreshed(videos, Feed.SEARCH) }
        }
    }

    fun run() {
        val q = query.trim()
        if (q.isEmpty()) return
        job?.cancel()
        loading = true
        job = viewModelScope.launch {
            try {
                videos = model.search(q)
                failure = null
            } catch (e: Exception) {
                failure = describe(e)
            } finally {
                searched = true
                loading = false
            }
        }
    }
}

@Composable
fun SearchScreen(model: AppModel) {
    val vm = viewModel { SearchViewModel(model) }
    val focus = LocalFocusManager.current
    ScreenScaffold("Search", large = false) { modifier ->
        VideoGrid(
            model,
            vm.videos,
            modifier = modifier,
            header = {
                item(span = { GridItemSpan(maxLineSpan) }) {
                    OutlinedTextField(
                        value = vm.query,
                        onValueChange = { vm.query = it },
                        modifier = Modifier.fillMaxWidth(),
                        placeholder = { Text("Search YouTube") },
                        leadingIcon = { Icon(Icons.Outlined.Search, null) },
                        trailingIcon = {
                            if (vm.query.isNotEmpty()) {
                                IconButton(onClick = { vm.query = "" }) { Icon(Icons.Outlined.Close, "Clear") }
                            }
                        },
                        singleLine = true,
                        shape = RoundedCornerShape(28.dp),
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                        keyboardActions = KeyboardActions(onSearch = {
                            focus.clearFocus()
                            vm.run()
                        }),
                    )
                }
            },
            empty = {
                val failure = vm.failure
                when {
                    vm.loading -> Box(Modifier.fillMaxWidth().padding(top = 96.dp), Alignment.Center) {
                        CircularProgressIndicator()
                    }
                    failure != null -> EmptyState(Icons.Outlined.Warning, "Search failed", failure)
                    else -> EmptyState(
                        Icons.Outlined.Search,
                        if (vm.searched) "No results" else "Search YouTube",
                        "Results never include Shorts.",
                    )
                }
            },
        )
    }
}
