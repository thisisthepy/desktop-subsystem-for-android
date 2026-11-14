package io.github.thisisthepy.dsa.agent.platform

import android.app.Activity
import android.os.Bundle
import android.view.Gravity

/**
 * The guest's home activity. It is not a launcher: it has no content, draws nothing (its
 * theme is transparent), and nobody sees it, because the host shows only app tasks.
 *
 * Android always keeps a home activity at the bottom of the display and moves input focus to
 * it whenever no app task holds focus. With no home of its own, the guest used Settings'
 * FallbackHome, which runs in the Settings process. Its window had to answer each focus
 * change within 5 seconds, and it could not while that process's main thread was busy
 * starting up or creating a Settings activity; the activity manager then killed the whole
 * process, and an open Settings task with it.
 *
 * This one runs in the agent's persistent process, which is always running, and whose main
 * thread does nothing else: the agent's own work runs on a thread of its own. So it answers
 * at once, and no app's process holds the home task.
 */
class HomeActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // One pixel, as the drag catcher's window is. At the display's size (a square as long
        // as the longest monitor) its two transparent buffers took 32 MiB each in the guest
        // and as much again on the host, for a window nobody sees. It still takes input focus
        // when no app holds it, which is all it is for.
        window.setLayout(1, 1)
        window.setGravity(Gravity.TOP or Gravity.START)
    }
}
