package io.github.thisisthepy.dsa.agent.install

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.Agent
import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.FakeEndpoint
import io.github.thisisthepy.dsa.agent.FakeInjector
import io.github.thisisthepy.dsa.agent.FakeLocator
import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.FakeTasks
import io.github.thisisthepy.dsa.agent.FakeWindowing
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.GoogleDeviceIdQuery
import org.junit.Assert.assertEquals
import org.junit.Test

class GoogleDeviceIdsTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()

    private fun started(port: GoogleDeviceIdPort): Agent {
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            googleDeviceId = port,
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun ask(): List<String> {
        daemon.deliver(
            Envelope.newBuilder().setGoogleDeviceIdQuery(GoogleDeviceIdQuery.getDefaultInstance())
                .build().toByteArray(),
        )
        scheduler.runDue()
        return daemon.sentMessages().filter { it.hasGoogleDeviceId() }.map { it.googleDeviceId.id }
    }

    @Test
    fun fr32_the_host_learns_the_google_services_framework_android_id() {
        started { "3940584823456789012" }
        assertEquals(listOf("3940584823456789012"), ask())
    }

    @Test
    fun fr32_no_id_yet_is_answered_empty() {
        started { null }
        assertEquals(listOf(""), ask())
    }

    @Test
    fun fr32_a_failure_to_ask_is_answered_empty() {
        started { throw SecurityException("the services framework refused") }
        assertEquals(listOf(""), ask())
    }
}
