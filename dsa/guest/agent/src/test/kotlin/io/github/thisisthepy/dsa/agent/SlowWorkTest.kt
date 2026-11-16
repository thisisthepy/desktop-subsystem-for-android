package io.github.thisisthepy.dsa.agent

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class SlowWorkTest {
    private val log = RecordingLog()
    private val slow = SlowWork("dsa-agent", log)
    private val work = Runnable {}

    @Test
    fun fr4_work_that_holds_the_agents_thread_is_named_and_quick_work_is_not() {
        slow.ran(work, 0, SlowWork.SLOW_MILLIS - 1)
        assertTrue(log.lines.isEmpty())

        slow.ran(work, 1_000, 2_400)
        val line = log.lines.single()
        assertTrue(line, line.startsWith("warn: the dsa-agent thread spent 1400 ms on one piece of work ("))
        assertTrue(line, line.contains(work.javaClass.name))
    }

    @Test
    fun fr4_slow_work_is_said_at_most_once_in_a_while_with_the_count_left_out() {
        slow.ran(work, 0, 500)
        slow.ran(work, 600, 1_100)
        slow.ran(work, 1_200, 1_700)
        assertEquals(1, log.lines.size)

        slow.ran(work, 20_000, 20_500)
        assertEquals(2, log.lines.size)
        assertTrue(log.lines[1], log.lines[1].endsWith("; 2 more since the last line"))
    }

    @Test
    fun fr4_work_that_has_not_ended_is_named_with_where_the_thread_is_and_only_once() {
        val stack = listOf("android.os.BinderProxy.transact", "a.b.Agent.refresh")
        // Not long enough yet.
        slow.stuck(work, 1_000, 1_000 + SlowWork.STUCK_MILLIS - 1) { stack }
        assertTrue(log.lines.isEmpty())

        slow.stuck(work, 1_000, 1_000 + SlowWork.STUCK_MILLIS) { stack }
        val line = log.lines.single()
        assertTrue(line, line.startsWith("warn: the dsa-agent thread has been in one piece of work for ${SlowWork.STUCK_MILLIS} ms and is still in it ("))
        assertTrue(line, line.contains("android.os.BinderProxy.transact"))
        assertTrue(line, line.contains("a.b.Agent.refresh"))

        // The same piece of work is not said again while it goes on; the next one is.
        slow.stuck(work, 1_000, 9_000) { stack }
        assertEquals(1, log.lines.size)
        slow.stuck(work, 12_000, 12_000 + SlowWork.STUCK_MILLIS) { stack }
        assertEquals(2, log.lines.size)
    }
}
