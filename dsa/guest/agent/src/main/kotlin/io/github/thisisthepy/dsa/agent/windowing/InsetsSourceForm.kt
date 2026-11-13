package io.github.thisisthepy.dsa.agent.windowing

/**
 * Which form of WindowContainerTransaction.addInsetsSource to call, read from the forms this
 * framework has (each as its parameter type names).
 *
 * Android 16 has two forms with seven parameters: the frame in display pixels (Rect) and the
 * size relative to the task's edges (Insets). Picking a form by its parameter count alone
 * found the Insets one and passed it a Rect, which the framework refused, so no task got a
 * top inset and every app drew its toolbar under the host's caption buttons. A frame form is
 * preferred; the Insets form is used when it is the only one.
 *
 * Android 17 adds forms whose bounding rectangles are InsetsBoundingRect[] beside the Rect[]
 * ones. The agent gives bounding rectangles as Rect[], so only a form that takes Rect[] there
 * (or has no such parameter) is chosen. Types are named as Class.getTypeName names them.
 *
 * A source along the task's bottom edge prefers the size form ([choose]'s preferSize): its
 * frame is the task's own bounds less that size, which the window manager works out again in
 * the same layout as each resize, so the band stays on the task's bottom edge while the task
 * is resized. A frame in display pixels is given again only after the agent has read the new
 * bounds; until then the old frame sits inside or below the resized task, and the app's
 * bottom inset would jump at each step of a resize.
 */
sealed interface InsetsSourceForm {
    /** The index of this form in the list it was chosen from. */
    val index: Int

    /** addInsetsSource(token, owner, index, type, Rect frame[, Rect[] bounding[, int flags]]). */
    data class Frame(override val index: Int, val parameterCount: Int) : InsetsSourceForm

    /** addInsetsSource(token, owner, index, type, Insets size[, Rect[] bounding[, int flags]]). */
    data class Size(override val index: Int, val parameterCount: Int) : InsetsSourceForm

    companion object {
        private const val RECT = "android.graphics.Rect"
        private const val RECTS = "android.graphics.Rect[]"
        private const val INSETS = "android.graphics.Insets"

        /**
         * The form to call among [forms], or null when none takes a frame or a size. A frame
         * form is chosen over a size form unless [preferSize].
         */
        fun choose(forms: List<List<String>>, preferSize: Boolean = false): InsetsSourceForm? {
            fun candidates(kind: String) = forms.withIndex()
                .filter { (_, types) -> types.size in 5..7 && types[4] == kind && (types.size < 6 || types[5] == RECTS) }
                .maxByOrNull { (_, types) -> types.size }
            val frame = candidates(RECT)?.let { (index, types) -> Frame(index, types.size) }
            val size = candidates(INSETS)?.let { (index, types) -> Size(index, types.size) }
            return if (preferSize) size ?: frame else frame ?: size
        }
    }
}
