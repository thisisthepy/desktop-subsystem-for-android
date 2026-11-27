package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The agent's process held a GPU device in the host's graphics renderer (35 MiB) and color
 * buffers for the pictures of three desktop widgets, which a clock redraws once a minute. Its
 * own windows show nothing anyone sees, so they and the widgets' pictures are drawn in
 * software. Only what is drawn for an app, its starting window and its toasts, keeps the GPU,
 * and only while it shows.
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

    private fun Element.children(tag: String): List<Element> {
        val nodes = getElementsByTagName(tag)
        return (0 until nodes.length).map { nodes.item(it) as Element }
    }

    private fun Element.attr(name: String): String = getAttributeNS(android, name)

    private fun sources(): Sequence<File> = File("src/main/kotlin").walkTopDown().filter { it.isFile && it.extension == "kt" }

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
    fun nfr1_only_an_apps_starting_window_asks_the_gpu_for_itself() {
        // A starting window is drawn for the app, as large as its window and with its animated
        // icon, so it keeps the GPU. Nothing else of the agent's may ask for it.
        val asking = sources()
            .filter { file ->
                val text = file.readText()
                "FLAG_HARDWARE_ACCELERATED" in text || "RENDERING_HINT_FORCE_HARDWARE_ACCELERATED" in text
            }
            .map { it.name }
            .toSet()
        assertEquals(setOf("AndroidStartingWindows.kt"), asking)
    }

    @Test
    fun nfr1_widget_pictures_are_drawn_without_a_gpu_device() {
        val widgets = File("src/main/kotlin/io/github/thisisthepy/dsa/agent/platform/AndroidWidgets.kt").readText().replace("\r\n", "\n")
        // The GPU renderer is made in one place: the fallback for views software cannot draw.
        val made = Regex("""HardwareRenderer\(\)""").findAll(widgets).map { it.range.first }.toList()
        assertEquals("the widgets make a GPU renderer outside their fallback", 1, made.size)
        val fallback = widgets.indexOf("inner class GpuFrame")
        assertTrue("the GPU fallback is missing", fallback >= 0)
        val fallbackEnd = widgets.indexOf("\n        }\n", fallback)
        assertTrue("the GPU renderer is made outside the fallback", made.single() in fallback..fallbackEnd)
        // And the fallback is made only when the widget's drawing mode asks for the GPU.
        assertTrue(widgets.contains("WidgetDraw.GPU ->"))
        assertEquals(1, Regex("""GpuFrame\(\)""").findAll(widgets).count())
        // The window a widget's views run in draws none of the widget: in software it would
        // draw every picture twice, and a bitmap that lives on the GPU would end the agent.
        val root = widgets.substring(widgets.indexOf("class CaptureRoot"))
        assertTrue(root.substringBefore("\n    }\n").contains("override fun dispatchDraw"))
    }

    @Test
    fun nfr1_a_widget_window_hears_its_views_change_while_it_draws_in_software() {
        // In a window drawn in software, a view's change goes up through invalidateChildInParent,
        // not onDescendantInvalidated, which only a GPU window calls.
        val widgets = File("src/main/kotlin/io/github/thisisthepy/dsa/agent/platform/AndroidWidgets.kt").readText().replace("\r\n", "\n")
        val root = widgets.substring(widgets.indexOf("class CaptureRoot")).substringBefore("\n    }\n")
        assertTrue(root.contains("override fun invalidateChildInParent"))
        assertTrue(root.contains("override fun onDescendantInvalidated"))
    }
}
