"""Regression tests for the Android-specific guest kernel configuration."""

import unittest
from pathlib import Path


CONFIG = Path(__file__).with_name("android.config")
PATCHES = Path(__file__).with_name("patches")


class AndroidKernelConfigTests(unittest.TestCase):
    def test_fr1_ashmem_is_off_and_memfd_is_on(self):
        config = CONFIG.read_text()
        self.assertNotIn("CONFIG_ASHMEM", config)
        self.assertIn("CONFIG_MEMFD_CREATE=y", config)

    def test_fr1_memfds_answer_the_ashmem_ioctls_android_checks_for(self):
        # libcutils uses memfd only when a memfd answers ASHMEM_GET_SIZE; otherwise it
        # opens /dev/ashmem, which this kernel does not have, and system_server dies.
        config = CONFIG.read_text()
        self.assertIn("CONFIG_MEMFD_ASHMEM_SHIM=y", config)
        patch = (PATCHES / "memfd-ashmem-shim.patch").read_text()
        self.assertIn("mm/memfd-ashmem-shim.c", patch)
        self.assertIn("case ASHMEM_GET_SIZE:", patch)
        self.assertIn(".unlocked_ioctl\t= shmem_file_ioctl,", patch)
        self.assertIn("config MEMFD_ASHMEM_SHIM", patch)

    def test_pr6_the_kernel_reads_the_bootconfig_that_carries_the_boot_secret(self):
        # Without it the kernel ignores the bootconfig the host appends to the initrd, the
        # guest's control daemon finds no secret, and the session never becomes active.
        self.assertIn("CONFIG_BOOT_CONFIG=y", CONFIG.read_text())

    def test_fr1_netd_firewall_and_traffic_rules_are_built_in(self):
        # The image ships no kernel modules, and netd exits when it cannot create the
        # iptables filter and mangle tables; init then restarts zygote with it.
        config = CONFIG.read_text()
        for option in [
            "CONFIG_IP_NF_FILTER=y",
            "CONFIG_IP_NF_MANGLE=y",
            "CONFIG_IP_NF_NAT=y",
            "CONFIG_IP6_NF_FILTER=y",
            "CONFIG_IP6_NF_MANGLE=y",
            "CONFIG_NETFILTER_XT_MATCH_CONNMARK=y",
            "CONFIG_NETFILTER_XT_TARGET_MARK=y",
            "CONFIG_NETFILTER_XT_TARGET_IDLETIMER=y",
            "CONFIG_NET_SCH_INGRESS=y",
            "CONFIG_XFRM_USER=y",
        ]:
            self.assertIn(option, config)

    def test_fr1_binder_device_nodes_are_created_for_android_init(self):
        config = CONFIG.read_text()
        self.assertIn('CONFIG_ANDROID_BINDER_DEVICES="binder,hwbinder,vndbinder"', config)

    def test_fr1_fbe_file_name_encryption_supports_aes_hctr2(self):
        config = CONFIG.read_text()
        self.assertIn("CONFIG_CRYPTO_HCTR2=y", config)

    def test_fr1_dm_verity_can_check_blake2b_hash_trees_in_first_stage(self):
        # The image's system_ext hash tree uses blake2b-256; first stage init sets up its
        # verity device before any module can load, so the hash must be built in.
        config = CONFIG.read_text()
        self.assertIn("CONFIG_CRYPTO_BLAKE2B=y", config)

    def test_pr3_hyper_v_socket_transport_is_built_into_the_guest_kernel(self):
        config = CONFIG.read_text()
        self.assertIn("CONFIG_VSOCKETS=y", config)
        self.assertIn("CONFIG_HYPERV_VSOCKETS=y", config)

    def test_pr3_vgem_is_built_into_the_guest_kernel(self):
        config = CONFIG.read_text()
        self.assertIn("CONFIG_DRM=y", config)
        self.assertIn("CONFIG_DRM_VGEM=y", config)

    def test_pr3_minigbm_has_a_drm_device_it_can_allocate_from(self):
        # The image's minigbm has no vgem backend; on a VM without virtio-gpu it allocates
        # dumb buffers from a vkms card node, which must exist before the allocator starts.
        config = CONFIG.read_text()
        self.assertIn("CONFIG_DRM_VKMS=y", config)

    def test_nfr2_free_guest_pages_are_reported_to_hyper_v(self):
        # The host gets the guest's freed memory back only through the balloon driver's free
        # page reporting, in the blocks compaction makes.
        config = CONFIG.read_text()
        for option in ["CONFIG_HYPERV_BALLOON=y", "CONFIG_PAGE_REPORTING=y", "CONFIG_COMPACTION=y"]:
            self.assertIn(option, config)

    def test_nfr1_the_guest_can_swap_to_zram(self):
        config = CONFIG.read_text()
        for option in [
            "CONFIG_SWAP=y",
            "CONFIG_ZSMALLOC=y",
            "CONFIG_ZRAM=y",
            "CONFIG_ZRAM_BACKEND_LZ4=y",
            "CONFIG_ZRAM_DEF_COMP_LZ4=y",
            "CONFIG_PSI=y",
            "CONFIG_MEMCG=y",
        ]:
            self.assertIn(option, config)

    def test_fr1_vmbus_root_device_is_platform_device_patch(self):
        patch = (PATCHES / "vmbus-platform-device.patch").read_text()
        self.assertIn("drivers/hv/vmbus_drv.c", patch)
        self.assertIn("vmbus_root_device = &pdev->dev;", patch)
        self.assertIn('device_rename(&pdev->dev, "vmbus")', patch)
        self.assertIn("device_move(&pdev->dev, &platform_bus, DPM_ORDER_NONE)", patch)


if __name__ == "__main__":
    unittest.main()
