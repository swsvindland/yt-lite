package local.ytlite.playback

import android.content.ComponentName
import android.media.browse.MediaBrowser as PlatformBrowser
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import androidx.media3.common.Player
import androidx.media3.session.MediaBrowser
import androidx.media3.session.SessionToken
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.guava.await
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.delay
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Browses and plays like Android Auto does, against the real service and
 * YouTube (needs network): the tabs, an Explore topic, and a video played
 * audio only from a browse item.
 */
@RunWith(AndroidJUnit4::class)
class AndroidAutoTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private val service = ComponentName(context, PlaybackService::class.java)

    @Test
    fun legacyBrowserSeesTabsAndSearch() {
        // Android Auto connects through the platform MediaBrowser API.
        val connected = CountDownLatch(1)
        val children = CountDownLatch(1)
        var extras: Bundle? = null
        var tabs = emptyList<String>()
        lateinit var browser: PlatformBrowser
        Handler(Looper.getMainLooper()).post {
            browser = PlatformBrowser(context, service, object : PlatformBrowser.ConnectionCallback() {
                override fun onConnected() {
                    extras = browser.extras
                    browser.subscribe(browser.root, object : PlatformBrowser.SubscriptionCallback() {
                        override fun onChildrenLoaded(parentId: String, items: List<PlatformBrowser.MediaItem>) {
                            tabs = items.filter { it.isBrowsable }.map { it.description.title.toString() }
                            children.countDown()
                        }
                    })
                    connected.countDown()
                }
            }, null)
            browser.connect()
        }
        assertTrue("connected", connected.await(20, TimeUnit.SECONDS))
        assertTrue("root children", children.await(20, TimeUnit.SECONDS))
        assertEquals(listOf("Subscriptions", "For you", "Explore"), tabs)
        assertTrue(extras!!.getBoolean("android.media.browse.SEARCH_SUPPORTED"))
        Handler(Looper.getMainLooper()).post { browser.disconnect() }
    }

    @Test
    fun browsesExploreAndPlaysAudioOnly() = runBlocking(Dispatchers.Main) {
        // MediaBrowser lives on the main thread; awaiting suspends without blocking it.
        val browser = MediaBrowser.Builder(context, SessionToken(context, service)).buildAsync().await()
        try {
            val root = browser.getLibraryRoot(null).await().value!!
            assertEquals(LibraryTree.ROOT, root.mediaId)

            val topics = browser.getChildren(LibraryTree.EXPLORE, 0, 100, null).await().value!!
            assertTrue("topics", topics.isNotEmpty())
            val videos = browser.getChildren(topics.first().mediaId, 0, 100, null).await().value!!
            assertTrue("videos", videos.isNotEmpty())
            val video = videos.first()
            assertTrue(video.mediaMetadata.isPlayable == true)
            assertEquals("content", video.mediaMetadata.artworkUri?.scheme)

            // Like tapping it in the car: resolved by the service, audio only.
            browser.setMediaItem(video)
            browser.prepare()
            browser.play()
            withTimeout(30_000) {
                while (!browser.isPlaying) delay(200)
            }
            assertEquals(video.mediaId, browser.currentMediaItem?.mediaId)
            assertTrue(VideoItems.isAudioOnly(browser.currentMediaItem))
            assertEquals(Player.STATE_READY, browser.playbackState)
            browser.stop()
            browser.clearMediaItems()
        } finally {
            browser.release()
        }
    }
}
