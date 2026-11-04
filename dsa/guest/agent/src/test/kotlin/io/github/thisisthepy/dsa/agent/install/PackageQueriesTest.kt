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
import io.github.thisisthepy.dsa.control.v1.QueryPackage
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PackageQueriesTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val log = RecordingLog()
    private val signer = ByteArray(32) { it.toByte() }

    private val packages = PackageInfoPort { name ->
        when (name) {
            "com.example.notes" -> InstalledPackage(41, "4.1", listOf(signer))
            "com.example.broken" -> throw IllegalStateException("the package manager died")
            else -> null
        }
    }

    private fun started(): Agent {
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            packages = packages,
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun ask(id: Int, name: String) {
        daemon.deliver(
            Envelope.newBuilder().setQueryPackage(QueryPackage.newBuilder().setRequestId(id).setPackageName(name))
                .build().toByteArray(),
        )
        scheduler.runDue()
    }

    private fun answers() = daemon.sentMessages().filter { it.hasPackageState() }.map { it.packageState }

    @Test
    fun fr11_the_host_learns_the_installed_version_and_signer_of_a_package() {
        started()
        ask(3, "com.example.notes")
        val state = answers().single()
        assertEquals(3, state.requestId)
        assertTrue(state.installed)
        assertEquals(41L, state.versionCode)
        assertEquals("4.1", state.versionName)
        assertArrayEquals(signer, state.signerSha256List.single().toByteArray())
    }

    @Test
    fun fr11_a_package_that_is_not_installed_is_answered_as_such() {
        started()
        ask(4, "com.example.missing")
        ask(5, "com.example.broken")
        val states = answers()
        assertEquals(listOf(4, 5), states.map { it.requestId })
        assertTrue(states.none { it.installed })
        assertFalse(states.any { it.signerSha256Count > 0 })
    }
}
