package io.github.thisisthepy.dsa.agent.trace

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.PointerAction
import io.github.thisisthepy.dsa.control.v1.PointerEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class InputTraceTest {
    private val lines = mutableListOf<String>()
    private val log = object : Log {
        override fun info(message: String) {
            lines += message
        }

        override fun warn(message: String, error: Throwable?) = Unit
    }

    private fun event(action: PointerAction) = PointerEvent.newBuilder().setTaskId(7).setAction(action).build()

    private fun trace(enabled: Boolean) = InputTrace(log, enabled, { 1234.0 }, { 0L })

    @Test
    fun fr5_a_press_is_traced_with_where_its_time_went() {
        trace(true).injected(event(PointerAction.POINTER_ACTION_DOWN), 2_000_000, 500_000)

        assertEquals(
            listOf(
                "input-trace stage=agent.inject task=7 action=POINTER_ACTION_DOWN " +
                    "utc=1970-01-01T00:00:00.000Z mono=1234.000 queued=2.0 inject=0.5",
            ),
            lines,
        )
    }

    @Test
    fun fr5_moves_are_not_traced() {
        trace(true).injected(event(PointerAction.POINTER_ACTION_MOVE), 0, 0)

        assertTrue(lines.isEmpty())
    }

    @Test
    fun fr5_the_trace_is_silent_unless_asked_for() {
        trace(false).injected(event(PointerAction.POINTER_ACTION_DOWN), 0, 0)

        assertTrue(lines.isEmpty())
    }
}
