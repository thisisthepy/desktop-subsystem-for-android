package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.appTask
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

class AppAnnouncementTest {
    private val sent = mutableListOf<Envelope>()
    private var channelUp = true
    private val lookups = mutableListOf<String>()
    private val apps = AppInfoSource { packageName ->
        lookups += packageName
        if (packageName == "org.example.gone") null else AppDetails("Name of $packageName", byteArrayOf(1, 2, 3))
    }
    private val reporter = TaskReporter(AGENT, apps) { frame ->
        if (channelUp) sent += decode(frame)
        channelUp
    }

    private fun kinds() = sent.map { it.bodyCase }

    @Test
    fun fr3_an_apps_name_and_icon_go_before_its_first_task() {
        reporter.update(listOf(appTask(1, packageName = "org.example.mail")))

        assertEquals(listOf(Envelope.BodyCase.APP_INFO, Envelope.BodyCase.TASK_UPDATED), kinds())
        val info = sent[0].appInfo
        assertEquals("org.example.mail", info.packageName)
        assertEquals("Name of org.example.mail", info.label)
        assertArrayEquals(byteArrayOf(1, 2, 3), info.iconPng.toByteArray())
    }

    @Test
    fun fr3_an_app_is_described_once_while_it_has_tasks() {
        reporter.update(listOf(appTask(1, packageName = "org.example.mail")))
        reporter.update(
            listOf(
                appTask(1, packageName = "org.example.mail", label = "Inbox"),
                appTask(2, packageName = "org.example.mail"),
            ),
        )

        assertEquals(1, sent.count { it.bodyCase == Envelope.BodyCase.APP_INFO })
        assertEquals(listOf("org.example.mail"), lookups)
    }

    @Test
    fun fr3_an_app_with_no_task_left_is_described_again_before_its_next_task() {
        reporter.update(listOf(appTask(1, packageName = "org.example.mail")))
        reporter.update(emptyList())
        sent.clear()

        reporter.update(listOf(appTask(5, packageName = "org.example.mail")))

        assertEquals(listOf(Envelope.BodyCase.APP_INFO, Envelope.BodyCase.TASK_UPDATED), kinds())
    }

    @Test
    fun fr3_a_new_connection_describes_every_app_again() {
        reporter.update(listOf(appTask(1, packageName = "org.example.mail")))
        sent.clear()

        reporter.resend()
        reporter.update(listOf(appTask(1, packageName = "org.example.mail")))

        assertEquals(listOf(Envelope.BodyCase.APP_INFO, Envelope.BodyCase.TASK_UPDATED), kinds())
    }

    @Test
    fun fr3_a_task_waits_until_its_app_has_been_described() {
        channelUp = false
        reporter.update(listOf(appTask(1, packageName = "org.example.mail")))
        channelUp = true

        reporter.update(listOf(appTask(1, packageName = "org.example.mail")))

        assertEquals(listOf(Envelope.BodyCase.APP_INFO, Envelope.BodyCase.TASK_UPDATED), kinds())
    }

    @Test
    fun fr3_an_app_the_agent_cannot_describe_still_gets_its_window() {
        reporter.update(listOf(appTask(1, packageName = "org.example.gone")))

        assertEquals(listOf(Envelope.BodyCase.TASK_UPDATED), kinds())
    }
}
