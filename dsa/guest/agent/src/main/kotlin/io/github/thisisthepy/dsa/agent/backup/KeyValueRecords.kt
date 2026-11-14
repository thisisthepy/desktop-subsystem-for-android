package io.github.thisisthepy.dsa.agent.backup

import java.io.ByteArrayOutputStream
import java.io.DataOutputStream
import java.nio.ByteBuffer
import java.nio.BufferUnderflowException

/** One record of an app's key-value backup: a key and its value, or null for a deleted key. */
class KeyValueRecord(val key: String, val value: ByteArray?) {
    override fun equals(other: Any?): Boolean =
        other is KeyValueRecord && other.key == key &&
            (if (value == null) other.value == null else other.value != null && value.contentEquals(other.value))

    override fun hashCode(): Int = key.hashCode() * 31 + (value?.contentHashCode() ?: 0)

    override fun toString(): String = "KeyValueRecord($key, ${value?.size ?: "deleted"})"
}

/**
 * The form the host keeps an app's key-value records in: each record is a 32-bit big-endian
 * key length, the key in UTF-8, a 32-bit big-endian value length (-1 for a deleted key) and
 * the value. The host merges an incremental backup's records into what it holds.
 */
object KeyValueRecords {
    fun encode(records: List<KeyValueRecord>): ByteArray {
        val bytes = ByteArrayOutputStream()
        val out = DataOutputStream(bytes)
        for (record in records) {
            val key = record.key.toByteArray(Charsets.UTF_8)
            out.writeInt(key.size)
            out.write(key)
            val value = record.value
            if (value == null) {
                out.writeInt(-1)
            } else {
                out.writeInt(value.size)
                out.write(value)
            }
        }
        out.flush()
        return bytes.toByteArray()
    }

    /** The records in [data]; null when it is not in this form. */
    fun decode(data: ByteArray): List<KeyValueRecord>? {
        val buffer = ByteBuffer.wrap(data)
        val records = mutableListOf<KeyValueRecord>()
        return try {
            while (buffer.hasRemaining()) {
                val keyLength = buffer.int
                if (keyLength < 0 || keyLength > buffer.remaining()) return null
                val key = ByteArray(keyLength).also { buffer.get(it) }
                val valueLength = buffer.int
                val value = when {
                    valueLength == -1 -> null
                    valueLength < 0 || valueLength > buffer.remaining() -> return null
                    else -> ByteArray(valueLength).also { buffer.get(it) }
                }
                records += KeyValueRecord(String(key, Charsets.UTF_8), value)
            }
            records
        } catch (e: BufferUnderflowException) {
            null
        }
    }
}
