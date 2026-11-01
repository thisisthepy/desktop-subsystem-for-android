package io.github.thisisthepy.dsa.agent.files

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.FileAction
import io.github.thisisthepy.dsa.control.v1.FileChunk
import io.github.thisisthepy.dsa.control.v1.FileProgress
import io.github.thisisthepy.dsa.control.v1.OpenFile
import java.io.File
import java.io.FileOutputStream

/** A file from the host, stored whole in the agent's own files. */
data class StoredFile(
    /** The transfer's own folder, under the store's root. */
    val folder: String,
    /** The file's name, as the host gave it. */
    val name: String,
    val file: File,
)

/** Asks the user which app opens (or takes, to share) a file from the host. */
fun interface FileOpener {
    fun open(file: StoredFile, action: FileAction)

    companion object {
        val NONE = FileOpener { _, _ -> throw IllegalStateException("this agent opens no files") }
    }
}

/**
 * Where the files from the host are kept: one folder per transfer under [root], named after the
 * transfer and when it began, so a file an app still shows is never overwritten by the next.
 * Folders older than [keepMillis] are removed whenever a transfer begins, so the files of past
 * transfers do not pile up in the guest's storage; an app gets the file at once, and an hour is
 * long enough for it to have read it.
 */
class TransferStore(
    val root: File,
    private val clock: () -> Long = System::currentTimeMillis,
    private val keepMillis: Long = KEEP_MILLIS,
) {
    /** Makes the folder of a new transfer and the empty file in it. */
    fun begin(transferId: Int, name: String): StoredFile {
        clean()
        val folder = "$transferId-${clock()}"
        val dir = File(root, folder)
        if (!dir.mkdirs() && !dir.isDirectory) throw IllegalStateException("cannot make $dir")
        val file = File(dir, name)
        file.writeBytes(ByteArray(0))
        return StoredFile(folder, name, file)
    }

    /** Adds [data] to the end of the file. */
    fun append(file: StoredFile, data: ByteArray) {
        FileOutputStream(file.file, true).use { it.write(data) }
    }

    /** Removes a transfer that will not be finished. */
    fun discard(file: StoredFile) {
        File(root, file.folder).deleteRecursively()
    }

    /** Removes the folders of transfers that began more than [keepMillis] ago. */
    fun clean() {
        val now = clock()
        root.listFiles()?.forEach { dir ->
            val began = dir.name.substringAfter('-', "").toLongOrNull()
            if (began == null || now - began > keepMillis) dir.deleteRecursively()
        }
    }

    companion object {
        const val KEEP_MILLIS = 60 * 60 * 1000L
    }
}

/**
 * Takes the files the host sends (OpenFile, then its FileChunk messages in order), stores each
 * in the [TransferStore], and once a file is whole asks the user which app opens it. After each
 * chunk it tells the host how far it has got (FileProgress), which is what lets the host send
 * the next ones; a transfer that goes wrong is ended with the reason, and its folder removed.
 *
 * Every method runs on the agent's scheduler thread.
 */
class FileReceiver(
    private val store: TransferStore?,
    private val opener: FileOpener,
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
) {
    private class Transfer(val file: StoredFile, val size: Long, val action: FileAction) {
        var received = 0L
    }

    private val transfers = mutableMapOf<Int, Transfer>()

    /** A new connection to the host: transfers of the last one will never be finished. */
    fun connected() {
        transfers.values.forEach { store?.discard(it.file) }
        transfers.clear()
    }

    /** Takes a frame of a transfer. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        when (envelope.bodyCase) {
            Envelope.BodyCase.OPEN_FILE -> begin(envelope.openFile)
            Envelope.BodyCase.FILE_CHUNK -> chunk(envelope.fileChunk)
            else -> return false
        }
        return true
    }

    private fun begin(open: OpenFile) {
        val id = open.transferId
        transfers.remove(id)?.let { store?.discard(it.file) }
        val refused = when {
            store == null -> "this agent keeps no files"
            !isUsableName(open.name) -> "\"${open.name}\" is not a file name Android can use"
            open.size < 0 || open.size > MAX_FILE_BYTES -> "the file is larger than the ${MAX_FILE_BYTES / (1024 * 1024)} MiB Android takes"
            open.action != FileAction.FILE_ACTION_VIEW && open.action != FileAction.FILE_ACTION_SEND -> "the host said neither to open nor to share the file"
            else -> null
        }
        if (refused != null) return fail(id, null, refused)
        val file = try {
            store!!.begin(id, open.name)
        } catch (e: Exception) {
            log.warn("could not make room for a file from the host", e)
            return fail(id, null, "Android could not store the file")
        }
        val transfer = Transfer(file, open.size, open.action)
        transfers[id] = transfer
        log.info("receiving ${open.name} (${open.size} bytes) from the host")
        if (open.size == 0L) finish(id, transfer)
    }

    private fun chunk(chunk: FileChunk) {
        val id = chunk.transferId
        val transfer = transfers[id]
        if (transfer == null) {
            log.warn("dropped a piece of transfer $id, which is not under way")
            return
        }
        if (chunk.offset != transfer.received) {
            return fail(id, transfer, "a piece of the file arrived out of order (at ${chunk.offset}, expected ${transfer.received})")
        }
        if (transfer.received + chunk.data.size() > transfer.size) {
            return fail(id, transfer, "more than the ${transfer.size} bytes announced arrived")
        }
        try {
            store!!.append(transfer.file, chunk.data.toByteArray())
        } catch (e: Exception) {
            log.warn("could not store a piece of a file from the host", e)
            return fail(id, transfer, "Android could not store the file; its storage may be full")
        }
        transfer.received += chunk.data.size()
        if (transfer.received == transfer.size) finish(id, transfer) else progress(id, transfer.received, "")
    }

    private fun finish(id: Int, transfer: Transfer) {
        transfers.remove(id)
        try {
            opener.open(transfer.file, transfer.action)
        } catch (e: Exception) {
            log.warn("could not ask which app opens ${transfer.file.name}", e)
            store?.discard(transfer.file)
            return progress(id, transfer.received, "no app in Android could be asked to open the file")
        }
        log.info("asked which app takes ${transfer.file.name}")
        progress(id, transfer.received, "")
    }

    private fun fail(id: Int, transfer: Transfer?, reason: String) {
        log.warn("refused a file from the host: $reason")
        transfers.remove(id)
        transfer?.let { store?.discard(it.file) }
        progress(id, transfer?.received ?: 0, reason)
    }

    private fun progress(id: Int, received: Long, error: String) {
        val frame = Envelope.newBuilder().setFileProgress(
            FileProgress.newBuilder().setTransferId(id).setReceived(received).setError(error),
        ).build()
        send(frame.toByteArray())
    }

    companion object {
        /** The largest file the host sends; it refuses larger ones itself. */
        const val MAX_FILE_BYTES = 64L * 1024 * 1024

        /** The longest name the guest's file system takes, in bytes. */
        private const val MAX_NAME_BYTES = 255

        /** Whether [name] can be a file's name in a transfer's folder, without leaving it. */
        fun isUsableName(name: String): Boolean =
            name.isNotEmpty() && name != "." && name != ".." &&
                name.toByteArray(Charsets.UTF_8).size <= MAX_NAME_BYTES &&
                !name.contains('/') && !name.contains('\\') && name.none { it.isISOControl() }
    }
}
