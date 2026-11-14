package io.github.thisisthepy.dsa.agent.channel

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.Envelope
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong

/**
 * Requests to the host that a framework service makes on its own thread and waits for: the
 * backup transport's uploads and reads, and the credential provider's passkey requests. The
 * backup manager and the credential manager call the agent's services on their binder threads
 * and expect an answer from the call, so each request blocks its caller until the host's
 * answer (matched by its request number) comes, or the wait runs out.
 *
 * The answers come on the agent's thread, through [handle]. A new connection ends every wait:
 * the host that was to answer is gone. [send] may be called on any thread.
 */
class HostCalls(
    private val send: (ByteArray) -> Boolean,
    private val log: Log,
) {
    private val next = AtomicLong(1)
    private val waiting = ConcurrentHashMap<Long, LinkedBlockingQueue<Any>>()

    /** A request number of its own, unique while the agent runs. */
    fun nextId(): Long = next.getAndIncrement()

    /**
     * Sends [request] and waits at most [timeoutMillis] for the host's answer to request [id].
     * Null when it could not be sent, no answer came in time, or the host went away.
     */
    fun call(id: Long, request: Envelope, timeoutMillis: Long): Envelope? {
        val queue = LinkedBlockingQueue<Any>()
        waiting[id] = queue
        try {
            if (!send(request.toByteArray())) return null
            val answer = try {
                queue.poll(timeoutMillis, TimeUnit.MILLISECONDS)
            } catch (e: InterruptedException) {
                Thread.currentThread().interrupt()
                null
            }
            return answer as? Envelope
        } finally {
            waiting.remove(id)
        }
    }

    /** Sends a message the host does not answer. */
    fun tell(message: Envelope): Boolean = send(message.toByteArray())

    /** Hands an answer to the request that waits for it. False for any other frame. */
    fun handle(envelope: Envelope): Boolean {
        val id = when (envelope.bodyCase) {
            Envelope.BodyCase.BACKUP_ANSWER -> envelope.backupAnswer.requestId
            Envelope.BodyCase.BACKUP_CONTENTS -> envelope.backupContents.requestId
            Envelope.BodyCase.BACKUP_FETCH_DATA -> envelope.backupFetchData.requestId
            Envelope.BodyCase.PASSKEY_RESPONSE -> envelope.passkeyResponse.requestId
            else -> return false
        }
        val queue = waiting[id]
        if (queue == null) {
            log.info("the host answered request $id, which nothing waits for any more")
        } else {
            queue.offer(envelope)
        }
        return true
    }

    /** A new connection: the host that had the requests under way is gone. */
    fun connected() {
        for (queue in waiting.values) queue.offer(GONE)
    }

    /** How many requests wait for an answer; for tests. */
    val pending: Int get() = waiting.size

    private companion object {
        /** Stands for "the host went away" in a request's queue. */
        val GONE = Any()
    }
}
