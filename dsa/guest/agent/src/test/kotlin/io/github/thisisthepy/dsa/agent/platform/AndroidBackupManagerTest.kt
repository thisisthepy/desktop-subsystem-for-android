package io.github.thisisthepy.dsa.agent.platform

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AndroidBackupManagerTest {
    private val host = "io.github.thisisthepy.dsa.agent/.platform.HostBackupTransportService"

    @Test
    fun fr36_backup_is_kept_off_with_the_hosts_transport_aosps_own_or_none() {
        assertTrue(AndroidBackupManager.offWith(host, host))
        assertTrue(AndroidBackupManager.offWith(AndroidBackupManager.LOCAL_TRANSPORT, host))
        assertTrue(AndroidBackupManager.offWith(null, host))
        assertTrue(AndroidBackupManager.offWith("", host))
    }

    @Test
    fun fr36_another_apps_transport_is_left_to_that_app_and_the_user() {
        assertFalse(
            AndroidBackupManager.offWith("com.google.android.gms/.backup.BackupTransportService", host),
        )
    }
}
