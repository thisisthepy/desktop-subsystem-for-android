package io.github.thisisthepy.dsa.agent.platform

import android.content.ClipData
import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import io.github.thisisthepy.dsa.agent.files.FileOpener
import io.github.thisisthepy.dsa.agent.files.StoredFile
import io.github.thisisthepy.dsa.control.v1.FileAction
import java.io.File
import java.io.FileNotFoundException

/** The folder the files from the host are kept in, under the agent's own files. */
fun hostFilesDir(context: Context): File = File(context.filesDir, "host-files")

/** The MIME type of a file named [name], from its extension; a generic type when unknown. */
fun mimeTypeOf(name: String): String {
    val extension = name.substringAfterLast('.', "").lowercase()
    return MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension) ?: "application/octet-stream"
}

/**
 * Serves the files from the host to the app the user picks, read only, as
 * `content://<package>.hostfiles/<transfer folder>/<name>`. It is not exported: an app reads a
 * file only through the permission the agent grants with the intent that offers it.
 */
class HostFileProvider : ContentProvider() {
    override fun onCreate() = true

    private fun fileOf(uri: Uri): File {
        val context = context ?: throw FileNotFoundException("no context")
        val segments = uri.pathSegments
        if (segments.size != 2) throw FileNotFoundException("no such file: $uri")
        val root = hostFilesDir(context).canonicalFile
        val file = File(File(root, segments[0]), segments[1]).canonicalFile
        // A path that leaves the folder of the host's files is never served.
        if (file.parentFile?.parentFile != root || !file.isFile) throw FileNotFoundException("no such file: $uri")
        return file
    }

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        if (mode != "r") throw SecurityException("files from the host are read only")
        return ParcelFileDescriptor.open(fileOf(uri), ParcelFileDescriptor.MODE_READ_ONLY)
    }

    override fun getType(uri: Uri): String = mimeTypeOf(uri.lastPathSegment ?: "")

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
        fun authority(context: Context) = "${context.packageName}.hostfiles"

        fun uriFor(context: Context, file: StoredFile): Uri = Uri.Builder()
            .scheme("content")
            .authority(authority(context))
            .appendPath(file.folder)
            .appendPath(file.name)
            .build()
    }
}

/**
 * Asks the user which app opens a file from the host (ACTION_VIEW) or takes it to share
 * (ACTION_SEND), with the system's chooser, which the host shows as a window. The app picked
 * may read the file once, through [HostFileProvider].
 */
class AndroidFileOpener(private val context: Context) : FileOpener {
    override fun open(file: StoredFile, action: FileAction) {
        val uri = HostFileProvider.uriFor(context, file)
        val type = mimeTypeOf(file.name)
        val intent = if (action == FileAction.FILE_ACTION_SEND) {
            Intent(Intent.ACTION_SEND).setType(type).putExtra(Intent.EXTRA_STREAM, uri)
        } else {
            Intent(Intent.ACTION_VIEW).setDataAndType(uri, type)
        }
        intent.clipData = ClipData.newRawUri(file.name, uri)
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        val chooser = Intent.createChooser(intent, file.name)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_GRANT_READ_URI_PERMISSION)
        context.startActivity(chooser)
    }
}
