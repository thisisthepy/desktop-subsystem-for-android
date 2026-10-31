package io.github.thisisthepy.dsa.agent.input

/**
 * Makes the agent's input method the guest's: enabled, selected, and never showing a soft
 * keyboard, so the host's input method is the only one the user sees.
 */
fun interface InputMethodSetup {
    fun useHostInputMethod()

    companion object {
        val NONE = InputMethodSetup {}
    }
}
