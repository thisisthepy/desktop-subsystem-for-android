package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import javax.xml.parsers.DocumentBuilderFactory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.w3c.dom.Element

/**
 * The agent can be the guest's browser only if it declares an activity that takes every http
 * and https link, and it can serve the host's files only through a provider no app reaches
 * without a grant.
 */
class LinkManifestTest {
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

    @Test
    fun fr9_the_link_activity_takes_every_http_and_https_link() {
        val activity = application.children("activity").single { it.attr("name") == ".platform.LinkActivity" }
        assertEquals("true", activity.attr("exported"))
        assertEquals("@android:style/Theme.NoDisplay", activity.attr("theme"))
        val filter = activity.children("intent-filter").single()
        assertEquals(setOf("android.intent.action.VIEW"), filter.names("action"))
        assertEquals(
            setOf("android.intent.category.DEFAULT", "android.intent.category.BROWSABLE"),
            filter.names("category"),
        )
        val data = filter.children("data")
        assertEquals(setOf("http", "https"), data.map { it.attr("scheme") }.toSet())
        assertTrue("a browser takes every host", data.all { it.attr("host").isEmpty() })
        assertTrue(
            "the browser role needs the role manager's permission",
            "android.permission.MANAGE_ROLE_HOLDERS" in manifest.names("uses-permission"),
        )
    }

    @Test
    fun fr9_the_host_files_are_served_only_with_a_grant() {
        val provider = application.children("provider").single { it.attr("name") == ".platform.HostFileProvider" }
        assertEquals("false", provider.attr("exported"))
        assertEquals("true", provider.attr("grantUriPermissions"))
        assertEquals("io.github.thisisthepy.dsa.agent.hostfiles", provider.attr("authorities"))
    }
}
