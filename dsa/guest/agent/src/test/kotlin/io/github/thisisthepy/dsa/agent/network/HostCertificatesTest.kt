package io.github.thisisthepy.dsa.agent.network

import com.google.protobuf.ByteString
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.FileSharing
import io.github.thisisthepy.dsa.control.v1.HostCertificates as HostCertificatesMessage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class HostCertificatesTest {
    private val log = RecordingLog()
    private var notices = 0
    private val certificates = HostCertificates({ notices += 1 }, log)

    private fun message(enabled: Boolean, vararg ders: ByteArray) = Envelope.newBuilder().setHostCertificates(
        HostCertificatesMessage.newBuilder()
            .setEnabled(enabled)
            .addAllCertificates(ders.map { ByteString.copyFrom(it) }),
    ).build()

    @Test
    fun fr28_the_running_apps_look_their_trust_anchors_up_again_when_the_hosts_roots_change() {
        assertTrue(certificates.handle(message(true, byteArrayOf(0x30, 0x00))))
        assertEquals("added roots are found at the next look", 1, notices)
        assertTrue(certificates.handle(message(false)))
        assertEquals("a root taken away stops being trusted in running apps", 2, notices)
    }

    @Test
    fun fr28_other_frames_are_left_for_the_other_handlers() {
        val sharing = Envelope.newBuilder().setFileSharing(FileSharing.newBuilder().setOn(true)).build()
        assertFalse(certificates.handle(sharing))
        assertEquals(0, notices)
    }

    @Test
    fun fr28_a_notice_that_fails_is_logged_and_the_frame_is_still_taken() {
        val failing = HostCertificates({ throw SecurityException("not allowed") }, log)
        assertTrue(failing.handle(message(true)))
        assertTrue(log.lines.any { it.startsWith("warn: the running apps could not be told") })
    }
}
