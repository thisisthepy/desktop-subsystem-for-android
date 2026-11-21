package io.github.thisisthepy.dsa.agent.platform

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.database.MatrixCursor
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.clipboard.ClipboardSync
import io.github.thisisthepy.dsa.agent.clipboard.GuestClipboard
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileNotFoundException

/**
 * The guest's clipboard through the framework's ClipboardManager.
 *
 * Since Android 10 only the default input method and the app with the input focus may read
 * the clipboard or hear that it changed. The clipboard service decides by the calling
 * package, and the agent's package is the guest's default input method ([HostInputMethod],
 * made so at boot by [AndroidInputMethodSetup]), so reads from the agent's process are
 * allowed whichever app has the focus. Should that ever not hold, the clipboard says it has a
 * clip but gives none, and that is logged once. Setting the clip needs no permission.
 *
 * An image from the host is kept as a file of the agent's own and put on the clipboard as a
 * `content://` URI of [HostClipboardProvider], MIME `image/png`; the clipboard service lets
 * the app that pastes it read it. An image an app copied is read by opening its URI, and made
 * PNG when it is another type.
 */
class AndroidClipboard(private val context: Context, private val log: Log) : GuestClipboard {
    private val manager = context.getSystemService(ClipboardManager::class.java)
    private var refusedLogged = false

    private fun primaryClip(): ClipData? {
        val clip = manager.primaryClip
        if (clip == null) {
            if (manager.hasPrimaryClip() && !refusedLogged) {
                refusedLogged = true
                log.warn("Android refused the agent its clipboard; is the agent still the default input method?")
            }
            return null
        }
        return clip.takeIf { it.itemCount > 0 }
    }

    override fun text(): String? {
        // Only text here: a clip of a URI is an image or stays in Android, and an intent stays.
        return primaryClip()?.getItemAt(0)?.text?.toString()
    }

    override fun image(): ByteArray? {
        val clip = primaryClip() ?: return null
        val uri = clip.getItemAt(0).uri ?: return null
        val type = context.contentResolver.getType(uri)
            ?: (0 until clip.description.mimeTypeCount).map { clip.description.getMimeType(it) }
                .firstOrNull { it.startsWith("image/") }
            ?: return null
        if (!ClipDescription.compareMimeTypes(type, "image/*")) return null
        val bytes = context.contentResolver.openInputStream(uri)?.use { input ->
            val out = ByteArrayOutputStream()
            val buffer = ByteArray(64 * 1024)
            var total = 0
            while (total <= ClipboardSync.MAX_IMAGE_BYTES) {
                val read = input.read(buffer)
                if (read < 0) break
                out.write(buffer, 0, read)
                total += read
            }
            out.toByteArray()
        } ?: return null
        if (ClipboardSync.isPng(bytes) || bytes.size > ClipboardSync.MAX_IMAGE_BYTES) return bytes
        // Another type (JPEG, WebP, GIF's first frame): decoded and written again as PNG.
        val bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size) ?: return null
        return try {
            ByteArrayOutputStream().also { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }.toByteArray()
        } finally {
            bitmap.recycle()
        }
    }

    override fun setText(text: String) {
        manager.setPrimaryClip(ClipData.newPlainText(LABEL, text))
    }

    override fun setImage(png: ByteArray) {
        val uri = HostClipboardProvider.store(context, png)
        val clip = ClipData(ClipDescription(LABEL, arrayOf("image/png")), ClipData.Item(uri))
        manager.setPrimaryClip(clip)
    }

    override fun listen(onChange: () -> Unit) {
        manager.addPrimaryClipChangedListener { onChange() }
    }

    private companion object {
        /** What a clip from the host is called, where Android shows a clip's label. */
        const val LABEL = "DSA"
    }
}

/**
 * Serves the image the host copied, read only, as `content://<package>.clipboard/<name>.png`,
 * to the app that pastes it. Only the latest image is kept: putting a new one removes the
 * others, so the images the host copies never pile up. It is not exported: an app reads the
 * image only through the permission the clipboard service grants when it pastes.
 */
class HostClipboardProvider : ContentProvider() {
    override fun onCreate() = true

    private fun fileOf(uri: Uri): File {
        val context = context ?: throw FileNotFoundException("no context")
        val segments = uri.pathSegments
        if (segments.size != 1) throw FileNotFoundException("no such image: $uri")
        val root = folder(context).canonicalFile
        val file = File(root, segments[0]).canonicalFile
        // A path that leaves the folder of the host's images is never served.
        if (file.parentFile != root || !file.isFile) throw FileNotFoundException("no such image: $uri")
        return file
    }

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        if (mode != "r") throw SecurityException("images from the host are read only")
        return ParcelFileDescriptor.open(fileOf(uri), ParcelFileDescriptor.MODE_READ_ONLY)
    }

    override fun getType(uri: Uri): String = "image/png"

    override fun query(
        uri: Uri,
        projection: Array<out String>?,
        selection: String?,
        selectionArgs: Array<out String>?,
        sortOrder: String?,
    ): Cursor {
        val file = fileOf(uri)
        val columns = projection ?: arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)
        val cursor = MatrixCursor(columns)
        cursor.addRow(
            columns.map { column ->
                when (column) {
                    OpenableColumns.DISPLAY_NAME -> file.name
                    OpenableColumns.SIZE -> file.length()
                    else -> null
                }
            },
        )
        return cursor
    }

    override fun insert(uri: Uri, values: ContentValues?): Uri? = null

    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?) = 0

    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?) = 0

    companion object {
        fun authority(context: Context) = "${context.packageName}.clipboard"

        private fun folder(context: Context) = File(context.filesDir, "clipboard")

        /**
         * Keeps [png] as the host's image and returns its URI. Each image gets a new name, so an
         * app that read the last one never sees it change under it.
         */
        fun store(context: Context, png: ByteArray): Uri {
            val folder = folder(context)
            folder.mkdirs()
            folder.listFiles()?.forEach { it.delete() }
            val name = "image-${System.currentTimeMillis()}.png"
            File(folder, name).writeBytes(png)
            return Uri.Builder().scheme("content").authority(authority(context)).appendPath(name).build()
        }
    }
}
