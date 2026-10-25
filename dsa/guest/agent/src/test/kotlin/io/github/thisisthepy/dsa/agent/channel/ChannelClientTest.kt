package io.github.thisisthepy.dsa.agent.channel

import io.github.thisisthepy.dsa.agent.FakeEndpoint
import io.github.thisisthepy.dsa.agent.FakeLocator
import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.RecordingLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ChannelClientTest {
    private val scheduler = FakeScheduler()
    private val locator = FakeLocator()
    private val frames = mutableListOf<String>()
    private var connections = 0
    private val client = ChannelClient(
        locator = locator,
        scheduler = scheduler,
        log = RecordingLog(),
        onFrame = { frames += String(it) },
        onConnected = { connections++ },
        retryMillis = 1000,
    )

    @Test
    fun fr3_the_agent_waits_for_the_control_daemon() {
        scheduler.post { client.start() }
        scheduler.advance(2500)
        assertFalse(client.connected)
        assertFalse(client.send("early".toByteArray()))
        assertEquals(3, locator.lookups)

        val daemon = FakeEndpoint()
        locator.up = daemon
        scheduler.advance(1000)

        assertTrue(client.connected)
        assertEquals(1, connections)
        assertTrue(client.send("frame".toByteArray()))
        assertEquals(listOf("frame"), daemon.sent.map { String(it) })
        daemon.deliver("from the host".toByteArray())
        scheduler.runDue()
        assertEquals(listOf("from the host"), frames)
    }

    @Test
    fun fr3_the_agent_subscribes_again_when_the_daemon_restarts() {
        val first = FakeEndpoint()
        locator.up = first
        scheduler.post { client.start() }
        scheduler.runDue()
        assertEquals(1, connections)

        locator.up = null
        first.die()
        scheduler.runDue()
        assertFalse(client.connected)

        val second = FakeEndpoint()
        locator.up = second
        scheduler.advance(1000)

        assertTrue(client.connected)
        assertEquals(2, connections)
        assertNotNull(second.subscriber)
        // A frame the dead connection still delivers is not taken for one of the new connection.
        first.subscriber!!.invoke("stale".toByteArray())
        second.deliver("fresh".toByteArray())
        scheduler.runDue()
        assertEquals(listOf("fresh"), frames)
    }

    @Test
    fun fr3_a_send_to_a_dead_daemon_starts_looking_for_it_again() {
        val first = FakeEndpoint()
        locator.up = first
        scheduler.post { client.start() }
        scheduler.runDue()

        first.dead = true
        assertFalse(client.send("lost".toByteArray()))
        assertFalse(client.connected)

        locator.up = FakeEndpoint()
        scheduler.advance(1000)
        assertTrue(client.connected)
        assertEquals(2, connections)
    }

    @Test
    fun fr3_a_daemon_that_dies_while_subscribing_is_looked_up_again() {
        locator.up = FakeEndpoint().apply { dead = true }
        scheduler.post { client.start() }
        scheduler.runDue()
        assertFalse(client.connected)

        locator.up = FakeEndpoint()
        scheduler.advance(1000)
        assertTrue(client.connected)
    }
}
