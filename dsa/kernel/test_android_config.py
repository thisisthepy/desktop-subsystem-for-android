"""Regression tests for the Android-specific guest kernel configuration."""

import os
import re
import unittest
from pathlib import Path


CONFIG = Path(__file__).with_name("android.config")
ARM64_CONFIG = Path(__file__).with_name("android-arm64.config")
PATCHES = Path(__file__).with_name("patches")
BUILD = Path(__file__).with_name("build.sh")


def fragment_lines(path):
    return [line for line in path.read_text().splitlines()
            if re.match(r"^CONFIG_[A-Z0-9_]+=", line) or re.match(r"^# CONFIG_[A-Z0-9_]+ is not set$", line)]


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

    def test_fr5_real_time_display_and_input_threads_preempt_the_kernel(self):
        # config-wsl has PREEMPT_NONE; a woken vsync thread would wait for kernel code to
        # reach a voluntary reschedule.
        config = CONFIG.read_text()
        self.assertIn("# CONFIG_PREEMPT_NONE is not set", config)
        self.assertIn("CONFIG_PREEMPT=y", config)

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

    def test_fr21_the_kernel_keeps_its_last_log_in_pstore_ramoops(self):
        config = CONFIG.read_text()
        for option in [
            "CONFIG_PSTORE=y",
            "CONFIG_PSTORE_CONSOLE=y",
            "CONFIG_PSTORE_PMSG=y",
            "CONFIG_PSTORE_RAM=y",
        ]:
            self.assertIn(option, config)

    def test_fr1_vmbus_root_device_is_platform_device_patch(self):
        patch = (PATCHES / "vmbus-platform-device.patch").read_text()
        self.assertIn("drivers/hv/vmbus_drv.c", patch)
        self.assertIn("vmbus_root_device = &pdev->dev;", patch)
        self.assertIn('device_rename(&pdev->dev, "vmbus")', patch)
        self.assertIn("device_move(&pdev->dev, &platform_bus, DPM_ORDER_NONE)", patch)

    def test_fr10_wakeup_sources_live_where_android_labels_them(self):
        # Android's suspend service reads every wakeup source's statistics. The platform
        # policy labels /sys/devices/virtual/wakeup, but a device's own wakeup source sits
        # below that device, whose Hyper-V path no fixed label can name; the service was
        # refused each read and logged it thousands of times.
        self.assertIn("CONFIG_PM_WAKELOCKS=y", CONFIG.read_text())
        patch = (PATCHES / "wakeup-sources-virtual.patch").read_text()
        self.assertIn("drivers/base/power/wakeup_stats.c", patch)
        self.assertIn("-\tdev->parent = parent;", patch)
        self.assertIn("+\tdev->parent = NULL;", patch)

    def test_fr5_system_server_may_move_apps_between_scheduling_cgroups(self):
        # The cpu, cpuset and blkio controllers are cgroup v1 hierarchies, where only root
        # or the task's own user may move a task. Android's kernels also let CAP_SYS_NICE
        # move it; without that system_server cannot put a foreground app in its groups,
        # and libprocessgroup logs "Failed to add task into cgroup" for each attempt.
        config = CONFIG.read_text()
        for option in ["CONFIG_CGROUPS=y", "CONFIG_CGROUP_SCHED=y", "CONFIG_FAIR_GROUP_SCHED=y",
                       "CONFIG_UCLAMP_TASK_GROUP=y", "CONFIG_CPUSETS=y", "CONFIG_BLK_CGROUP=y",
                       "CONFIG_CGROUP_FREEZER=y", "CONFIG_MEMCG=y"]:
            self.assertIn(option, config)
        patch = (PATCHES / "cgroup-v1-sys-nice.patch").read_text()
        self.assertIn("kernel/cgroup/cgroup-v1.c", patch)
        self.assertIn("+\t    !file_ns_capable(of->file, tcred->user_ns, CAP_SYS_NICE))", patch)

    def test_fr25_the_arm64_kernel_keeps_what_hyper_v_on_arm64_boots_and_returns_memory_with(self):
        # Hyper-V on ARM64 boots the kernel's EFI stub from UEFI and describes the machine
        # with ACPI; the console is a PL011 UART. VMBus, its storage, network, sockets and
        # integration services, and the balloon driver's free page reporting are what the
        # guest reaches the host and hands memory back through.
        config = ARM64_CONFIG.read_text()
        for option in [
            "CONFIG_EFI=y",
            "CONFIG_EFI_STUB=y",
            "CONFIG_ACPI=y",
            "CONFIG_SERIAL_AMBA_PL011=y",
            "CONFIG_SERIAL_AMBA_PL011_CONSOLE=y",
            "CONFIG_HYPERV=y",
            "CONFIG_HYPERV_VMBUS=y",
            "CONFIG_HYPERV_TIMER=y",
            "CONFIG_HYPERV_UTILS=y",
            "CONFIG_HYPERV_STORAGE=y",
            "CONFIG_HYPERV_NET=y",
            "CONFIG_HYPERV_BALLOON=y",
            "CONFIG_PAGE_REPORTING=y",
            "CONFIG_VSOCKETS=y",
            "CONFIG_HYPERV_VSOCKETS=y",
        ]:
            self.assertIn(option, config)

    def test_fr25_the_arm64_kernel_uses_4k_pages(self):
        # The host's free page reporting order counts 4 KiB pages (5 is 128 KiB), and
        # Android's arm64 userspace and apps are built for 4 KiB pages.
        self.assertIn("CONFIG_ARM64_4K_PAGES=y", ARM64_CONFIG.read_text())

    def test_fr25_the_build_makes_an_arm64_image_from_the_wsl_arm64_configuration(self):
        script = BUILD.read_text()
        self.assertIn("Microsoft/config-wsl-arm64", script)
        self.assertIn("android-arm64.config", script)
        self.assertIn("arch/arm64/boot/Image", script)
        self.assertIn("aarch64-linux-gnu-", script)
        # The x86_64 build stays as it was.
        self.assertIn("Microsoft/config-wsl\"", script)
        self.assertIn("arch/x86/boot/bzImage", script)

    def test_fr25_the_arm64_fragment_does_not_repeat_or_contradict_the_common_one(self):
        common = {line.split("=")[0].removeprefix("# ").split(" ")[0] for line in fragment_lines(CONFIG)}
        for line in fragment_lines(ARM64_CONFIG):
            name = line.split("=")[0].removeprefix("# ").split(" ")[0]
            if name in common:
                # Allowed only when it says the same thing.
                self.assertIn(line, CONFIG.read_text().splitlines(), name)

    def test_fr25_no_patch_touches_x86_only_code(self):
        # The same patches apply to both architectures' builds.
        for patch in PATCHES.glob("*.patch"):
            for line in patch.read_text().splitlines():
                if line.startswith(("--- a/", "+++ b/")):
                    self.assertNotIn("arch/x86", line, patch.name)

    def test_fr25_a_merged_configuration_kept_every_fragment_line(self):
        # build.sh --config-only leaves the merged .config; CI points this at each one.
        merged = os.environ.get("DSA_KERNEL_MERGED_CONFIG")
        if not merged:
            self.skipTest("DSA_KERNEL_MERGED_CONFIG names no merged configuration")
        arch = os.environ.get("DSA_KERNEL_ARCH", "x86_64")
        lines = Path(merged).read_text().splitlines()
        fragments = [CONFIG] + ([ARM64_CONFIG] if arch == "arm64" else [])
        for fragment in fragments:
            for line in fragment_lines(fragment):
                self.assertIn(line, lines, f"{fragment.name} for {arch}")
        self.assertFalse(any(line.startswith("CONFIG_ASHMEM=") for line in lines))
        if arch == "arm64":
            self.assertIn("CONFIG_ARM64=y", lines)
        else:
            self.assertIn("CONFIG_X86_64=y", lines)


if __name__ == "__main__":
    unittest.main()
