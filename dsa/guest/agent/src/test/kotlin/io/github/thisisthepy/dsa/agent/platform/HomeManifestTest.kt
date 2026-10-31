package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The guest has no launcher, but Android always starts a home activity. Without one of its
 * own it starts Settings' FallbackHome, which shares the Settings process: when that
 * process was busy starting Settings, FallbackHome answered a focus change too late, the
 * system killed the process for it, and the user's Settings task went with it. So the agent
 * declares the home activity, in its own always-running process whose main thread does
 * nothing else.
 */
class HomeManifestTest {
    private val android = "http://schemas.android.com/apk/res/android"
    private val manifest = DocumentBuilderFactory.newInstance()
        .apply { isNamespaceAware = true }
        .newDocumentBuilder()
        .parse(File("src/main/AndroidManifest.xml"))
        .documentElement

    private val application = manifest.children("application").single()

    private fun Element.children(tag: String): List<Element> {
        val nodes = getElementsByTagName(tag)
        return (0 until nodes.length).map { nodes.item(it) as Element }
    }

    private fun Element.attr(name: String): String = getAttributeNS(android, name)

    private fun Element.names(tag: String): Set<String> = children(tag).map { it.attr("name") }.toSet()

    private fun homeActivities(): List<Element> = application.children("activity").filter { activity ->
        activity.children("intent-filter").any { filter ->
            "android.intent.action.MAIN" in filter.names("action") &&
                "android.intent.category.HOME" in filter.names("category")
        }
    }

    @Test
    fun fr3_the_agent_declares_the_guests_home_activity() {
        val home = homeActivities().singleOrNull()
        assertNotNull("the agent declares no home activity, so Settings' FallbackHome takes the role", home)
        val filter = home!!.children("intent-filter").single()
        // The activity manager resolves home with CATEGORY_DEFAULT as well.
        assertTrue("android.intent.category.DEFAULT" in filter.names("category"))
        // The role controller and the activity manager must be able to resolve it.
        assertEquals("true", home.attr("exported"))
    }

    @Test
    fun fr3_the_home_activity_runs_in_the_agents_persistent_process() {
        val home = homeActivities().single()
        // A separate process would start cold, as FallbackHome's did, and could be slow to
        // answer a focus change; the agent's persistent process is always running and its
        // main thread is free, because the agent's work runs on a thread of its own.
        assertEquals("", home.attr("process"))
        assertEquals("", application.attr("process"))
        assertEquals("true", application.attr("persistent"))
    }

    @Test
    fun fr3_the_home_activity_is_available_before_the_user_unlocks() {
        val home = homeActivities().single()
        // Before the first unlock only direct-boot-aware activities resolve, and FallbackHome
        // would take home until then.
        val directBootAware = home.attr("directBootAware").ifEmpty { application.attr("directBootAware") }
        assertEquals("true", directBootAware)
    }

    @Test
    fun fr3_the_home_activity_shows_nothing_and_is_no_app_the_user_can_launch() {
        val home = homeActivities().single()
        // The host is the shell: the home activity draws nothing and is not in the app list
        // the host shows, which holds only activities in the launcher category.
        assertEquals("@android:style/Theme.Translucent.NoTitleBar", home.attr("theme"))
        assertEquals("true", home.attr("excludeFromRecents"))
        for (filter in home.children("intent-filter")) {
            assertFalse("android.intent.category.LAUNCHER" in filter.names("category"))
        }
    }
}
