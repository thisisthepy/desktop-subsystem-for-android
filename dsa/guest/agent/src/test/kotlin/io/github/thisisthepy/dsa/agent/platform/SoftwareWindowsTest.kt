package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The agent's process held a GPU device in the host's graphics renderer (35 MiB) and color
 * buffers for the pictures of the desktop widgets. Its own windows show nothing anyone sees,
 * so they draw in software, and so does every widget whose picture software draws exactly.
 * What is drawn for an app (its starting window and its toasts) keeps the GPU, and so does a
 * widget that needs the GPU, exactly as it was drawn before.
 *
 * The agent's tests run on the JVM without a framework, so these read the manifest and the
 * sources.
 */
class SoftwareWindowsTest {
    private val android = "http://schemas.android.com/apk/res/android"
    private val manifest = DocumentBuilderFactory.newInstance()
        .apply { isNamespaceAware = true }
        .newDocumentBuilder()
        .parse(File("src/main/AndroidManifest.xml"))
        .documentElement

    private val widgets = File("src/main/kotlin/io/github/thisisthepy/dsa/agent/platform/AndroidWidgets.kt")
        .readText()
        .replace("\r\n", "\n")

    private fun Element.children(tag: String): List<Element> {
        val nodes = getElementsByTagName(tag)
        return (0 until nodes.length).map { nodes.item(it) as Element }
    }

    private fun Element.attr(name: String): String = getAttributeNS(android, name)

    private fun sources(): Sequence<File> = File("src/main/kotlin").walkTopDown().filter { it.isFile && it.extension == "kt" }

    /** The body of the class or function that starts at [start], up to its closing brace at [indent]. */
    private fun block(start: String, indent: String): String {
        val at = widgets.indexOf(start)
        assertTrue("$start is missing", at >= 0)
        val end = widgets.indexOf("\n$indent}\n", at)
        assertTrue("$start has no end", end > at)
        return widgets.substring(at, end)
    }

    @Test
    fun nfr1_the_agents_own_windows_draw_without_a_gpu_device() {
        val application = manifest.children("application").single()
        assertEquals("false", application.attr("hardwareAccelerated"))
        for (tag in listOf("activity", "activity-alias")) {
            for (activity in application.children(tag)) {
                assertTrue(
                    "${activity.attr("name")} asks for the GPU; the agent's own windows show nothing that needs it",
                    activity.attr("hardwareAccelerated") != "true",
                )
            }
        }
    }

    @Test
    fun nfr1_only_starting_windows_and_widgets_that_need_the_gpu_ask_for_it() {
        val asking = sources()
            .filter { file ->
                val text = file.readText()
                "FLAG_HARDWARE_ACCELERATED" in text || "RENDERING_HINT_FORCE_HARDWARE_ACCELERATED" in text
            }
            .map { it.name }
            .toSet()
        assertEquals(setOf("AndroidStartingWindows.kt", "AndroidWidgets.kt"), asking)
        // A widget's window asks for the GPU only when the widget is drawn with it.
        val asks = widgets.lines().filter { "FLAG_HARDWARE_ACCELERATED" in it && !it.trimStart().startsWith("//") }
        assertEquals(1, asks.size)
        assertTrue(asks.single().trimStart().startsWith("if (hardware)"))
        assertTrue(widgets.contains("openWindow(id, layoutWidth, layoutHeight, size.densityDpi, root, hardware = gpu)"))
    }

    @Test
    fun nfr1_a_widget_makes_a_gpu_renderer_only_when_it_is_drawn_with_the_gpu() {
        // The GPU renderer, its image reader and its render node are made in one place.
        val made = Regex("""HardwareRenderer\(\)""").findAll(widgets).map { it.range.first }.toList()
        assertEquals("the widgets make a GPU renderer outside their GPU drawing", 1, made.size)
        val frame = widgets.indexOf("private inner class GpuFrame")
        val frameEnd = widgets.indexOf("\n        }\n", frame)
        assertTrue("the GPU renderer is made outside the GPU drawing", made.single() in frame..frameEnd)
        assertEquals(1, Regex("""RenderNode\(""").findAll(widgets).count())
        // And that place is reached only by a widget drawn with the GPU.
        val constructed = Regex("""GpuFrame\(\)""").findAll(widgets).map { it.range.first }.toList()
        assertEquals(1, constructed.size)
        assertTrue(widgets.contains("private var frame: GpuFrame? = if (gpu) GpuFrame() else null"))
        // It is dropped on close, and the last drawing with the GPU asks the process to let
        // the renderer go, so the framework destroys its GPU context.
        val close = block("        fun close() {\n            if (closed) return", "        ")
        assertTrue(close.contains("frame = null"))
        assertTrue(close.contains("if (gpu) gpuClosed()"))
        assertTrue(block("    private fun gpuClosed()", "    ").contains("Runtime.getRuntime().gc()"))
    }

    @Test
    fun fr12_a_widget_drawn_with_the_gpu_is_drawn_exactly_as_before() {
        // Its window draws the widget as before, through render nodes, which clip its views to
        // their outlines and draw their shadows.
        val root = block("    internal class CaptureRoot(", "    ")
        assertTrue(root.contains("if (drawsInWindow || picturing) super.dispatchDraw(canvas)"))
        assertTrue(widgets.contains("CaptureRoot(densityContext(size.densityDpi), drawsInWindow = gpu)"))
        // The software outline clip never touches a drawing with the GPU.
        val view = block("        override fun drawChild(", "        ")
        assertTrue(view.contains("if (failed == null || canvas.isHardwareAccelerated) return super.drawChild(canvas, child, drawingTime)"))
        // No view is changed to make software work: no alpha, layer or outline is set.
        val software = listOf(block("        private fun captureInSoftware()", "        "), view, block("        private fun clipToOutline(", "        "))
        for (part in software) {
            for (setter in listOf("alpha =", "transitionAlpha", "setLayerType", "outlineProvider =", "clipToOutline =", "overlay.add")) {
                assertTrue("the software drawing sets $setter on a view", setter !in part)
            }
        }
    }

    @Test
    fun nfr1_a_widget_window_hears_its_views_change_while_it_draws_in_software() {
        // In a window drawn in software, a view's change goes up through invalidateChildInParent,
        // not onDescendantInvalidated, which only a GPU window calls.
        val root = block("    internal class CaptureRoot(", "    ")
        assertTrue(root.contains("override fun invalidateChildInParent"))
        assertTrue(root.contains("override fun onDescendantInvalidated"))
    }
}
