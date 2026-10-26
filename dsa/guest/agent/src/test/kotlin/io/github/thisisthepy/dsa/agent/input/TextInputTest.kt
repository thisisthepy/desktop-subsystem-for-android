package io.github.thisisthepy.dsa.agent.input

import io.github.thisisthepy.dsa.agent.FakeInjector
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.control.v1.ImeCommit
import io.github.thisisthepy.dsa.control.v1.ImeComposition
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** An editor that keeps its text as an input method would see it change. */
class FakeEditor : EditorConnection {
    var takesText = true
    var text = StringBuilder()
    var composing: IntRange? = null
    var cursor = 0
    val calls = mutableListOf<String>()

    override fun setComposing(text: String, cursor: Int): Boolean {
        calls += "compose $text $cursor"
        if (!takesText) return false
        val start = composing?.first ?: this.cursor
        composing?.let { this.text.delete(it.first, it.last + 1) }
        this.text.insert(start, text)
        composing = if (text.isEmpty()) null else start until start + text.length
        this.cursor = start + cursor
        return true
    }

    override fun commit(text: String): Boolean {
        calls += "commit $text"
        if (!takesText) return false
        val start = composing?.first ?: cursor
        composing?.let { this.text.delete(it.first, it.last + 1) }
        this.text.insert(start, text)
        composing = null
        cursor = start + text.length
        return true
    }
}

class TextInputTest {
    private val editor = FakeEditor()
    private val injector = FakeInjector()
    private val log = RecordingLog()
    private val reported = mutableSetOf(7, 8)
    private val focusedTasks = mutableSetOf(7)
    private val focusRequests = mutableListOf<Int>()
    private val sent = mutableListOf<ByteArray>()
    private var keysFor: (String) -> List<InjectedKey>? = { null }
    private val input = TextInput(
        editor = { editor },
        injector = injector,
        keys = TextKeys { keysFor(it) },
        isReported = { it in reported },
        isFocused = { it in focusedTasks },
        focus = { taskId ->
            focusRequests += taskId
            true
        },
        bounds = { taskId -> if (taskId in reported) TaskBounds(100, 50, 500, 650) else null },
        focusedTask = { focusedTasks.firstOrNull() },
        send = { sent += it },
        log = log,
    )

    private fun compose(text: String, cursor: Int = text.length, taskId: Int = 7) = input.handle(
        ImeComposition.newBuilder().setTaskId(taskId).setText(text).setCursor(cursor).build(),
    )

    private fun commit(text: String, taskId: Int = 7) =
        input.handle(ImeCommit.newBuilder().setTaskId(taskId).setText(text).build())

    @Test
    fun fr6_korean_composes_in_the_editor_and_commits_in_place() {
        // 한글: ㅎ, 하, 한, then 한 committed as 글 begins, then 글 committed.
        compose("ㅎ")
        compose("하")
        compose("한")
        assertEquals("한", editor.text.toString())
        assertEquals(0 until 1, editor.composing)
        commit("한")
        compose("ㄱ")
        compose("글")
        commit("글")
        assertEquals("한글", editor.text.toString())
        assertEquals(null, editor.composing)
        assertEquals(2, editor.cursor)
        assertTrue(injector.keys.isEmpty())
    }

    @Test
    fun fr6_backspace_through_the_jamo_removes_the_composition_from_the_editor() {
        commit("a")
        compose("한")
        compose("하")
        compose("ㅎ")
        compose("")
        assertEquals("a", editor.text.toString())
        assertEquals(null, editor.composing)
        assertEquals(1, editor.cursor)
    }

    @Test
    fun fr6_text_composed_in_the_middle_of_a_sentence_goes_where_the_cursor_is() {
        commit("ac")
        editor.cursor = 1
        compose("b")
        commit("b")
        assertEquals("abc", editor.text.toString())
        assertEquals(2, editor.cursor)
    }

    @Test
    fun fr6_the_composing_cursor_is_where_the_host_input_method_has_it() {
        compose("abc", cursor = 1)
        assertEquals(listOf("compose abc 1"), editor.calls)
        // A cursor past the end is at the end.
        compose("abc", cursor = 9)
        assertEquals("compose abc 3", editor.calls.last())
    }

    @Test
    fun fr6_text_for_a_task_without_the_focus_gives_it_the_focus_first() {
        commit("x", taskId = 8)
        assertEquals(listOf(8), focusRequests)
        commit("y", taskId = 7)
        assertEquals(listOf(8), focusRequests)
    }

    @Test
    fun fr6_text_for_a_task_the_host_was_never_told_about_is_dropped_once_with_a_warning() {
        commit("x", taskId = 99)
        commit("y", taskId = 99)
        assertEquals("", editor.text.toString())
        assertEquals(1, log.lines.count { "task 99" in it })
    }

    @Test
    fun fr6_text_typed_where_no_editor_takes_text_goes_as_the_keys_that_type_it() {
        editor.takesText = false
        val key = InjectedKey(1L, 1L, android.view.KeyEvent.ACTION_DOWN, android.view.KeyEvent.KEYCODE_W, 0, 0, 0)
        keysFor = { text -> if (text == "w") listOf(key, key.copy(action = android.view.KeyEvent.ACTION_UP)) else null }
        commit("w")
        assertEquals(
            listOf(android.view.KeyEvent.ACTION_DOWN, android.view.KeyEvent.ACTION_UP),
            injector.keys.map { it.action },
        )
        // A character no key types, and a composition, have nowhere to go.
        commit("한")
        compose("한")
        assertEquals(2, injector.keys.size)
        assertEquals(2, log.lines.count { it.startsWith("warn: dropped") })
    }

    @Test
    fun fr6_the_editor_cursor_goes_to_the_host_in_the_focused_tasks_pixels_once_per_place() {
        input.cursorMoved(140.4f, 90f, 141.6f, 120f)
        input.cursorMoved(140.4f, 90f, 141.6f, 120f)
        input.cursorMoved(150f, 90f, 151f, 120f)
        val cursors = sent.map { decode(it).imeCursor }
        assertEquals(2, cursors.size)
        assertEquals(7, cursors[0].taskId)
        assertEquals(listOf(40, 40, 42, 70), cursors[0].cursor.let { listOf(it.left, it.top, it.right, it.bottom) })
        assertEquals(50, cursors[1].cursor.left)
    }

    @Test
    fun fr6_no_cursor_is_reported_while_no_task_holds_the_focus() {
        focusedTasks.clear()
        input.cursorMoved(1f, 2f, 3f, 4f)
        assertTrue(sent.isEmpty())
    }
}
