package io.github.thisisthepy.dsa.agent.windowing

import org.junit.Test
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull

class InsetsSourceFormTest {
    private val head = listOf("android.window.WindowContainerToken", "android.os.IBinder", "int", "int")
    private fun form(fifth: String, vararg rest: String) = head + fifth + rest

    @Test
    fun fr4_android_16_is_given_a_frame_although_a_size_form_has_as_many_parameters() {
        // Android 16 lists the Insets form first; the Rect form is the one a frame goes to.
        val forms = listOf(
            form("android.graphics.Insets", "android.graphics.Rect[]", "int"),
            form("android.graphics.Rect", "android.graphics.Rect[]", "int"),
        )
        assertEquals(InsetsSourceForm.Frame(1, 7), InsetsSourceForm.choose(forms))
    }

    @Test
    fun fr4_older_frameworks_keep_their_frame_forms() {
        assertEquals(
            InsetsSourceForm.Frame(1, 6),
            InsetsSourceForm.choose(listOf(form("android.graphics.Rect"), form("android.graphics.Rect", "android.graphics.Rect[]"))),
        )
    }

    @Test
    fun fr4_a_framework_with_only_the_size_form_gets_a_size() {
        assertEquals(
            InsetsSourceForm.Size(0, 7),
            InsetsSourceForm.choose(listOf(form("android.graphics.Insets", "android.graphics.Rect[]", "int"))),
        )
    }

    @Test
    fun fr22_android_17_is_given_the_form_that_takes_rect_bounding_rectangles() {
        // Android 17 has a frame form with InsetsBoundingRect[] too; the Rect[] one is chosen.
        val forms = listOf(
            form("android.graphics.Rect", "android.view.InsetsBoundingRect[]", "int"),
            form("android.graphics.Insets", "android.graphics.Rect[]", "int"),
            form("android.graphics.Rect", "android.graphics.Rect[]", "int"),
            form("android.graphics.Insets", "android.view.InsetsBoundingRect[]", "int"),
        )
        assertEquals(InsetsSourceForm.Frame(2, 7), InsetsSourceForm.choose(forms))
    }

    @Test
    fun fr22_the_bottom_band_takes_the_size_form_and_a_frame_where_there_is_none() {
        val forms = listOf(
            form("android.graphics.Rect", "android.view.InsetsBoundingRect[]", "int"),
            form("android.graphics.Insets", "android.graphics.Rect[]", "int"),
            form("android.graphics.Rect", "android.graphics.Rect[]", "int"),
        )
        assertEquals(InsetsSourceForm.Size(1, 7), InsetsSourceForm.choose(forms, preferSize = true))
        assertEquals(
            InsetsSourceForm.Frame(0, 6),
            InsetsSourceForm.choose(listOf(form("android.graphics.Rect", "android.graphics.Rect[]")), preferSize = true),
        )
    }

    @Test
    fun fr4_no_known_form_is_reported_as_none() {
        assertNull(InsetsSourceForm.choose(listOf(head)))
    }
}
