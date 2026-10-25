package io.github.thisisthepy.dsa.agent

/** Runs the agent's work on its one thread, in order. */
interface Scheduler {
    fun post(work: () -> Unit)

    fun postDelayed(delayMillis: Long, work: () -> Unit)
}

/** Where the agent's messages go: the guest's log on the guest, a list in tests. */
interface Log {
    fun info(message: String)

    fun warn(message: String, error: Throwable? = null)
}
