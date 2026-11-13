"""Regression tests for the Android-specific guest kernel configuration."""

import os
import re
import unittest
from pathlib import Path


CONFIG = Path(__file__).with_name("android.config")
ARM64_CONFIG = Path(__file__).with_name("android-arm64.config")
VIRTIO_CONFIG = Path(__file__).with_name("virtio.config")
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

    def test_fr28_both_families_can_redirect_dns_queries_to_the_guests_stub(self):
        # dsa-channeld --dns-redirect fills a nat OUTPUT chain for IPv4 and IPv6 that sends
        # port 53 to the daemon's stub, which hands each query to the host's resolver.
        config = CONFIG.read_text()
        for option in ["CONFIG_IP_NF_NAT=y", "CONFIG_IP6_NF_NAT=y", "CONFIG_NETFILTER_XT_TARGET_REDIRECT=y"]:
            self.assertIn(option, config)

    def test_fr32_listed_apps_see_the_computers_memory_in_meminfo_and_sysinfo(self):
        # Google Play reads the device's memory from /proc/meminfo and sysinfo(); the apps the
        # guest lists see the computer's memory there, every other process the VM's own.
        self.assertIn("CONFIG_DSA_HOST_MEMORY=y", CONFIG.read_text())
        patch = (PATCHES / "dsa-host-memory.patch").read_text()
        self.assertIn("+++ b/fs/proc/dsa_host_memory.c", patch)
        # Both read paths take it: MemTotal in /proc/meminfo and sysinfo()'s totalram.
        for path in ("fs/proc/meminfo.c", "kernel/sys.c"):
            hunk = patch[patch.index(f"+++ b/{path}"):]
            hunk = hunk[:hunk.find("diff --git", 1)] if "diff --git" in hunk[1:] else hunk
            self.assertIn("+\tdsa_host_memory_adjust(", hunk, path)
        # Keyed on the calling process's own uid, never on anything a binder peer says.
        self.assertIn("dsa_app_listed(current_uid())", patch)
        self.assertNotIn("drivers/android/binder.c", patch)
        # Only the total changes, and only upwards; free and available memory stay the VM's.
        self.assertIn("+\tif (pages > info->totalram)\n+\t\tinfo->totalram = pages;", patch)
        self.assertNotIn("freeram =", patch)
        # Parameters the bootconfig's kernel.dsa_host_memory.* reach.
        self.assertIn('+#define MODULE_PARAM_PREFIX "dsa_host_memory."', patch)
        self.assertIn("+module_param_named(total_kb, dsa_total_kb, ulong, 0444);", patch)
        self.assertIn("+module_param_cb(uids, &dsa_uids_ops, NULL, 0644);", patch)

    def test_fr32_the_host_memory_patch_hunks_count_their_lines(self):
        header = re.compile(r"^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@")
        old_left = new_left = 0
        current = None
        for line in (PATCHES / "dsa-host-memory.patch").read_text().splitlines():
            match = header.match(line)
            if match:
                self.assertEqual((old_left, new_left), (0, 0), current)
                current = line
                old_left = int(match.group(1) or 1)
                new_left = int(match.group(2) or 1)
                continue
            if old_left == 0 and new_left == 0:
                continue
            if line.startswith("+"):
                new_left -= 1
            elif line.startswith("-"):
                old_left -= 1
            else:
                old_left -= 1
                new_left -= 1
        self.assertEqual((old_left, new_left), (0, 0), current)

    def test_fr35_the_su_guard_is_built_in_and_hides_su_by_inode(self):
        # su exists only for the uids the host allows; the guard returns -ENOENT for any
        # other task, so an app that may not ask never finds su (INTENT D28).
        config = CONFIG.read_text()
        self.assertIn("CONFIG_DSA_SU_GUARD=y", config)
        self.assertIn("CONFIG_SECURITYFS=y", config)
        patch = (PATCHES / "dsa-su-guard-lsm.patch").read_text()
        self.assertIn("security/dsa_su/dsa_su.c", patch)
        self.assertIn("return -ENOENT;", patch)
        self.assertIn("LSM_HOOK_INIT(inode_permission", patch)
        self.assertIn("LSM_HOOK_INIT(bprm_creds_for_exec", patch)
        self.assertIn("securityfs_create_dir(\"dsa_su\"", patch)

    def test_fr35_the_su_guard_is_in_the_lsm_list_before_selinux(self):
        # A security module CONFIG_LSM does not name never initialises, and the guard must
        # answer before SELinux so a hidden su is a missing file, not a denial.
        match = re.search(r'^CONFIG_LSM="([^"]*)"$', CONFIG.read_text(), re.MULTILINE)
        self.assertIsNotNone(match, "android.config sets CONFIG_LSM")
        modules = match.group(1).split(",")
        self.assertIn("dsa_su", modules)
        self.assertIn("selinux", modules)
        self.assertLess(modules.index("dsa_su"), modules.index("selinux"))

    def test_fr35_a_disabled_guard_hides_su_from_every_uid(self):
        # With root elevation off, su is a missing file for every app; the guard switched
        # off never shows a watched su to all.
        patch = (PATCHES / "dsa-su-guard-lsm.patch").read_text()
        self.assertIn("+\tif (watched && dsa_su_state.enabled) {", patch)
        self.assertIn("+\tif (!watched || allowed)", patch)

    def test_fr35_a_missing_su_path_does_not_stop_the_rest_of_a_write(self):
        # The image keeps su at one of the watched paths; a path that does not exist is
        # skipped, and a write applies as a whole or not at all.
        patch = (PATCHES / "dsa-su-guard-lsm.patch").read_text()
        self.assertIn("+\tif (err == -ENOENT || err == -ENOTDIR)\n+\t\treturn 0;", patch)
        self.assertIn("+\t*next = dsa_su_state;", patch)
        self.assertIn("+\t\tdsa_su_state = *next;", patch)

    def test_fr35_the_su_guard_builds_against_the_pinned_kernel_and_has_an_lsm_slot(self):
        # The pinned tree declares securityfs in <linux/security.h> and has no
        # <linux/securityfs.h>. It sizes the LSM id list and every hook's static call slots
        # by the modules lsm_count.h knows, and panics at boot on a module it does not count.
        patch = (PATCHES / "dsa-su-guard-lsm.patch").read_text()
        self.assertNotIn("linux/securityfs.h", patch)
        self.assertIn("+#include <linux/security.h>", patch)
        self.assertIn("+++ b/include/linux/lsm_count.h", patch)
        self.assertIn("+#if IS_ENABLED(CONFIG_DSA_SU_GUARD)\n+#define DSA_SU_ENABLED 1,", patch)
        self.assertIn("+\t\tDSA_SU_ENABLED)", patch)

    def test_fr35_the_su_guard_patch_hunks_count_their_lines(self):
        # git apply refuses a hunk whose header miscounts its lines.
        header = re.compile(r"^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@")
        old_left = new_left = 0
        current = None
        for line in (PATCHES / "dsa-su-guard-lsm.patch").read_text().splitlines():
            match = header.match(line)
            if match:
                self.assertEqual((old_left, new_left), (0, 0), current)
                current = line
                old_left = int(match.group(1) or 1)
                new_left = int(match.group(2) or 1)
                continue
            if old_left == 0 and new_left == 0:
                continue
            if line.startswith("+"):
                new_left -= 1
            elif line.startswith("-"):
                old_left -= 1
            else:
                old_left -= 1
                new_left -= 1
        self.assertEqual((old_left, new_left), (0, 0), current)

    def test_nfr1_built_in_parts_the_guest_never_uses_are_left_out(self):
        # The kernel image stays resident for the VM's life. config-wsl builds these in for a
        # WSL distribution; the guest's disks, file systems and initramfs need none of them.
        lines = fragment_lines(CONFIG)
        for option in ["XFS_FS", "SQUASHFS", "ISO9660_FS", "UDF_FS", "EXT2_FS", "BLK_DEV_NVME",
                       "BLK_DEV_RAM", "KSM", "KALLSYMS_ALL"]:
            self.assertIn(f"# CONFIG_{option} is not set", lines)
        # What Android's storage does use stays built in.
        for option in ["CONFIG_EROFS_FS=y", "CONFIG_F2FS_FS=y"]:
            self.assertIn(option, lines)

    def test_nfr1_zram_has_zstd_as_a_second_compressor_for_cold_pages(self):
        # The idle reclaim compresses what stays in zram again with zstd; without
        # multi-compression the kernel refuses the second compressor and keeps LZ4 alone.
        config = CONFIG.read_text()
        for option in ["CONFIG_ZRAM_BACKEND_LZ4=y", "CONFIG_ZRAM_BACKEND_ZSTD=y",
                       "CONFIG_ZRAM_DEF_COMP_LZ4=y", "CONFIG_ZRAM_MULTI_COMP=y"]:
            self.assertIn(option, config)

    def test_nfr1_reclaim_ages_pages_with_the_multi_generational_lru(self):
        # The guest's gradual trim reclaims a bounded amount of the coldest cache; without
        # MGLRU it cannot tell cold cache from the visible app's working set as well.
        config = CONFIG.read_text()
        self.assertIn("CONFIG_LRU_GEN=y", config)
        self.assertIn("CONFIG_LRU_GEN_ENABLED=y", config)

    def test_nfr1_the_slab_caches_can_be_read_one_by_one(self):
        # Without SLUB's debugging support there is no /proc/slabinfo, and the guest's
        # unreclaimable slab is one sum the memory measurement cannot break down. The support
        # stays idle: nothing turns its checks on.
        lines = fragment_lines(CONFIG)
        self.assertIn("CONFIG_SLUB_DEBUG=y", lines)
        self.assertIn("# CONFIG_SLUB_DEBUG_ON is not set", lines)

    def test_nfr1_a_read_only_ext4_mount_keeps_no_conversion_thread(self):
        # Android mounts every APEX package as a read-only ext4 image (about 75 with Google
        # Play), and each mount kept a rescuer thread for a conversion workqueue it can never
        # use. The patch makes the workqueue only for a mount that can write, or when one is
        # remounted read-write, and guards every other use of it.
        patch = (PATCHES / "ext4-rsv-conversion-read-only.patch").read_text()
        self.assertIn("+++ b/fs/ext4/super.c", patch)
        self.assertIn("+++ b/fs/ext4/page-io.c", patch)
        self.assertIn("+\tif (!sb_rdonly(sb) || !bdev_read_only(sb->s_bdev)) {", patch)
        self.assertIn("+static int ext4_alloc_rsv_conversion_wq(struct super_block *sb)", patch)
        # Remounting read-write makes it before the file system may write.
        self.assertIn("+\t\t\tif (!sbi->rsv_conversion_wq) {", patch)
        # Unmounting, syncing and a write's completion each cope with a mount without one.
        self.assertIn("+\tif (sbi->rsv_conversion_wq)\n+\t\tdestroy_workqueue(sbi->rsv_conversion_wq);", patch)
        self.assertIn("+\tif (sbi->rsv_conversion_wq)\n+\t\tflush_workqueue(sbi->rsv_conversion_wq);", patch)
        self.assertIn("+\tif (WARN_ON_ONCE(!wq))\n+\t\twq = system_unbound_wq;", patch)

    def test_nfr1_the_ext4_patch_hunks_count_their_lines(self):
        # git apply refuses a hunk whose header miscounts its lines.
        header = re.compile(r"^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@")
        old_left = new_left = 0
        current = None
        for line in (PATCHES / "ext4-rsv-conversion-read-only.patch").read_text().splitlines():
            match = header.match(line)
            if match:
                self.assertEqual((old_left, new_left), (0, 0), current)
                current = line
                old_left = int(match.group(1) or 1)
                new_left = int(match.group(2) or 1)
                continue
            if old_left == 0 and new_left == 0:
                continue
            if line.startswith("+"):
                new_left -= 1
            elif line.startswith("-"):
                old_left -= 1
            else:
                old_left -= 1
                new_left -= 1
        self.assertEqual((old_left, new_left), (0, 0), current)

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

    def test_fr25_an_arm_translation_package_can_be_mounted_and_run(self):
        # init mounts the package's erofs image from its file, binds it into /system, and
        # registers its translator for ARM64 executables in binfmt_misc. The Google apps
        # image is laid over with overlayfs.
        config = CONFIG.read_text()
        for option in ["CONFIG_EROFS_FS=y", "CONFIG_EROFS_FS_BACKED_BY_FILE=y", "CONFIG_OVERLAY_FS=y",
                       "CONFIG_BINFMT_MISC=y"]:
            self.assertIn(option, config)

    def test_nfr2_fr25_fr32_the_images_are_mounted_from_their_files_without_a_loop_device(self):
        # A loop device kept every page read from the Google apps or the translation image
        # twice: once in erofs's page cache and once in the image file's own. erofs mounts an
        # image file itself from Linux 6.12, and its directio option, which the scripts give,
        # is in the same release.
        lines = fragment_lines(CONFIG)
        self.assertIn("CONFIG_EROFS_FS=y", lines)
        self.assertIn("CONFIG_EROFS_FS_BACKED_BY_FILE=y", lines)
        tag = Path(__file__).with_name("wsl-kernel.tag").read_text().strip()
        version = re.search(r"(\d+)\.(\d+)", tag)
        self.assertIsNotNone(version, tag)
        self.assertGreaterEqual((int(version.group(1)), int(version.group(2))), (6, 12), tag)
        image = Path(__file__).parent.parent / "guest" / "image"
        # Each image is a file on the user's add-on disk (and, for one boot after an update, the
        # copy an older DSA kept in /data), or the default translator in /system.
        images = {
            "dsa-gapps.rc": ["/mnt/dsa-addons/gapps.img", "/data/vendor/dsa/gapps/system.img"],
            "dsa-arm-translation.rc": ["/mnt/dsa-addons/arm-translation.img",
                                       "/data/vendor/dsa/arm-translation/system.img",
                                       "/system/etc/dsa/arm-translation/default.img"],
            "dsa-widevine.rc": ["/mnt/dsa-addons/widevine.img"],
        }
        # In the host's direct-mapped mode each add-on image is mounted from its own partition on
        # the persistent memory the add-on disk is then, with DAX, so it is not read into a cache.
        partitions = {"dsa-gapps.rc": "/dev/block/by-name/dsa_gapps",
                      "dsa-arm-translation.rc": "/dev/block/by-name/dsa_arm_translation",
                      "dsa-widevine.rc": "/dev/block/by-name/dsa_widevine"}
        for name, files in images.items():
            mounts = [line.split() for line in (image / name).read_text().splitlines()
                      if line.strip().startswith("mount erofs ")]
            mapped = [words for words in mounts if words[2] == partitions[name]]
            self.assertEqual(len(mapped), 1, name)
            self.assertIn("dax=always", mapped[0][-1].split(","), name)
            mounts = [words for words in mounts if words not in mapped]
            self.assertEqual(sorted(words[2] for words in mounts), sorted(files), name)
            for words in mounts:
                self.assertNotIn("loop@", " ".join(words), name)
                # The kernel gets only the last argument that is not one of init's flags, so
                # directio shares it with the SELinux contexts.
                self.assertIn("directio", words[-1].split(","), name)
        # The add-on disk itself is an erofs partition the kernel mounts from its block device.
        disk = [line.split() for line in (image / "dsa-addons.rc").read_text().splitlines()
                if line.strip().startswith("mount erofs ")]
        self.assertEqual([words[2] for words in disk], ["/dev/block/by-name/dsa_addons"])
        # The loop driver stays for apexd.
        self.assertIn("CONFIG_BLK_DEV_LOOP=y", lines)

    def test_nfr1_the_system_images_can_be_mapped_from_persistent_memory_with_dax(self):
        # With the direct-mapped system setting the host attaches the read-only disks as
        # persistent memory (Hyper-V's through ACPI NFIT, virtio-pmem under KVM), and erofs maps
        # their files with dax=always instead of caching them in the guest.
        lines = fragment_lines(CONFIG)
        for option in ["CONFIG_ACPI_NFIT=y", "CONFIG_LIBNVDIMM=y", "CONFIG_BLK_DEV_PMEM=y",
                       "CONFIG_VIRTIO_PMEM=y", "CONFIG_ZONE_DEVICE=y", "CONFIG_DAX=y",
                       "CONFIG_FS_DAX=y", "CONFIG_EROFS_FS=y"]:
            self.assertIn(option, lines)
        # dm-linear carries DAX for the logical partitions; dm-verity cannot, so the image's
        # direct-mapped table verifies nothing itself (the host checks the disk instead).
        patch = (PATCHES / "pmem-dax.patch").read_text()
        self.assertIn("drivers/acpi/nfit/core.c", patch)
        # A raw namespace on Hyper-V gets page structs, which DAX needs, without a pfn info
        # block the read-only device could not hold.
        self.assertIn("+\t\tset_bit(ND_REGION_PAGEMAP, &ndr_desc->flags);", patch)
        # The NVDIMM bus sits below the platform device "nfit", which the image names as a boot
        # device, so first stage init makes by-name links for the persistent memory disk.
        self.assertIn('platform_device_register_simple("nfit", PLATFORM_DEVID_NONE,', patch)
        self.assertIn("nvdimm_bus_register(acpi_nfit_bus_parent(dev),", patch)
        self.assertIn("+#include <linux/platform_device.h>", patch)
        # virtio-pmem's bus moves below the platform device "virtio-pmem" the same way: the VMM
        # picks a pmem device's PCI slot itself, so the slot cannot be named as a boot device.
        self.assertIn("drivers/nvdimm/virtio_pmem.c", patch)
        self.assertIn('platform_device_register_simple("virtio-pmem",', patch)
        self.assertIn("nvdimm_bus_register(virtio_pmem_bus_parent(vdev),", patch)

    def test_nfr1_persistent_memory_disks_are_read_only_in_the_guest(self):
        # Every persistent memory device in the guest is a disk image the host maps read only,
        # so its region is read only and the pmem driver marks its disk read only: a write
        # fails in the guest rather than reaching the host's mapping, as on the read-only SCSI
        # disk outside the direct-mapped mode.
        patch = (PATCHES / "pmem-dax.patch").read_text().replace("\r\n", "\n")
        self.assertIn("+\tND_REGION_READONLY = 5,", patch)
        self.assertIn("+\tif (test_bit(ND_REGION_READONLY, &ndr_desc->flags))\n+\t\tro = 1;", patch)
        self.assertIn("+\t\tset_bit(ND_REGION_READONLY, &ndr_desc->flags);", patch)
        self.assertIn("+\tset_bit(ND_REGION_READONLY, &ndr_desc.flags);", patch)

    def test_nfr1_the_pmem_patch_hunks_count_their_lines(self):
        text = (PATCHES / "pmem-dax.patch").read_text().replace("\r\n", "\n")
        hunks = re.split(r"(?m)^(@@ -\d+,(\d+) \+\d+,(\d+) @@.*)$", text)
        self.assertGreater(len(hunks), 1)
        for at in range(1, len(hunks), 4):
            old, new = int(hunks[at + 1]), int(hunks[at + 2])
            body = [line for line in hunks[at + 3].strip("\n").split("\n")
                    if not line.startswith(("diff --git ", "--- a/", "+++ b/"))]
            self.assertEqual(sum(1 for line in body if line[:1] in (" ", "-")), old, hunks[at])
            self.assertEqual(sum(1 for line in body if line[:1] in (" ", "+")), new, hunks[at])

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

    def test_fr29_host_folders_can_be_mounted_with_9p_over_a_socket(self):
        config = CONFIG.read_text()
        self.assertIn("CONFIG_NET_9P=y", config)
        # trans=fd: the guest's daemon hands the kernel a Hyper-V socket to speak 9P on.
        self.assertIn("CONFIG_NET_9P_FD=y", config)
        self.assertIn("CONFIG_9P_FS=y", config)

    def test_fr28_the_synthetic_network_adapter_and_virt_wifi_are_built_in(self):
        # The host attaches a Hyper-V synthetic adapter; hv_netvsc drives it, and virt_wifi
        # shows it to Android as Wi-Fi. Both architectures need them, and the arm64 fragment
        # names hv_netvsc as well.
        config = CONFIG.read_text().splitlines()
        for option in ["CONFIG_HYPERV_NET=y", "CONFIG_WIRELESS=y", "CONFIG_CFG80211=y",
                       "CONFIG_WLAN=y", "CONFIG_VIRT_WIFI=y"]:
            self.assertIn(option, config)
        self.assertIn("CONFIG_HYPERV_NET=y", ARM64_CONFIG.read_text().splitlines())

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

    def test_nfr2_freed_pages_are_reported_to_the_host_without_a_delay(self):
        # Upstream waits two seconds before each reporting pass and again after each pass that
        # used up its budget, so a large free reached the host over many seconds. The patch
        # makes every pass start at once, for Hyper-V's and virtio's balloon alike.
        for path in [CONFIG, VIRTIO_CONFIG]:
            self.assertIn("CONFIG_PAGE_REPORTING=y", path.read_text())
        patch = (PATCHES / "page-reporting-no-delay.patch").read_text()
        self.assertIn("+++ b/mm/page_reporting.c", patch)
        self.assertIn("-#define PAGE_REPORTING_DELAY	(2 * HZ)", patch)
        self.assertIn("+#define PAGE_REPORTING_DELAY	0", patch)

    def test_nfr2_every_free_page_can_be_reported_to_the_host_again(self):
        # A host that faults guest memory in clusters takes reported free neighbours back in
        # with a page the guest touches again; the guest marks them reported and never offers
        # them again. The parameter empties the per-CPU lists, forgets the marks and starts a
        # pass. It applies after the patch that removes the reporting delay.
        patch = (PATCHES / "page-reporting-rereport.patch").read_text()
        self.assertIn("+++ b/mm/page_reporting.c", patch)
        self.assertIn("+module_param_cb(rereport, &page_reporting_rereport_ops, NULL, 0200);", patch)
        self.assertIn("+\t\tdrain_all_pages(NULL);", patch)
        self.assertIn("+\t\t\t\t__ClearPageReported(page);", patch)
        self.assertIn("+\t\t__page_reporting_request(prdev);", patch)
        names = sorted(p.name for p in PATCHES.glob("*.patch"))
        self.assertLess(names.index("page-reporting-no-delay.patch"),
                        names.index("page-reporting-rereport.patch"))

    def test_nfr2_the_guest_says_how_much_free_memory_the_host_has_back(self):
        # Read-only parameters for the agent's memory report: the free memory marked reported
        # now (counted from the free lists when read) and the memory reported since boot. A
        # write is refused rather than reaching a missing set operation.
        patch = (PATCHES / "page-reporting-stats.patch").read_text()
        self.assertIn("+++ b/mm/page_reporting.c", patch)
        self.assertIn("+module_param_cb(reported_kb, &page_reporting_reported_ops, NULL, 0444);", patch)
        self.assertIn("+module_param_cb(reported_total_kb, &page_reporting_reported_total_ops, NULL, 0444);",
                      patch)
        self.assertIn("+\t\tatomic_long_add(1UL << order, &page_reporting_reported_pages);", patch)
        self.assertIn("+\treturn -EPERM;", patch)
        names = sorted(p.name for p in PATCHES.glob("*.patch"))
        self.assertLess(names.index("page-reporting-rereport.patch"),
                        names.index("page-reporting-stats.patch"))

    def test_nfr2_free_pages_the_host_backs_again_with_a_neighbour_are_reported_again(self):
        # Hyper-V faults guest memory in 64 KiB clusters. When the allocator hands out a reported
        # block smaller than the cluster the host names (page_reporting.fault_cluster_order), the
        # free blocks in the rest of the cluster lose their reported mark and the next pass
        # reports them again. Off (0) unless the host sets it.
        patch = (PATCHES / "page-reporting-taken-clusters.patch").read_text()
        for path in ["mm/page_alloc.c", "mm/page_reporting.c", "mm/page_reporting.h"]:
            self.assertIn(f"+++ b/{path}", patch)
        self.assertIn("+\tbool reported = page_reported(page);", patch)
        self.assertIn("+\tif (reported)\n+\t\tpage_reporting_taken(zone, page, high);", patch)
        self.assertIn("+\tif (order < page_reporting_fault_cluster_order)", patch)
        self.assertIn("+unsigned int page_reporting_fault_cluster_order __read_mostly;", patch)
        self.assertIn("+module_param_cb(fault_cluster_order, &page_reporting_fault_cluster_ops,", patch)
        self.assertIn("+\t\t\t__ClearPageReported(p);", patch)
        self.assertIn("+module_param_cb(refaulted_total_kb, &page_reporting_refaulted_total_ops, NULL, 0444);",
                      patch)
        names = sorted(p.name for p in PATCHES.glob("*.patch"))
        self.assertLess(names.index("page-reporting-stats.patch"),
                        names.index("page-reporting-taken-clusters.patch"))

    def test_nfr1_cold_zram_pages_can_be_written_to_a_backing_file_by_age(self):
        # Writeback by age needs the access time; compressing a page again with zstd must not
        # reset it, or every recompressed page would look unused since boot.
        config = CONFIG.read_text()
        for option in ["CONFIG_ZRAM_WRITEBACK=y", "CONFIG_ZRAM_TRACK_ENTRY_ACTIME=y",
                       "CONFIG_ZRAM_MULTI_COMP=y", "CONFIG_BLK_DEV_LOOP=y"]:
            self.assertIn(option, config)
        patch = (PATCHES / "zram-recompress-keeps-access-time.patch").read_text()
        self.assertIn("+++ b/drivers/block/zram/zram_drv.c", patch)
        self.assertIn("+\tac_time = zram->table[index].ac_time;", patch)
        self.assertIn("+\tzram->table[index].ac_time = ac_time;", patch)
        keep = patch.index("+\tac_time = zram->table[index].ac_time;")
        restore = patch.index("+\tzram->table[index].ac_time = ac_time;")
        free = patch.index(" \tzram_free_page(zram, index);")
        self.assertLess(keep, free)
        self.assertLess(free, restore)

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

    def test_fr21_the_kernel_keeps_its_last_log_in_pstore_on_a_disk(self):
        # A panic ends the VM, so a RAM backend keeps nothing; pstore/blk writes to the small
        # disk the host attaches, found by its GPT partition UUID.
        config = CONFIG.read_text()
        for option in [
            "CONFIG_PSTORE=y",
            "CONFIG_PSTORE_CONSOLE=y",
            "CONFIG_PSTORE_PMSG=y",
            "CONFIG_PSTORE_BLK=y",
            "CONFIG_EFI_PARTITION=y",
            "# CONFIG_PSTORE_RAM is not set",
        ]:
            self.assertIn(option, config)
        self.assertNotIn("CONFIG_PSTORE_RAM=y", config)

    def test_fr21_pstore_blk_waits_for_its_disk(self):
        # Hyper-V disks probe asynchronously; without the wait pstore/blk looks for its disk
        # before it exists and never registers.
        patch = (PATCHES / "pstore-blk-wait-for-disk.patch").read_text()
        self.assertIn("fs/pstore/blk.c", patch)
        self.assertIn("+\twait_for_device_probe();", patch)
    def test_fr1_vmbus_root_device_is_platform_device_patch(self):
        patch = (PATCHES / "vmbus-platform-device.patch").read_text()
        self.assertIn("drivers/hv/vmbus_drv.c", patch)
        self.assertIn("vmbus_root_device = &pdev->dev;", patch)
        self.assertIn('device_rename(&pdev->dev, "vmbus")', patch)
        self.assertIn("device_move(&pdev->dev, &platform_bus, DPM_ORDER_NONE)", patch)

    def test_fr1_command_line_partitions_find_the_disk_by_its_gpt_guid(self):
        # Hyper-V names disks sda, sdb, ... in probe order, which changes when the host
        # attaches another disk. The image names the system disk by the GUID in its GPT.
        patch = (PATCHES / "cmdline-partition-uuid.patch").read_text()
        self.assertIn("block/partitions/cmdline.c", patch)
        self.assertIn("+\tchar name[UUID_STRING_LEN + 1];", patch)
        self.assertIn('+\tif (!memcmp(header, "EFI PART", 8))', patch)
        self.assertIn('"%pUl", header + 56', patch)
        self.assertIn("+\t\t\tparts = cmdline_parts_find(bdev_parts, guid);", patch)
        self.assertIn("queue_logical_block_size(state->disk->queue) >> 9", patch)

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

    def test_fr27_the_arm64_kernel_keeps_what_hyper_v_on_arm64_boots_and_returns_memory_with(self):
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

    def test_fr2_the_kernel_boots_under_kvm_with_virtio_devices(self):
        # The Linux host runs the same kernel under crosvm: its disks, network adapter, vsock,
        # balloon and console are virtio, and the host stops the guest with SysRq on the
        # virtio console (hvc0). Without them the guest finds no disk and never reaches the host.
        config = VIRTIO_CONFIG.read_text()
        for option in [
            "CONFIG_VIRTIO_PCI=y",
            "CONFIG_VIRTIO_BLK=y",
            "CONFIG_VIRTIO_NET=y",
            "CONFIG_VIRTIO_VSOCKETS=y",
            "CONFIG_VIRTIO_BALLOON=y",
            "CONFIG_VIRTIO_CONSOLE=y",
            "CONFIG_HVC_DRIVER=y",
            "CONFIG_MAGIC_SYSRQ=y",
        ]:
            self.assertIn(option, config)
        self.assertIn("CONFIG_PCI_HOST_GENERIC=y", ARM64_CONFIG.read_text())

    def test_nfr2_hotplugged_memory_holds_its_own_page_structs(self):
        # The Linux host boots the VM small and plugs memory in with virtio-mem. Without
        # hot-plug and hot-remove the guest never sees the memory or cannot give it back;
        # without memmap_on_memory the page structs of plugged memory come out of the small
        # boot memory; without auto-online plugged memory stays offline and unused.
        lines = fragment_lines(VIRTIO_CONFIG)
        for option in [
            "CONFIG_MEMORY_HOTPLUG=y",
            "CONFIG_MEMORY_HOTREMOVE=y",
            "CONFIG_MHP_MEMMAP_ON_MEMORY=y",
            "CONFIG_MHP_DEFAULT_ONLINE_TYPE_ONLINE_AUTO=y",
            "CONFIG_VIRTIO_MEM=y",
        ]:
            self.assertIn(option, lines)

    def test_fr27_the_arm64_kernel_uses_4k_pages(self):
        # The host's free page reporting order counts 4 KiB pages (5 is 128 KiB), and
        # Android's arm64 userspace and apps are built for 4 KiB pages.
        self.assertIn("CONFIG_ARM64_4K_PAGES=y", ARM64_CONFIG.read_text())

    def test_fr27_the_build_makes_an_arm64_image_from_the_wsl_arm64_configuration(self):
        script = BUILD.read_text()
        self.assertIn("Microsoft/config-wsl-arm64", script)
        self.assertIn("android-arm64.config", script)
        self.assertIn("arch/arm64/boot/Image", script)
        self.assertIn("aarch64-linux-gnu-", script)
        # The x86_64 build stays as it was.
        self.assertIn("Microsoft/config-wsl\"", script)
        self.assertIn("arch/x86/boot/bzImage", script)

    def test_fr27_the_arm64_fragment_does_not_repeat_or_contradict_the_common_one(self):
        common = {line.split("=")[0].removeprefix("# ").split(" ")[0] for line in fragment_lines(CONFIG)}
        for line in fragment_lines(ARM64_CONFIG):
            name = line.split("=")[0].removeprefix("# ").split(" ")[0]
            if name in common:
                # Allowed only when it says the same thing.
                self.assertIn(line, CONFIG.read_text().splitlines(), name)

    def test_fr2_the_kernel_drives_the_virtio_devices_of_the_macos_vm(self):
        # Apple's Virtualization framework gives the VM virtio devices on PCI: the disks,
        # the network adapter, the console, the sockets, the balloon and entropy. The VM
        # loads no modules, so each driver is built in, for both architectures.
        config = VIRTIO_CONFIG.read_text().splitlines()
        for option in ["CONFIG_VIRTIO_PCI=y", "CONFIG_VIRTIO_BLK=y", "CONFIG_VIRTIO_NET=y",
                       "CONFIG_VIRTIO_CONSOLE=y", "CONFIG_VIRTIO_VSOCKETS=y",
                       "CONFIG_VIRTIO_BALLOON=y", "CONFIG_HW_RANDOM_VIRTIO=y"]:
            self.assertIn(option, config)
        script = BUILD.read_text()
        self.assertEqual(script.count('"$kernel_dir/virtio.config"'), 2)

    def test_fr2_the_arm64_kernel_finds_the_devices_of_an_apple_silicon_vm(self):
        # The framework describes an arm64 VM with a device tree and a generic ECAM PCI
        # bridge, and keeps the time in a PL031 clock.
        config = ARM64_CONFIG.read_text().splitlines()
        for option in ["CONFIG_OF=y", "CONFIG_PCI_HOST_GENERIC=y", "CONFIG_RTC_DRV_PL031=y"]:
            self.assertIn(option, config)

    def test_pr1_hyper_v_sockets_register_before_virtio_sockets(self):
        # Only one guest-to-host transport registers; built in, they register in link order.
        # Hyper-V's steps aside without VMBus, so it must come first for one kernel to reach
        # the host on Hyper-V and on Apple's framework.
        patch = (PATCHES / "vsock-hyperv-first.patch").read_text().splitlines()
        added = [line for line in patch if line.startswith("+obj-")]
        removed = [line for line in patch if line.startswith("-obj-")]
        self.assertEqual(added, ["+obj-$(CONFIG_HYPERV_VSOCKETS) += hv_sock.o"])
        self.assertEqual(removed, ["-obj-$(CONFIG_HYPERV_VSOCKETS) += hv_sock.o"])
        body = [line[1:] for line in patch if line[:1] in (" ", "+") and "obj-" in line]
        self.assertLess(body.index("obj-$(CONFIG_HYPERV_VSOCKETS) += hv_sock.o"),
                        body.index("obj-$(CONFIG_VIRTIO_VSOCKETS) += vmw_vsock_virtio_transport.o"))

    def test_fr27_no_patch_touches_x86_only_code(self):
        # The same patches apply to both architectures' builds.
        for patch in PATCHES.glob("*.patch"):
            for line in patch.read_text().splitlines():
                if line.startswith(("--- a/", "+++ b/")):
                    self.assertNotIn("arch/x86", line, patch.name)

    def test_fr27_a_merged_configuration_kept_every_fragment_line(self):
        # build.sh --config-only leaves the merged .config; CI points this at each one.
        merged = os.environ.get("DSA_KERNEL_MERGED_CONFIG")
        if not merged:
            self.skipTest("DSA_KERNEL_MERGED_CONFIG names no merged configuration")
        lines = Path(merged).read_text().splitlines()
        # The architecture comes from the merged file's own header ("# Linux/arm64 ..."), so
        # a run that forgets to name it cannot check an arm64 configuration against the
        # x86_64 fragments, or skip the arm64 fragment.
        built = next((line[len("# Linux/"):].split()[0] for line in lines
                      if line.startswith("# Linux/")), None)
        # The header names the kernel's ARCH as make was given it: "x86" or "x86_64" for
        # the x86_64 kernel, "arm64" for arm64. Map it to this repository's names.
        known = {"x86": "x86_64", "x86_64": "x86_64", "arm64": "arm64"}
        self.assertIn(built, known, f"{merged} names no known architecture")
        arch = known[built]
        named = os.environ.get("DSA_KERNEL_ARCH")
        if named:
            self.assertEqual(arch, named, f"{merged} is a {built} configuration, not {named}")
        fragments = [CONFIG] + ([ARM64_CONFIG] if arch == "arm64" else []) + [VIRTIO_CONFIG]
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
