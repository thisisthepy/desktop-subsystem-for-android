package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.requests.TaskActions

/**
 * The apps the user set to keep game focus, as the framework was last told.
 *
 * Android tells an app that it lost the focus when the user clicks another app's window,
 * although DSA keeps the app's task visible and resumed beside it, and a game such as Monument
 * Valley pauses then. DSA's framework does not tell an app it was given ([TaskActions.setKeepFocus])
 * while its task stays visible; keys and text still go only to the window that has the focus.
 *
 * The host sends the list when it connects and whenever the user changes it ([hostSays]), and
 * the guest's control daemon hands the last one to an agent that starts after it, so the
 * framework hears it again after the agent or Android starts again. The first list is always
 * given, also an empty one, since the window manager may hold one from an agent that ran
 * before; after that only a list that differs. An image whose framework lacks the change is
 * not asked, and says so once.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class KeepFocus(private val actions: TaskActions, private val log: Log) {
    private var supported: Boolean? = null
    private var given: Set<String>? = null

    /**
     * Whether the framework can keep an app's focus. Asked once, the first time it is needed:
     * the framework does not change while the agent runs.
     */
    val available: Boolean
        get() = supported ?: actions.keepFocusSupported().also { found ->
            supported = found
            log.info(
                if (found) {
                    "the framework keeps the focus of the apps set to keep game focus"
                } else {
                    "the framework cannot keep an app's focus; keep game focus does nothing on this image"
                },
            )
        }

    /** The host's latest list of the apps that keep their focus. */
    fun hostSays(packages: Set<String>) {
        if (packages == given) return
        if (!available) return
        if (actions.setKeepFocus(packages)) {
            given = packages
            log.info("${packages.size} apps keep their focus while another app's window has it")
        } else {
            // Given again with the host's next list.
            log.warn("the framework did not take the ${packages.size} apps that keep their focus")
        }
    }
}
