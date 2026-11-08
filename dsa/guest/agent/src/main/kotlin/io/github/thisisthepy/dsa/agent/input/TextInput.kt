package io.github.thisisthepy.dsa.agent.input

import android.text.InputType
import android.view.inputmethod.EditorInfo
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.ImeCommit
import io.github.thisisthepy.dsa.control.v1.ImeComposition
import io.github.thisisthepy.dsa.control.v1.ImeCursor
import io.github.thisisthepy.dsa.control.v1.ImeDeleteSurrounding
import io.github.thisisthepy.dsa.control.v1.ImeEditor
import io.github.thisisthepy.dsa.control.v1.ImeInputKind
import io.github.thisisthepy.dsa.control.v1.ImeSetComposingRegion
import io.github.thisisthepy.dsa.control.v1.ImeSetSelection
import io.github.thisisthepy.dsa.control.v1.Rect

/**
 * The input method's side of the focused editor's InputConnection: what the agent's input
 * method (platform/HostInputMethod.kt) does with the host's text.
 */
interface EditorConnection {
    /**
     * Makes [text] the editor's composition, in place of the one it has, with the cursor
     * [cursor] UTF-16 code units from the start of [text]. An empty [text] removes the
     * composition. False when no editor takes text now.
     */
    fun setComposing(text: String, cursor: Int): Boolean

    /** Types [text] in place of the composition, which ends. False when no editor takes text now. */
    fun commit(text: String): Boolean

    /**
     * Deletes [before] UTF-16 code units before the cursor and [after] after it, keeping the
     * composition. False when no editor takes text now.
     */
    fun deleteSurrounding(before: Int, after: Int): Boolean

    /**
     * Selects from [start] to [end], offsets in UTF-16 code units from the start of the
     * editor's text, keeping the composition. False when no editor takes text now.
     */
    fun setSelection(start: Int, end: Int): Boolean

    /**
     * Makes the text from [start] to [end] the composition, as the host's input method does
     * when it reconverts a word; equal offsets end the composition and keep its text. False
     * when no editor takes text now.
     */
    fun setComposingRegion(start: Int, end: Int): Boolean

    companion object {
        /** No editor, as before the input method starts. */
        val NONE = object : EditorConnection {
            override fun setComposing(text: String, cursor: Int) = false
            override fun commit(text: String) = false
            override fun deleteSurrounding(before: Int, after: Int) = false
            override fun setSelection(start: Int, end: Int) = false
            override fun setComposingRegion(start: Int, end: Int) = false
        }
    }
}

/**
 * The focused editor as the host's input method reads it: the kind of text it takes, and its
 * text around the selection. Offsets are UTF-16 code units from the start of the editor's
 * whole text; [selectionStart] is the anchor and [selectionEnd] the active end. A composition
 * whose start equals its end is none.
 */
data class EditorState(
    val kind: ImeInputKind,
    val text: String,
    val textStart: Int,
    val selectionStart: Int,
    val selectionEnd: Int,
    val compositionStart: Int,
    val compositionEnd: Int,
) {
    companion object {
        /** No editor takes text: the focus is on a view that reads keys, or on nothing. */
        val NONE = EditorState(ImeInputKind.IME_INPUT_KIND_NONE, "", 0, 0, 0, 0, 0)

        /** The most text around the selection the host takes, in UTF-16 code units. */
        const val MAX_TEXT = 4096

        /** How far the text reaches on each side of the selection, where the editor's goes on. */
        const val REACH = 1024
    }
}

/**
 * The kind of text an editor takes, from its EditorInfo's input type and options, so the
 * host's input method offers the keys for it and keeps composition and prediction out of a
 * password.
 */
fun inputKind(inputType: Int, imeOptions: Int): ImeInputKind {
    val variation = inputType and InputType.TYPE_MASK_VARIATION
    return when (inputType and InputType.TYPE_MASK_CLASS) {
        InputType.TYPE_NULL -> ImeInputKind.IME_INPUT_KIND_NONE
        InputType.TYPE_CLASS_NUMBER ->
            if (variation == InputType.TYPE_NUMBER_VARIATION_PASSWORD) {
                ImeInputKind.IME_INPUT_KIND_NUMBER_PASSWORD
            } else {
                ImeInputKind.IME_INPUT_KIND_NUMBER
            }
        InputType.TYPE_CLASS_PHONE -> ImeInputKind.IME_INPUT_KIND_PHONE
        InputType.TYPE_CLASS_DATETIME -> when (variation) {
            InputType.TYPE_DATETIME_VARIATION_DATE -> ImeInputKind.IME_INPUT_KIND_DATE
            InputType.TYPE_DATETIME_VARIATION_TIME -> ImeInputKind.IME_INPUT_KIND_TIME
            else -> ImeInputKind.IME_INPUT_KIND_TEXT
        }
        InputType.TYPE_CLASS_TEXT -> when (variation) {
            InputType.TYPE_TEXT_VARIATION_PASSWORD,
            InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD,
            InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD,
            -> ImeInputKind.IME_INPUT_KIND_PASSWORD
            InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS,
            InputType.TYPE_TEXT_VARIATION_WEB_EMAIL_ADDRESS,
            -> ImeInputKind.IME_INPUT_KIND_EMAIL
            InputType.TYPE_TEXT_VARIATION_URI -> ImeInputKind.IME_INPUT_KIND_URL
            InputType.TYPE_TEXT_VARIATION_PERSON_NAME -> ImeInputKind.IME_INPUT_KIND_PERSON_NAME
            else ->
                if (imeOptions and EditorInfo.IME_MASK_ACTION == EditorInfo.IME_ACTION_SEARCH) {
                    ImeInputKind.IME_INPUT_KIND_SEARCH
                } else {
                    ImeInputKind.IME_INPUT_KIND_TEXT
                }
        }
        else -> ImeInputKind.IME_INPUT_KIND_TEXT
    }
}

/** The key events that type some text where no editor takes text, such as a game's view. */
fun interface TextKeys {
    /** The presses and releases that type [text] in order; null when a character has no key. */
    fun keysFor(text: String): List<InjectedKey>?
}

/**
 * Puts the host's text into the guest: what the host's input method composes and commits in a
 * task's window, and the characters its keys type.
 *
 * The guest has no keyboard of its own: the agent's input method is the system's only one,
 * shows nothing, and is the input method side of whichever editor has the focus. A
 * composition becomes the editor's composing text, underlined where the user types it, with
 * the cursor where the host's input method has it; a commit replaces it. Before text goes to
 * a task that does not hold the input focus, the task is given it, as for a key.
 *
 * Text typed where no editor takes text (a game, a view of its own that reads keys) goes as
 * the key events of the guest's virtual keyboard for each character, so typing still works
 * there; a composition there has nowhere to go and is dropped.
 *
 * The other way, the input method reports where the editor's cursor is on the display, and
 * this class tells the host where it is in the focused task, so the host's candidate list
 * appears beside it.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class TextInput(
    private val editor: () -> EditorConnection,
    private val injector: InputInjector,
    private val keys: TextKeys,
    /** Whether the host was told about the task. */
    private val isReported: (Int) -> Boolean,
    /** Whether the task holds the input focus in the guest, as last read. */
    private val isFocused: (Int) -> Boolean,
    /** Gives the task the input focus; false when the framework refused. */
    private val focus: (Int) -> Boolean,
    /** Where the host was told the task is on the display. */
    private val bounds: (Int) -> TaskBounds?,
    /** The task the host was told holds the input focus, if any. */
    private val focusedTask: () -> Int?,
    /** Sends one control frame to the host. */
    private val send: (ByteArray) -> Unit,
    private val log: Log,
) {
    private val unknownTasks = HashSet<Int>()
    private var lastCursor: Pair<Int, Rect>? = null
    private var lastEditor: Pair<Int, EditorState>? = null
    private var failing = false

    /** Acts on a composition from the host. */
    fun handle(composition: ImeComposition) {
        val taskId = composition.taskId
        if (!reach(taskId)) return
        val text = composition.text
        val cursor = composition.cursor.coerceIn(0, text.length)
        if (!editor().setComposing(text, cursor) && text.isNotEmpty()) {
            log.warn("dropped a composition for task $taskId: no editor there takes text")
        }
    }

    /** Acts on committed text from the host. */
    fun handle(commit: ImeCommit) {
        val taskId = commit.taskId
        if (!reach(taskId)) return
        val text = commit.text
        if (text.isEmpty() || editor().commit(text)) return
        val typed = keys.keysFor(text)
        if (typed == null) {
            log.warn("dropped text for task $taskId: no editor there takes text, and no key types it")
            return
        }
        for (key in typed) inject(key)
    }

    /** Deletes text around the editor's cursor, as the host's input method asks. */
    fun handle(delete: ImeDeleteSurrounding) {
        val taskId = delete.taskId
        if (!reach(taskId)) return
        val before = delete.beforeLength.coerceAtLeast(0)
        val after = delete.afterLength.coerceAtLeast(0)
        if (before == 0 && after == 0) return
        if (!editor().deleteSurrounding(before, after)) {
            log.warn("dropped a deletion for task $taskId: no editor there takes text")
        }
    }

    /** Selects text in the editor, as the host's input method asks. */
    fun handle(selection: ImeSetSelection) {
        val taskId = selection.taskId
        if (!reach(taskId)) return
        if (selection.start < 0 || selection.end < 0) return
        if (!editor().setSelection(selection.start, selection.end)) {
            log.warn("dropped a selection for task $taskId: no editor there takes text")
        }
    }

    /** Makes text in the editor the composition again, as the host's input method asks. */
    fun handle(region: ImeSetComposingRegion) {
        val taskId = region.taskId
        if (!reach(taskId)) return
        if (region.start < 0 || region.end < region.start) return
        if (!editor().setComposingRegion(region.start, region.end)) {
            log.warn("dropped a composing region for task $taskId: no editor there takes text")
        }
    }

    /**
     * The focused editor is now [state]. The host hears it for the focused task, once for each
     * state, so its input method reads the editor's own text.
     */
    fun editorChanged(state: EditorState) {
        val taskId = focusedTask() ?: return
        if (lastEditor == taskId to state) return
        lastEditor = taskId to state
        send(
            Envelope.newBuilder()
                .setImeEditor(
                    ImeEditor.newBuilder()
                        .setTaskId(taskId)
                        .setKind(state.kind)
                        .setText(state.text)
                        .setTextStart(state.textStart)
                        .setSelectionStart(state.selectionStart)
                        .setSelectionEnd(state.selectionEnd)
                        .setCompositionStart(state.compositionStart)
                        .setCompositionEnd(state.compositionEnd),
                )
                .build()
                .toByteArray(),
        )
    }

    /**
     * The focused editor's cursor is at these display pixels. The host hears where it is in
     * the focused task, once for each place it moves to.
     */
    fun cursorMoved(left: Float, top: Float, right: Float, bottom: Float) {
        val taskId = focusedTask() ?: return
        val task = bounds(taskId) ?: return
        val rect = Rect.newBuilder()
            .setLeft(Math.round(left) - task.left)
            .setTop(Math.round(top) - task.top)
            .setRight(Math.round(right) - task.left)
            .setBottom(Math.round(bottom) - task.top)
            .build()
        if (lastCursor == taskId to rect) return
        lastCursor = taskId to rect
        send(
            Envelope.newBuilder()
                .setImeCursor(ImeCursor.newBuilder().setTaskId(taskId).setCursor(rect))
                .build()
                .toByteArray(),
        )
    }

    /** Forgets the tasks not in [taskIds], as tasks end. */
    fun retainTasks(taskIds: Set<Int>) {
        unknownTasks.removeAll(taskIds)
        if (lastCursor?.first !in taskIds) lastCursor = null
        if (lastEditor?.first !in taskIds) lastEditor = null
    }

    /** Whether text for the task can go to it: the host knows the task, and it holds the focus. */
    private fun reach(taskId: Int): Boolean {
        if (!isReported(taskId)) {
            if (unknownTasks.add(taskId)) {
                log.warn("dropped text input for task $taskId, which the host was never told about")
            }
            return false
        }
        unknownTasks.remove(taskId)
        if (!isFocused(taskId) && !focus(taskId)) {
            log.warn("could not give task $taskId the input focus; the text may reach another task")
        }
        return true
    }

    private fun inject(key: InjectedKey) {
        val injected = try {
            injector.injectKey(key)
        } catch (e: RuntimeException) {
            if (!failing) log.warn("the framework refused the keys of typed text", e)
            failing = true
            return
        }
        if (!injected) {
            if (!failing) log.warn("the framework refused the keys of typed text")
            failing = true
        } else {
            failing = false
        }
    }
}
