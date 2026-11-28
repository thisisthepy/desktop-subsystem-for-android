package io.github.thisisthepy.dsa.agent.platform

import android.content.res.Resources
import android.view.Display
import android.view.InputWindowHandle
import android.window.WindowInfosListener
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.overlays.GuestWindow
import io.github.thisisthepy.dsa.agent.overlays.OverlaySource
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds

/**
 * The default display's windows, from SurfaceFlinger's own list
 * (android.window.WindowInfosListener, with the ACCESS_SURFACE_FLINGER permission a
 * platform-signed app is granted).
 *
 * The window manager tells no app about another app's windows that are in no task, and it
 * cannot be changed here. SurfaceFlinger lists every window that has a surface for whoever
 * dispatches input, with what the window manager said of each: its type, its flags, its owner
 * and its frame, front to back, again whenever any of them changes (an app that moves its
 * overlay with updateViewLayout changes its geometry). The listener is called on a binder
 * thread and SurfaceFlinger holds back its next list until it returns, so it only copies what
 * it needs and hands it on.
 *
 * A window's frame comes without its surface insets; the surface the hwcomposer sees is that
 * much larger on every side, so the insets are added back. Frames are in the display's own
 * pixels, which are the logical ones while the display is not rotated, and the guest's never
 * is.
 */
class AndroidOverlayWindows(private val log: Log) : OverlaySource {
    private var listener: WindowInfosListener? = null

    override fun listen(onChange: (List<GuestWindow>) -> Unit): Boolean {
        return try {
            val created = object : WindowInfosListener() {
                override fun onWindowInfosChanged(
                    windowHandles: Array<InputWindowHandle>?,
                    displayInfos: Array<WindowInfosListener.DisplayInfo>?,
                ) {
                    onChange(windows(windowHandles))
                }
            }
            val now = created.register()
            listener = created
            onChange(windows(now?.first))
            true
        } catch (e: SecurityException) {
            log.warn("may not read SurfaceFlinger's windows; app overlay windows are not shown on the host", e)
            false
        } catch (e: RuntimeException) {
            log.warn("could not listen to SurfaceFlinger's windows; app overlay windows are not shown on the host", e)
            false
        } catch (e: LinkageError) {
            log.warn("this framework does not list SurfaceFlinger's windows; app overlay windows are not shown on the host", e)
            false
        }
    }

    override fun density(): Int = Resources.getSystem().displayMetrics.densityDpi

    private fun windows(handles: Array<InputWindowHandle>?): List<GuestWindow> {
        if (handles == null) return emptyList()
        val out = ArrayList<GuestWindow>(handles.size)
        for (handle in handles) {
            if (handle.displayId != Display.DEFAULT_DISPLAY) continue
            val name = handle.name.orEmpty()
            val client = try {
                handle.windowToken
            } catch (e: RuntimeException) {
                null
            }
            val identity: Any = client ?: handle.token ?: name
            val inset = maxOf(0, handle.surfaceInset)
            val frame = handle.frame
            out += GuestWindow(
                identity = identity,
                packageName = handle.packageName.orEmpty(),
                ownerUid = handle.ownerUid,
                type = handle.layoutParamsType,
                surface = TaskBounds(frame.left - inset, frame.top - inset, frame.right + inset, frame.bottom + inset),
                // The name is the window's identity hash, a space, and its title.
                title = name.substringAfter(' ', ""),
                windowName = name.substringBefore(' '),
                visible = handle.inputConfig and NOT_VISIBLE == 0 && handle.alpha > 0f && !frame.isEmpty,
                focusable = handle.inputConfig and NOT_FOCUSABLE == 0,
                touchable = handle.inputConfig and NOT_TOUCHABLE == 0,
            )
        }
        return out
    }

    private companion object {
        // android.os.InputConfig.
        const val NOT_VISIBLE = 1 shl 1
        const val NOT_FOCUSABLE = 1 shl 2
        const val NOT_TOUCHABLE = 1 shl 3
    }
}
