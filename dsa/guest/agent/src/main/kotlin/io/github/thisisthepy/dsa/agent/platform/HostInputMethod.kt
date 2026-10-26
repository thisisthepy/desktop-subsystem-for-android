package io.github.thisisthepy.dsa.agent.platform

import android.graphics.Matrix
import android.graphics.RectF
import android.inputmethodservice.InputMethodService
import android.text.InputType
import android.view.inputmethod.CursorAnchorInfo
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.ExtractedTextRequest
import android.view.inputmethod.InputConnection
import io.github.thisisthepy.dsa.agent.input.EditorConnection

/**
 * The guest's input method, which is not a keyboard: it shows nothing, ever, and types what
 * the host's input method composes and commits.
 *
 * The host is the shell, so the user types with the input method of the host, in a window of
 * the host. Android still delivers text to an editor only through an input method, so the
 * agent declares this one, makes it the system's only and default input method
 * ([AndroidInputMethodSetup]), and drives the focused editor's InputConnection from the host's
 * messages ([connection]). It never asks for a window: it has no input view, refuses to be
 * shown, and is never fullscreen, so no soft keyboard appears, with a hardware keyboard or
 * without one.
 *
 * The other way, it asks each editor it is bound to for its cursor and passes where it is on
 * the display to the agent, which tells the host, so the host's candidate list appears beside
 * it.
 *
 * It runs in the agent's process (the activity manager binds it there), so the agent reaches
 * it directly.
 */
class HostInputMethod : InputMethodService() {
    override fun onCreate() {
        super.onCreate()
        current = this
    }

    override fun onDestroy() {
        if (current === this) current = null
        super.onDestroy()
    }

    override fun onEvaluateInputViewShown(): Boolean = false

    override fun onShowInputRequested(flags: Int, configChange: Boolean): Boolean = false

    override fun onEvaluateFullscreenMode(): Boolean = false

    override fun onStartInput(attribute: EditorInfo?, restarting: Boolean) {
        super.onStartInput(attribute, restarting)
        // Where the cursor is now, and again whenever it moves.
        currentInputConnection?.requestCursorUpdates(
            InputConnection.CURSOR_UPDATE_IMMEDIATE or InputConnection.CURSOR_UPDATE_MONITOR,
        )
    }

    override fun onUpdateCursorAnchorInfo(info: CursorAnchorInfo) {
        val rect = cursorRect(info) ?: return
        onCursor?.invoke(rect)
    }

    /** The connection of the editor that takes text now; null when the focus is on no editor. */
    private fun editor(): InputConnection? {
        val info = currentInputEditorInfo ?: return null
        if (info.inputType and InputType.TYPE_MASK_CLASS == InputType.TYPE_NULL) return null
        return currentInputConnection
    }

    companion object {
        @Volatile
        private var current: HostInputMethod? = null

        /** Where the cursor of the focused editor goes, in display pixels; called on any thread. */
        @Volatile
        var onCursor: ((RectF) -> Unit)? = null

        /** The focused editor, through the input method; the agent calls it on its own thread. */
        val connection: EditorConnection = object : EditorConnection {
            override fun setComposing(text: String, cursor: Int): Boolean {
                val ic = current?.editor() ?: return false
                if (text.isEmpty()) {
                    // Removes the composition and what it showed.
                    ic.setComposingText("", 1)
                    ic.finishComposingText()
                    return true
                }
                ic.beginBatchEdit()
                try {
                    // 1 puts the cursor after the text, 0 before it; anywhere else is set after.
                    ic.setComposingText(text, if (cursor == 0) 0 else 1)
                    if (cursor in 1 until text.length) {
                        val end = ic.getExtractedText(ExtractedTextRequest(), 0)
                            ?.let { it.startOffset + it.selectionEnd }
                        if (end != null) {
                            val at = end - (text.length - cursor)
                            ic.setSelection(at, at)
                        }
                    }
                } finally {
                    ic.endBatchEdit()
                }
                return true
            }

            override fun commit(text: String): Boolean {
                val ic = current?.editor() ?: return false
                return ic.commitText(text, 1)
            }
        }

        /**
         * The cursor's rectangle in display pixels: the insertion marker if the editor gives
         * one, else the last character of the composition.
         */
        private fun cursorRect(info: CursorAnchorInfo): RectF? {
            val local = if (!info.insertionMarkerHorizontal.isNaN()) {
                val x = info.insertionMarkerHorizontal
                RectF(x, info.insertionMarkerTop, x + 1f, info.insertionMarkerBottom)
            } else {
                val start = info.composingTextStart
                val text = info.composingText ?: return null
                if (start < 0 || text.isEmpty()) return null
                info.getCharacterBounds(start + text.length - 1) ?: return null
            }
            val toScreen = Matrix(info.matrix)
            toScreen.mapRect(local)
            return local
        }
    }
}
