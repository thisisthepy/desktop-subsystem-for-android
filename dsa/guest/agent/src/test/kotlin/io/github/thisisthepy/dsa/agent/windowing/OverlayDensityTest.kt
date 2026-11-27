package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.RecordingLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class OverlayDensityTest {
    private val actions = FakeActions()
    private val log = RecordingLog()
    private val density = OverlayDensity(actions, log)

    @Test
    fun fr41_an_overlay_window_is_given_its_density_through_the_framework() {
        actions.overlayDensity = true

        assertTrue(density.give("faa9c03", 262))
        assertTrue(density.give("faa9c03", 0))

        assertEquals(listOf("faa9c03" to 262, "faa9c03" to 0), actions.overlayDensities)
        // The framework is asked once whether it can.
        assertEquals(1, actions.overlayDensityAsked)
    }

    @Test
    fun fr41_an_image_without_the_change_gives_no_overlay_a_density() {
        actions.overlayDensity = false
        actions.overlayDensitiesBefore["faa9c03"] = 262

        assertFalse(density.give("faa9c03", 262))
        assertEquals(0, density.given("faa9c03"))

        assertTrue(actions.overlayDensities.isEmpty())
        assertFalse(density.available)
        assertEquals(1, actions.overlayDensityAsked)
    }

    @Test
    fun fr41_the_density_a_window_was_given_before_is_read_from_the_framework() {
        actions.overlayDensity = true
        actions.overlayDensitiesBefore["faa9c03"] = 262

        assertEquals(262, density.given("faa9c03"))
        assertEquals(0, density.given("b0b0b0"))
    }
}
