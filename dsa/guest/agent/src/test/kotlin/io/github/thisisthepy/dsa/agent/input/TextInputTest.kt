package io.github.thisisthepy.dsa.agent.input

import android.text.InputType
import android.view.inputmethod.EditorInfo
import io.github.thisisthepy.dsa.agent.FakeInjector
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.control.v1.ImeCommit
import io.github.thisisthepy.dsa.control.v1.ImeDeleteSurrounding
import io.github.thisisthepy.dsa.control.v1.ImeComposition
import io.github.thisisthepy.dsa.control.v1.ImeInputKind
import io.github.thisisthepy.dsa.control.v1.ImeSetComposingRegion
import io.github.thisisthepy.dsa.control.v1.ImeSetSelection
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

    override fun deleteSurrounding(before: Int, after: Int): Boolean {
        calls += "delete $before $after"
        if (!takesText) return false
        val from = (cursor - before).coerceAtLeast(0)
        val to = (cursor + after).coerceAtMost(text.length)
        text.delete(cursor, to)
        text.delete(from, cursor)
        composing = composing?.let { (it.first - (cursor - from)) until (it.last + 1 - (cursor - from)) }
        cursor = from
        return true
    }

    override fun setSelection(start: Int, end: Int): Boolean {
        calls += "select $start $end"
        if (!takesText) return false
        cursor = end
        return true
    }

    override fun setComposingRegion(start: Int, end: Int): Boolean {
        calls += "region $start $end"
        if (!takesText) return false
        composing = if (start == end) null else start until end
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
    fun fr6_the_input_method_selects_and_reconverts_text_in_the_editor() {
        commit("한글")
        input.handle(ImeSetSelection.newBuilder().setTaskId(7).setStart(0).setEnd(1).build())
        input.handle(ImeSetComposingRegion.newBuilder().setTaskId(7).setStart(0).setEnd(1).build())
        assertEquals(0 until 1, editor.composing)
        compose("漢")
        commit("漢")
        assertEquals("漢글", editor.text.toString())
        // An empty region ends the composition and keeps its text.
        input.handle(ImeSetComposingRegion.newBuilder().setTaskId(7).setStart(1).setEnd(1).build())
        assertEquals(null, editor.composing)
        assertEquals(listOf("select 0 1", "region 0 1"), editor.calls.filter { it.startsWith("select") || it.startsWith("region 0") })
    }

    @Test
    fun fr6_the_editor_goes_to_the_host_for_the_focused_task_once_per_state() {
        val state = EditorState(ImeInputKind.IME_INPUT_KIND_EMAIL, "me@", 10, 13, 13, 0, 0)
        input.editorChanged(state)
        input.editorChanged(state)
        input.editorChanged(EditorState.NONE)
        val editors = sent.map { decode(it).imeEditor }
        assertEquals(2, editors.size)
        assertEquals(7, editors[0].taskId)
        assertEquals(ImeInputKind.IME_INPUT_KIND_EMAIL, editors[0].kind)
        assertEquals("me@", editors[0].text)
        assertEquals(listOf(10, 13, 13), editors[0].let { listOf(it.textStart, it.selectionStart, it.selectionEnd) })
        assertEquals(ImeInputKind.IME_INPUT_KIND_NONE, editors[1].kind)
    }

    @Test
    fun fr6_no_editor_is_reported_while_no_task_holds_the_focus() {
        focusedTasks.clear()
        input.editorChanged(EditorState.NONE)
        assertTrue(sent.isEmpty())
    }

    @Test
    fun fr6_each_input_type_maps_to_the_kind_of_text_the_host_offers_keys_for() {
        val search = EditorInfo.IME_ACTION_SEARCH
        val cases = listOf(
            Triple(InputType.TYPE_NULL, 0, ImeInputKind.IME_INPUT_KIND_NONE),
            Triple(InputType.TYPE_CLASS_TEXT, 0, ImeInputKind.IME_INPUT_KIND_TEXT),
            Triple(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD, 0, ImeInputKind.IME_INPUT_KIND_PASSWORD),
            Triple(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD, 0, ImeInputKind.IME_INPUT_KIND_PASSWORD),
            Triple(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD, 0, ImeInputKind.IME_INPUT_KIND_PASSWORD),
            Triple(InputType.TYPE_CLASS_NUMBER, 0, ImeInputKind.IME_INPUT_KIND_NUMBER),
            Triple(InputType.TYPE_CLASS_NUMBER or InputType.TYPE_NUMBER_VARIATION_PASSWORD, 0, ImeInputKind.IME_INPUT_KIND_NUMBER_PASSWORD),
            Triple(InputType.TYPE_CLASS_PHONE, 0, ImeInputKind.IME_INPUT_KIND_PHONE),
            Triple(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS, 0, ImeInputKind.IME_INPUT_KIND_EMAIL),
            Triple(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI, 0, ImeInputKind.IME_INPUT_KIND_URL),
            Triple(InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PERSON_NAME, 0, ImeInputKind.IME_INPUT_KIND_PERSON_NAME),
            Triple(InputType.TYPE_CLASS_DATETIME or InputType.TYPE_DATETIME_VARIATION_DATE, 0, ImeInputKind.IME_INPUT_KIND_DATE),
            Triple(InputType.TYPE_CLASS_DATETIME or InputType.TYPE_DATETIME_VARIATION_TIME, 0, ImeInputKind.IME_INPUT_KIND_TIME),
            Triple(InputType.TYPE_CLASS_TEXT, search, ImeInputKind.IME_INPUT_KIND_SEARCH),
        )
        for ((type, options, kind) in cases) {
            assertEquals("$type $options", kind, inputKind(type, options))
        }
    }

    @Test
    fun fr6_the_input_method_deletes_around_the_cursor() {
        commit("abcd")
        input.handle(
            ImeDeleteSurrounding.newBuilder().setTaskId(7).setBeforeLength(2).setAfterLength(0).build(),
        )
        assertEquals("ab", editor.text.toString())
        assertEquals(2, editor.cursor)
    }

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
