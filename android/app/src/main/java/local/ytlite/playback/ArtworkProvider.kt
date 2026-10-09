package local.ytlite.playback

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import java.io.File
import java.io.FileNotFoundException
import java.net.URL
import local.ytlite.BuildConfig

/**
 * Video thumbnails as content:// URIs. Android Auto (and the system's media
 * controls) can't load web URLs for artwork, so they're downloaded here and
 * served from the cache. Read-only, and only public YouTube thumbnails.
 */
class ArtworkProvider : ContentProvider() {
    override fun onCreate() = true

    override fun getType(uri: Uri) = "image/jpeg"

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        val id = uri.lastPathSegment?.takeIf { VIDEO_ID.matches(it) }
            ?: throw FileNotFoundException(uri.toString())
        val dir = File(context!!.cacheDir, "artwork").apply { mkdirs() }
        val file = File(dir, "$id.jpg")
        if (!file.exists()) {
            try {
                val partial = File.createTempFile(id, ".part", dir)
                URL("https://i.ytimg.com/vi/$id/mqdefault.jpg").openStream().use { input ->
                    partial.outputStream().use { input.copyTo(it) }
                }
                partial.renameTo(file)
            } catch (e: Exception) {
                throw FileNotFoundException("thumbnail for $id: $e")
            }
        }
        return ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
    }

    override fun query(
        uri: Uri, projection: Array<out String>?, selection: String?,
        selectionArgs: Array<out String>?, sortOrder: String?,
    ): Cursor? = null

    override fun insert(uri: Uri, values: ContentValues?): Uri? = null

    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?) = 0

    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?) = 0

    companion object {
        private val VIDEO_ID = Regex("[A-Za-z0-9_-]{11}")

        fun uri(videoId: String): Uri =
            Uri.parse("content://${BuildConfig.APPLICATION_ID}.artwork/$videoId")
    }
}
