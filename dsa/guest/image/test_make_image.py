"""Regression tests for the guest image layout helpers."""

import base64
import gzip
import importlib.util
import json
import re
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("make_image", Path(__file__).with_name("make-image.py"))
make_image = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(make_image)


class ImageLayoutTests(unittest.TestCase):
    def test_fr1_selects_vendor_apexes_without_the_cuttlefish_launcher(self):
        cmdline = make_image.kernel_cmdline("blkdevparts=sda:1@0(misc)")
        self.assertIn("androidboot.boot_devices=vmbus", cmdline)
        self.assertNotIn("androidboot.boot_part_uuid", cmdline)
        self.assertIn(
            "androidboot.vendor.apex.com.android.hardware.gatekeeper=com.android.hardware.gatekeeper.nonsecure",
            cmdline,
        )
        self.assertIn(
            "androidboot.vendor.apex.com.android.hardware.keymint=com.android.hardware.keymint.rust_nonsecure",
            cmdline,
        )
        self.assertIn(
            "androidboot.vendor.apex.com.android.hardware.graphics.composer=com.android.hardware.graphics.composer.ranchu",
            cmdline,
        )
        self.assertIn(
            "androidboot.vendor.apex.com.google.emulated.camera.provider.hal=com.google.emulated.camera.provider.hal",
            cmdline,
        )

    def test_fr1_avb_footer_partitions_match_the_signed_aosp_layout(self):
        sizes = dict(make_image.SMALL_PARTITIONS)
        self.assertEqual(sizes["boot_a"], 64 << 20)
        self.assertEqual(sizes["init_boot_a"], 8 << 20)
        self.assertEqual(sizes["vendor_boot_a"], 64 << 20)

    def test_fr1_hyperv_partition_names_cover_the_gpt_ranges(self):
        layout = {
            "misc": (1 << 20, 1 << 20),
            "vbmeta_a": (66 << 20, 1 << 20),
            "super": (100 << 20, 1536 << 20),
        }
        self.assertEqual(
            make_image.command_line_partitions(layout, {"misc": make_image.MISC_UUID}),
            "blkdevparts=sda:1048576@1048576(misc:811d3bbb-a7ff-437a-857f-f54af996b190),1048576@69206016(vbmeta_a),"
            "1610612736@104857600(super)",
        )

    def test_fr1_verified_hash_partition_needs_a_first_stage_entry(self):
        found = [("chain", "vbmeta_system"), ("chain", "boot"), ("chain", "init_boot"), ("hash", "vendor_boot")]
        fstab = """\\
/dev/block/by-name/boot /boot emmc defaults recoveryonly,slotselect,first_stage_mount,avb=boot
/dev/block/by-name/init_boot /init_boot emmc defaults recoveryonly,slotselect,first_stage_mount,avb=init_boot
"""
        self.assertEqual(make_image.unlisted_verified_boot_partitions(found, fstab), ["vbmeta_system", "vendor_boot"])

    def test_fr1_windows_backed_wsl_uses_a_sparse_raw_disk(self):
        self.assertTrue(make_image.is_wsl_windows_mount(("/", "mnt", "c", "dsa", "disk.raw")))
        self.assertFalse(make_image.is_wsl_windows_mount(("/", "home", "dsa", "disk.raw")))

    def test_fr1_windows_sparse_disk_materializes_the_backup_gpt(self):
        with patch.object(make_image, "use_windows_sparse_file", return_value=True), \
             patch.object(make_image, "windows_path", return_value=r"C:\\dsa\\disk.raw"), \
             patch.object(make_image, "run") as run:
            make_image.materialize_backup_gpt(Path("disk.raw"))
        self.assertEqual(run.call_args.args[:3], ("powershell.exe", "-NoProfile", "-Command"))
        self.assertIn("16896", run.call_args.args[3])

    def test_fr1_vendor_boot_is_signed_after_its_ramdisk_changes(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "target-files.zip"
            with zipfile.ZipFile(target, "w") as archive:
                archive.writestr("META/apkcerts.txt", "")
                archive.writestr("META/apexkeys.txt", "")
            args = make_image.key_arguments(target, "keys")
        self.assertIn("--avb_extra_custom_image_key", args)
        self.assertIn("vendor_boot=keys/avb.pem", args)

    def test_fr1_userdata_keeps_fbe_without_metadata_encryption(self):
        fstab = Path(__file__).with_name("fstab.dsa").read_text()
        userdata = next(line for line in fstab.splitlines() if " /data " in line)
        self.assertIn("fileencryption=aes-256-xts:aes-256-hctr2", userdata)
        self.assertNotIn("keydirectory=", userdata)

    def test_pr3_graphics_library_replaces_the_vendor_gfxstream_payload(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            replacement = root / "libOpenglSystemCommon.so"
            replacement.write_bytes(b"hyper-v-gfxstream")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("VENDOR/lib64/libOpenglSystemCommon.so", b"cuttlefish-gfxstream")
                archive.writestr("SYSTEM/build.prop", b"ro.debuggable=1\n")
            modified = root / "modified.zip"
            make_image.modify(source, modified, graphics_library=replacement)
            with zipfile.ZipFile(modified) as archive:
                self.assertEqual(
                    archive.read("VENDOR/lib64/libOpenglSystemCommon.so"),
                    b"hyper-v-gfxstream",
                )

    def test_pr3_graphics_library_must_have_a_vendor_gfxstream_payload_to_replace(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            replacement = root / "libOpenglSystemCommon.so"
            replacement.write_bytes(b"hyper-v-gfxstream")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/build.prop", b"ro.debuggable=1\n")
            with self.assertRaisesRegex(ValueError, "does not contain"):
                make_image.modify(source, root / "modified.zip", graphics_library=replacement)

    def test_pr3_graphics_vsock_sepolicy_fragment_is_applied(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            plat_cil = (
                "(typeattribute appdomain)\n"
                "(type surfaceflinger)\n"
                "(typeattribute hal_graphics_allocator_server)\n"
                "(typeattribute hal_graphics_composer_server)\n"
                "(type bootanim)\n"
                "(neverallow appdomain self (vsock_socket (create connect bind listen accept)))\n"
            )
            mapping_cil = "(typeattributeset appdomain (untrusted_app))\n"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", plat_cil)
                archive.writestr("SYSTEM/etc/selinux/mapping/35.0.cil", mapping_cil)
                archive.writestr("VENDOR/etc/selinux/plat_sepolicy_vers.txt", "35.0\n")
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy_and_mapping.sha256", "old-hash\n")
                archive.writestr("VENDOR/etc/selinux/precompiled_sepolicy.plat_sepolicy_and_mapping.sha256", "old-hash\n")
                archive.writestr("VENDOR/etc/selinux/precompiled_sepolicy", b"old-precompiled-policy")
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=1\n")
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                new_cil = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertIn(
                    "(allow appdomain self (vsock_socket (create connect read write getattr setopt getopt shutdown)))",
                    new_cil,
                )
                self.assertIn(
                    "(allow surfaceflinger self (vsock_socket (create connect read write getattr setopt getopt shutdown)))",
                    new_cil,
                )
                # Verify neverallow was narrowed
                neverallow_rule = next(l for l in new_cil.splitlines() if l.startswith("(neverallow"))
                self.assertEqual(neverallow_rule, "(neverallow appdomain self (vsock_socket (bind listen accept)))")

                # Verify SHA-256 hash was updated
                expected_hash = make_image.compute_plat_sepolicy_sha256(
                    new_cil.encode(), mapping_cil.encode()
                )
                self.assertEqual(
                    archive.read("SYSTEM/etc/selinux/plat_sepolicy_and_mapping.sha256").decode(),
                    expected_hash,
                )
                self.assertEqual(
                    archive.read("VENDOR/etc/selinux/precompiled_sepolicy.plat_sepolicy_and_mapping.sha256").decode(),
                    expected_hash,
                )
                # When otatools is not supplied, precompiled_sepolicy is removed so the device compiles at boot
                self.assertNotIn("VENDOR/etc/selinux/precompiled_sepolicy", archive.namelist())

    def test_pr3_the_graphics_allocator_can_open_the_drm_device(self):
        plat_cil = "(typeattribute hal_graphics_allocator_server)\n(type graphics_device)\n"
        patched = make_image.patch_plat_sepolicy(plat_cil, make_image.SEPOLICY_FRAGMENT.read_text())
        rule = next(
            (l for l in patched.splitlines() if l.startswith("(allow hal_graphics_allocator_server graphics_device (chr_file")),
            None,
        )
        self.assertIsNotNone(rule, "the allocator has no rule for the DRM device")
        for perm in ("open", "read", "write", "ioctl"):
            self.assertIn(perm, rule)

    def test_pr3_neverallow_is_narrowed_only_for_vsock_socket(self):
        cil = (
            "(neverallow all_untrusted_apps base_typeattr_1 (vsock_socket (ioctl read write create getattr "
            "setattr lock relabelfrom relabelto append map bind connect listen accept getopt setopt shutdown "
            "recvfrom sendto name_bind)))\n"
        )
        narrowed = make_image.narrow_vsock_neverallows(cil)
        # Sockets cannot be bound or listened on, so those remain forbidden
        self.assertIn("bind", narrowed)
        self.assertIn("listen", narrowed)
        self.assertIn("accept", narrowed)
        self.assertIn("ioctl", narrowed)
        # Client operations are no longer forbidden
        for perm in ["create", "connect", "setopt", "shutdown", "read", "write", "getattr", "getopt"]:
            self.assertNotIn(f" {perm} ", narrowed)
            self.assertFalse(narrowed.endswith(f" {perm})))\n"))
        self.assertTrue(narrowed.startswith("(neverallow all_untrusted_apps base_typeattr_1 (vsock_socket ("))

    def test_pr3_policy_outside_vsock_socket_is_unchanged(self):
        cil = (
            "(typeattribute appdomain)\n"
            "(typeattribute all_untrusted_apps)\n"
            "(allow appdomain app_data_file (file (read write)))\n"
            "(neverallow all_untrusted_apps file_type (file (mounton)))\n"
            "(neverallow domain file_type (blk_file (read write)))\n"
            "(neverallow all_untrusted_apps base_typeattr_1 (vsock_socket (create connect bind)))\n"
        )
        narrowed = make_image.narrow_vsock_neverallows(cil)
        self.assertIn("(typeattribute appdomain)\n", narrowed)
        self.assertIn("(typeattribute all_untrusted_apps)\n", narrowed)
        self.assertIn("(allow appdomain app_data_file (file (read write)))\n", narrowed)
        self.assertIn("(neverallow all_untrusted_apps file_type (file (mounton)))\n", narrowed)
        self.assertIn("(neverallow domain file_type (blk_file (read write)))\n", narrowed)
        self.assertIn("(neverallow all_untrusted_apps base_typeattr_1 (vsock_socket (bind)))\n", narrowed)


    def test_pr3_precompiled_sepolicy_is_recompiled_when_secilc_is_present(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            otatools = root / "otatools"
            bin_dir = otatools / "bin"
            bin_dir.mkdir(parents=True)
            secilc = bin_dir / "secilc"
            # Fake secilc executable script that writes a binary policy
            secilc.write_text("#!/bin/sh\nexit 0\n")
            secilc.chmod(0o755)

            source = root / "target-files.zip"
            plat_cil = "(typeattribute appdomain)\n"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", plat_cil)
                archive.writestr("SYSTEM/etc/selinux/mapping/35.0.cil", "")
                archive.writestr("VENDOR/etc/selinux/plat_sepolicy_vers.txt", "35.0\n")
                archive.writestr("VENDOR/etc/selinux/precompiled_sepolicy", b"old-policy")
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=1\n")

            modified = root / "modified.zip"
            with patch("subprocess.run") as mock_run:
                def fake_run(cmd, *a, **kw):
                    # Write dummy output to -o argument
                    out_idx = cmd.index("-o") + 1
                    Path(cmd[out_idx]).write_bytes(b"recompiled-sepolicy-binary")
                    return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")
                mock_run.side_effect = fake_run
                make_image.modify(source, modified, otatools=otatools)

            with zipfile.ZipFile(modified) as archive:
                self.assertIn("VENDOR/etc/selinux/precompiled_sepolicy", archive.namelist())
                self.assertEqual(
                    archive.read("VENDOR/etc/selinux/precompiled_sepolicy"),
                    b"recompiled-sepolicy-binary",
                )

    def test_pr3_precompiled_sepolicy_is_compiled_from_the_files_init_would_use(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            secilc = root / "otatools/bin/secilc"
            secilc.parent.mkdir(parents=True)
            secilc.write_text("#!/bin/sh\nexit 0\n")
            secilc.chmod(0o755)

            # The partitions each carry a mapping file named after the same version, and the
            # build carries mapping and genfs files for versions this vendor does not use.
            cil = {
                "SYSTEM/etc/selinux/plat_sepolicy.cil": "plat",
                "SYSTEM/etc/selinux/mapping/202504.cil": "plat mapping",
                "SYSTEM/etc/selinux/mapping/202504.compat.cil": "plat compat",
                "SYSTEM/etc/selinux/mapping/202404.cil": "old plat mapping",
                "SYSTEM/etc/selinux/plat_sepolicy_genfs_202504.cil": "genfs 202504",
                "SYSTEM/etc/selinux/plat_sepolicy_genfs_202604.cil": "genfs 202604",
                "SYSTEM_EXT/etc/selinux/system_ext_sepolicy.cil": "system_ext",
                "SYSTEM_EXT/etc/selinux/mapping/202504.cil": "system_ext mapping",
                "PRODUCT/etc/selinux/product_sepolicy.cil": "product",
                "PRODUCT/etc/selinux/mapping/202504.cil": "product mapping",
                "VENDOR/etc/selinux/plat_pub_versioned.cil": "pub versioned",
                "VENDOR/etc/selinux/vendor_sepolicy.cil": "vendor",
                "ODM/etc/selinux/odm_sepolicy.cil": "odm",
            }
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                for name, text in cil.items():
                    archive.writestr(name, text)
                archive.writestr("VENDOR/etc/selinux/plat_sepolicy_vers.txt", "202504\n")
                archive.writestr("VENDOR/etc/selinux/genfs_labels_version.txt", "202504\n")
                archive.writestr("ODM/etc/selinux/precompiled_sepolicy", b"old-policy")

            compiled = []
            with patch("subprocess.run") as mock_run:
                def fake_run(cmd, *a, **kw):
                    out = cmd.index("-o")
                    inputs = [arg for arg in cmd[1:] if arg.endswith(".cil")]
                    compiled.extend(Path(arg).read_text() for arg in inputs)
                    Path(cmd[out + 1]).write_bytes(b"policy")
                    return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")
                mock_run.side_effect = fake_run
                make_image.modify(source, root / "modified.zip", otatools=root / "otatools")

            self.assertTrue(compiled[0].startswith("plat"))
            self.assertEqual(
                compiled[1:],
                [
                    "plat mapping",
                    "plat compat",
                    "system_ext",
                    "system_ext mapping",
                    "product",
                    "product mapping",
                    "pub versioned",
                    "vendor",
                    "odm",
                    "genfs 202504",
                ],
            )


    def test_fr1_two_disk_layout_separates_system_and_data_partitions(self):
        system_partitions = [name for name, _ in make_image.SYSTEM_SMALL_PARTITIONS]
        self.assertIn("vbmeta_a", system_partitions)
        self.assertIn("boot_a", system_partitions)
        self.assertNotIn("metadata", system_partitions)
        self.assertNotIn("userdata", system_partitions)

        data_partitions = [name for name, _ in make_image.DATA_PARTITIONS]
        self.assertEqual(data_partitions, ["misc", "metadata", "userdata"])

    def test_fr1_misc_is_on_the_writable_data_disk(self):
        # The boot control HAL writes the boot state into misc and aborts when it cannot,
        # and the system disk is attached read-only.
        system_partitions = [name for name, _ in make_image.SYSTEM_SMALL_PARTITIONS]
        self.assertNotIn("misc", system_partitions)
        self.assertIn("misc", [name for name, _ in make_image.DATA_PARTITIONS])


    def test_pr3_kernel_cmdline_selects_guest_graphics_stack(self):
        cmdline = make_image.kernel_cmdline("blkdevparts=sda:1@0(misc)")
        self.assertIn("androidboot.hardware.gltransport=hvsocket", cmdline)
        self.assertIn("androidboot.hardware.egl=angle", cmdline)
        self.assertIn("androidboot.hardware.vulkan=ranchu", cmdline)
        self.assertIn("androidboot.hardware.gralloc=minigbm", cmdline)
        self.assertIn("androidboot.cpuvulkan.version=0", cmdline)

    def test_fr1_android_shares_memory_through_memfd(self):
        # libcutils falls back to /dev/ashmem unless sys.use_memfd is true, and the guest
        # kernel has no ashmem driver.
        props = make_image.edit_props("ro.debuggable=1\n")
        self.assertIn("sys.use_memfd=true\n", props)

    def test_pr3_composer_starts_without_a_display_device(self):
        # Cuttlefish's composer otherwise looks for a virtio-gpu DRM card or a goldfish
        # display, finds neither under Hyper-V, and surfaceflinger aborts.
        cmdline = make_image.kernel_cmdline("blkdevparts=sda:1@0(misc)")
        self.assertIn("androidboot.hardware.hwcomposer.mode=noop", cmdline)
        self.assertIn("androidboot.hardware.hwcomposer.display_finder_mode=noop", cmdline)

    def test_fr1_lights_hal_listens_on_an_unprivileged_vsock_port(self):
        # Without a port the Cuttlefish lights HAL binds vsock port 0, which needs
        # CAP_NET_BIND_SERVICE, and aborts; system_server waits for it and never boots.
        cmdline = make_image.kernel_cmdline("blkdevparts=sda:1@0(misc)")
        port = int(cmdline.split("androidboot.vsock_lights_port=", 1)[1].split()[0])
        self.assertGreaterEqual(port, 1024)
        self.assertNotIn(port, {5000, 5566, 5567})

    def test_fr1_hals_whose_devices_hyperv_lacks_are_not_in_the_image(self):
        # The oemlock, UWB, Bluetooth and Thread HALs need Cuttlefish's virtio consoles or
        # radios and abort in a loop; system_server waits for the declared oemlock HAL forever.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            removed = [
                "com.google.cf.oemlock", "com.android.hardware.uwb", "com.google.cf.bt",
                "com.android.hardware.threadnetwork", "com.google.cf.nfc", "com.google.cf.rild",
            ]
            with zipfile.ZipFile(source, "w") as archive:
                for apex in removed + ["com.android.hardware.boot"]:
                    archive.writestr(f"VENDOR/apex/{apex}.apex", b"apex")
                archive.writestr("META/apexkeys.txt", "".join(
                    f'name="{apex}.apex" public_key="k.avbpubkey" private_key="k.pem" container_certificate="c.x509.pem" container_private_key="c.pk8" partition="vendor"\n'
                    for apex in removed + ["com.android.hardware.boot"]
                ))

            modified = root / "modified.zip"
            make_image.modify(source, modified)

            with zipfile.ZipFile(modified) as archive:
                names = archive.namelist()
                keys = archive.read("META/apexkeys.txt").decode()
            for apex in removed:
                self.assertNotIn(f"VENDOR/apex/{apex}.apex", names)
                self.assertNotIn(f'name="{apex}.apex"', keys)
            self.assertIn("VENDOR/apex/com.android.hardware.boot.apex", names)
            self.assertIn('name="com.android.hardware.boot.apex"', keys)

    def test_fr1_bluetooth_is_not_advertised_without_a_controller(self):
        # With the feature declared, system_server starts the Bluetooth stack, which aborts
        # in a loop because no HCI HAL exists.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/etc/permissions 0 2000 755 capabilities=0x0\n")
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                xml = archive.read("VENDOR/etc/permissions/dsa-unavailable-features.xml").decode()
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
            for feature in ["android.hardware.bluetooth", "android.hardware.bluetooth_le"]:
                self.assertIn(f'<unavailable-feature name="{feature}" />', xml)
            self.assertIn("vendor/etc/permissions/dsa-unavailable-features.xml 0 0 644 capabilities=0x0", fs_config)

    def test_fr4_nfc_and_telephony_are_not_advertised_without_the_hardware(self):
        # Declared, the NFC service and the phone process start, fail to reach their HALs and
        # time out every few seconds; each ANR dumps the stacks of many processes and takes
        # the CPU that the apps need to draw.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/etc/permissions 0 2000 755 capabilities=0x0\n")
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                xml = archive.read("VENDOR/etc/permissions/dsa-unavailable-features.xml").decode()
            for feature in ["android.hardware.nfc", "android.hardware.nfc.any", "android.hardware.nfc.hce",
                            "android.hardware.telephony", "android.hardware.telephony.radio.access",
                            "android.hardware.telephony.subscription", "android.hardware.telephony.data",
                            "android.hardware.telephony.calling", "android.hardware.telephony.messaging",
                            "android.hardware.telephony.gsm", "android.hardware.telephony.ims"]:
                self.assertIn(f'<unavailable-feature name="{feature}" />', xml)

    def test_fr1_system_server_does_not_wait_for_a_factory_reset_protection_partition(self):
        # With ro.frp.pst set, system_server starts PersistentDataBlockService, which needs
        # an frp partition the DSA disks do not have, and dies at boot phase 500. Empty,
        # neither it nor OemLockService (without an oemlock HAL) starts.
        props = make_image.edit_props("ro.frp.pst=/dev/block/by-name/frp\n")
        self.assertIn("ro.frp.pst=\n", props)
        self.assertNotIn("by-name/frp", props)

    # The Wi-Fi init script as the pinned Android 16 and 17 builds ship it, comments trimmed.
    WIFI_RC = (
        "on post-fs-data\n"
        "   restorecon_recursive /data/misc/apexdata/com.android.wifi\n"
        "\n"
        "on property:sys.user.0.ce_available=true\n"
        "   restorecon_recursive /data/misc_ce/0/apexdata/com.android.wifi\n"
        "\n"
        "# Below are for kernel tracing related stuff.\n"
        "on fs\n"
        "   setprop sys.wifitracing.started 0\n"
        "\n"
        "on property:sys.boot_completed=1 && property:sys.wifitracing.started=0\n"
        "   # Create trace buffer, and set basic configuration.\n"
        "   mkdir /sys/kernel/tracing/instances/wifi 711\n"
        "   restorecon_recursive /sys/kernel/tracing/instances/wifi\n"
        "   write /sys/kernel/tracing/instances/wifi/tracing_on 0\n"
        "   write /sys/kernel/tracing/instances/wifi/buffer_size_kb 1\n"
        "   write /sys/kernel/tracing/instances/wifi/trace_options disable_on_free\n"
        "\n"
        "   write /sys/kernel/tracing/instances/wifi/events/cfg80211/rdev_connect/enable 1\n"
        "   write /sys/kernel/tracing/instances/wifi/events/net/filter name==${wifi.interface:-wlan0}\n"
        "   write /sys/kernel/tracing/instances/wifi/events/net/net_dev_queue/enable 1\n"
        "   write /sys/kernel/tracing/instances/wifi/events/net/netif_receive_skb/enable 1\n"
        "\n"
        "   chown system /sys/kernel/tracing/instances/wifi/tracing_on\n"
        "   chmod 600 /sys/kernel/tracing/instances/wifi/trace\n"
        "   setprop sys.wifitracing.started 1\n"
        "\n"
        "on property:sys.boot_completed=1 && property:wifi.interface=* && property:sys.wifitracing.started=1\n"
        "   # Override default value.\n"
        "   write /sys/kernel/tracing/instances/wifi/events/net/filter name==${wifi.interface}\n"
    )

    def test_fr3_boot_completed_sets_up_no_wifi_trace_instance(self):
        # On Hyper-V each write into tracefs takes up to two seconds, and lmkd attaches its
        # memory event programs to tracepoints behind them when boot completes, past its
        # watchdog's two seconds; the watchdog then killed the app the user had just opened.
        rc = make_image.edit_wifi_rc(self.WIFI_RC)
        self.assertNotIn("sys.wifitracing.started=0", rc)
        self.assertNotIn("instances/wifi/buffer_size_kb", rc)
        self.assertNotIn("instances/wifi/events/net/net_dev_queue/enable", rc)
        self.assertNotIn("mkdir /sys/kernel/tracing", rc)
        # The rest of the script stays: Wi-Fi's data migration and its other triggers.
        self.assertIn("on post-fs-data\n   restorecon_recursive /data/misc/apexdata/com.android.wifi\n", rc)
        self.assertIn("on property:sys.user.0.ce_available=true\n", rc)
        self.assertIn("on fs\n   setprop sys.wifitracing.started 0\n", rc)
        self.assertIn("property:sys.wifitracing.started=1\n", rc)

    def test_fr3_an_unrecognised_wifi_rc_is_not_passed_through(self):
        # A new build that set the trace instance up some other way would bring the stall
        # back unnoticed, so the image is not made until this edit is looked at again.
        with self.assertRaisesRegex(ValueError, "wifi.rc"):
            make_image.edit_wifi_rc("on property:sys.boot_completed=1\n"
                                    "   mkdir /sys/kernel/tracing/instances/wifi 711\n")

    def test_fr3_modify_removes_the_wifi_trace_setup_from_the_system_image(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/init/wifi.rc", self.WIFI_RC)
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                rc = archive.read("SYSTEM/etc/init/wifi.rc").decode()
            self.assertNotIn("sys.wifitracing.started=0", rc)
            self.assertIn("on post-fs-data\n", rc)

    def test_pr3_graphics_clients_can_open_the_drm_card_minigbm_uses(self):
        # Every process that maps a graphics buffer runs minigbm's mapper, which opens the
        # vkms card. Labelled graphics_device, as Cuttlefish labels its composer's cards,
        # apps and the boot animation are refused and cannot dequeue a single buffer.
        fc = make_image.update_vendor_file_contexts(
            "/dev/dri u:object_r:gpu_device:s0\n"
            "/dev/dri/card0  u:object_r:graphics_device:s0\n"
            "/dev/dri/card1  u:object_r:graphics_device:s0\n"
            "/dev/dri/renderD128  u:object_r:gpu_device:s0\n"
        )
        self.assertIn("/dev/dri/card0  u:object_r:gpu_device:s0", fc)
        self.assertIn("/dev/dri/card1  u:object_r:gpu_device:s0", fc)
        self.assertNotIn("graphics_device", fc)
        self.assertEqual(fc.count("/dev/dri/card1 "), 1)

    def test_pr3_edit_props_sets_graphics_properties(self):
        props = make_image.edit_props("ro.debuggable=1\n")
        self.assertIn("ro.hardware.egl=angle\n", props)
        self.assertIn("ro.hardware.vulkan=ranchu\n", props)
        self.assertIn("ro.hardware.gralloc=minigbm\n", props)
        self.assertIn("ro.cpuvulkan.version=0\n", props)

    def test_pr3_vulkan_loader_uses_the_vendor_driver_not_the_cuttlefish_apex(self):
        # With ro.vulkan.apex set, the loader takes Cuttlefish's driver from its APEX,
        # which reaches the host only through virtio-gpu or the goldfish pipe. The APEX's
        # own init script sets it "on init", so dropping it from build.prop is not enough:
        # build.prop, read first, claims the read-only property with no value.
        props = make_image.edit_props("ro.vulkan.apex=com.google.cf.vulkan\nro.debuggable=1\n")
        self.assertIn("ro.vulkan.apex=\n", props)
        self.assertNotIn("com.google.cf.vulkan", props)

    def test_pr3_modify_installs_vulkan_and_opengl_libraries_and_filesystem_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            gl_replacement = root / "libOpenglSystemCommon.so"
            gl_replacement.write_bytes(b"hyper-v-gl")
            vk_replacement = root / "vulkan.ranchu.so"
            vk_replacement.write_bytes(b"hyper-v-vk")

            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("VENDOR/lib64/libOpenglSystemCommon.so", b"cuttlefish-gl")
                archive.writestr("SYSTEM/build.prop", b"ro.debuggable=1\n")
                archive.writestr("VENDOR/build.prop", b"ro.hardware=cutf_cvm\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor(/.*)? u:object_r:vendor_file:s0\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/etc/fstab.dsa 0 0 644 capabilities=0x0\n")

            modified = root / "modified.zip"
            make_image.modify(source, modified, graphics_library=gl_replacement, vulkan_driver=vk_replacement)

            with zipfile.ZipFile(modified) as archive:
                names = archive.namelist()
                self.assertIn("VENDOR/lib64/libOpenglSystemCommon.so", names)
                self.assertIn("VENDOR/lib64/hw/vulkan.ranchu.so", names)
                self.assertEqual(archive.read("VENDOR/lib64/libOpenglSystemCommon.so"), b"hyper-v-gl")
                self.assertEqual(archive.read("VENDOR/lib64/hw/vulkan.ranchu.so"), b"hyper-v-vk")

                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/lib64/hw/vulkan.ranchu.so 0 0 644 capabilities=0x0", fs_config)
                self.assertIn("vendor/lib64/libOpenglSystemCommon.so 0 0 644 capabilities=0x0", fs_config)

                fc = archive.read("VENDOR/etc/selinux/vendor_file_contexts").decode()
                self.assertIn("vulkan\\.ranchu\\.so", fc)
                self.assertIn("same_process_hal_file", fc)
                self.assertIn("libOpenglSystemCommon\\.so", fc)


    def test_fr2_android_log_reaches_the_vm_console_log(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("VENDOR/etc/ueventd.rc", "# seriallogging\n/dev/hvc2 0660 system logd\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/etc/ueventd.rc 0 0 644 capabilities=0x0\n")

            modified = root / "modified.zip"
            make_image.modify(source, modified)

            with zipfile.ZipFile(modified) as archive:
                rc = archive.read("VENDOR/etc/init/dsa-console-logcat.rc").decode()
                self.assertIn("/system/bin/logcat -b all -v threadtime -f /dev/ttyS0", rc)
                self.assertIn("seclabel u:r:logpersist:s0", rc)
                self.assertIn("user logd", rc)
                ueventd = archive.read("VENDOR/etc/ueventd.rc").decode()
                self.assertTrue(ueventd.splitlines()[-1].split() == ["/dev/ttyS0", "0660", "system", "logd"])
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/etc/init/dsa-console-logcat.rc 0 0 644 capabilities=0x0", fs_config)

    def test_pr6_the_control_daemon_is_installed_in_its_own_domain(self):
        # The daemon reads the boot's secret from /proc/bootconfig, so it needs a domain
        # allowed to; no app domain gains that.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            daemon = root / "dsa-channeld"
            daemon.write_bytes(b"\x7fELF daemon")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n(typeattribute appdomain)\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
                archive.writestr("META/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")

            modified = root / "modified.zip"
            make_image.modify(source, modified, channeld=daemon)

            with zipfile.ZipFile(modified) as archive:
                self.assertEqual(archive.read("VENDOR/bin/dsa-channeld"), b"\x7fELF daemon")
                rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode()
                self.assertIn("service dsa_channeld /vendor/bin/dsa-channeld", rc)
                self.assertIn("stdio_to_kmsg", rc)
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/bin/dsa-channeld 0 2000 755 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/init/dsa-channeld.rc 0 0 644 capabilities=0x0", fs_config)
                for contexts in ["VENDOR/etc/selinux/vendor_file_contexts", "META/vendor_file_contexts"]:
                    self.assertIn(
                        "/vendor/bin/dsa-channeld u:object_r:dsa_channeld_exec:s0",
                        archive.read(contexts).decode(),
                    )
                policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertIn("(typetransition init dsa_channeld_exec process dsa_channeld)", policy)
                self.assertIn("(allow dsa_channeld proc_bootconfig (file (read open getattr)))", policy)
                # stdio_to_kmsg hands it /dev/kmsg_debug; denied, its messages are lost.
                self.assertIn("(allow dsa_channeld kmsg_debug_device (chr_file (write append getattr)))", policy)
                self.assertIn("(allow dsa_channeld self (vsock_socket (create connect", policy)
                self.assertNotRegex(policy, r"\(allow (appdomain|untrusted_app)\S* proc_bootconfig")

    def test_fr3_the_channel_service_is_declared_labelled_and_allowed(self):
        # The agent and the hwcomposer reach the daemon over binder. servicemanager refuses
        # a VINTF-stable service the vendor manifest does not declare, and checks every
        # registration and lookup against the label vendor_service_contexts gives it.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            daemon = root / "dsa-channeld"
            daemon.write_bytes(b"daemon")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr(
                    "VENDOR/etc/selinux/vendor_service_contexts",
                    "android.hardware.graphics.composer3.IComposer/default u:object_r:hal_graphics_composer_service:s0\n",
                )

            modified = root / "modified.zip"
            make_image.modify(source, modified, channeld=daemon)

            with zipfile.ZipFile(modified) as archive:
                manifest = archive.read("VENDOR/etc/vintf/manifest/dsa-channel.xml").decode()
                self.assertIn('<hal format="aidl">', manifest)
                self.assertIn("<name>io.github.thisisthepy.dsa.channel</name>", manifest)
                self.assertIn("<version>1</version>", manifest)
                self.assertIn("<fqname>IChannel/default</fqname>", manifest)
                contexts = archive.read("VENDOR/etc/selinux/vendor_service_contexts").decode().splitlines()
                self.assertIn(
                    "android.hardware.graphics.composer3.IComposer/default u:object_r:hal_graphics_composer_service:s0",
                    contexts,
                )
                self.assertIn(
                    "io.github.thisisthepy.dsa.channel.IChannel/default u:object_r:dsa_channel_service:s0",
                    contexts,
                )
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/etc/vintf/manifest/dsa-channel.xml 0 0 644 capabilities=0x0", fs_config)
                policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertIn("(typeattributeset service_manager_type (dsa_channel_service))", policy)
                self.assertIn("(allow dsa_channeld dsa_channel_service (service_manager (add find)))", policy)
                self.assertIn("(allow dsa_channeld servicemanager (binder (call transfer)))", policy)
                for client in ["system_app", "platform_app", "hal_graphics_composer_server"]:
                    self.assertIn(f"(allow {client} dsa_channel_service (service_manager (find)))", policy)
                    self.assertIn(f"(allow {client} dsa_channeld (binder (call transfer)))", policy)
                    # The listener callbacks go the other way.
                    self.assertIn(f"(allow dsa_channeld {client} (binder (call transfer)))", policy)
                # Only the daemon registers the service.
                self.assertEqual(policy.count("(service_manager (add"), 1)

    def test_fr3_the_channel_service_is_labelled_without_vendor_service_contexts(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            daemon = root / "dsa-channeld"
            daemon.write_bytes(b"daemon")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
            modified = root / "modified.zip"
            make_image.modify(source, modified, channeld=daemon)
            with zipfile.ZipFile(modified) as archive:
                self.assertEqual(
                    archive.read("VENDOR/etc/selinux/vendor_service_contexts").decode(),
                    "io.github.thisisthepy.dsa.channel.IChannel/default u:object_r:dsa_channel_service:s0\n",
                )

    def test_fr3_the_service_name_matches_the_aidl_package(self):
        aidl = Path(__file__).parents[1] / "aidl/io/github/thisisthepy/dsa/channel/IChannel.aidl"
        package = next(
            line.split()[1].rstrip(";") for line in aidl.read_text().splitlines() if line.startswith("package ")
        )
        self.assertEqual(make_image.CHANNEL_INSTANCE, f"{package}.IChannel/default")

    def test_pr6_the_daemon_label_reaches_the_file_contexts_the_images_are_built_from(self):
        # mkfs.erofs labels the vendor image from META/file_contexts.bin. Left as it was,
        # the daemon came out as vendor_file and init refused to start it.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            daemon = root / "dsa-channeld"
            daemon.write_bytes(b"daemon")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_file_contexts", "/system/bin/init u:object_r:init_exec:s0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
                archive.writestr("META/file_contexts.bin", b"\x8a\xff\x7c\xf9 compiled")

            modified = root / "modified.zip"
            make_image.modify(source, modified, channeld=daemon)

            with zipfile.ZipFile(modified) as archive:
                lines = archive.read("META/file_contexts.bin").decode().splitlines()
            self.assertEqual(lines[0], "/system/bin/init u:object_r:init_exec:s0")
            self.assertIn("/vendor/bin/sh u:object_r:vendor_shell_exec:s0", lines)
            self.assertIn("/vendor/bin/dsa-channeld u:object_r:dsa_channeld_exec:s0", lines)

    def test_pr6_without_a_control_daemon_the_image_has_none(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                self.assertNotIn("VENDOR/bin/dsa-channeld", archive.namelist())
                self.assertNotIn("VENDOR/etc/init/dsa-channeld.rc", archive.namelist())
                self.assertNotIn("VENDOR/etc/vintf/manifest/dsa-channel.xml", archive.namelist())
                self.assertNotIn("VENDOR/etc/selinux/vendor_service_contexts", archive.namelist())
                self.assertNotIn("dsa_channeld", archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode())

    def _target_files_with_system_ext(self, root):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr(
                "META/apkcerts.txt",
                'name="Settings.apk" certificate="build/make/target/product/security/platform.x509.pem" '
                'private_key="build/make/target/product/security/platform.pk8" partition="system_ext"\n',
            )
            archive.writestr("META/apexkeys.txt", "")
            archive.writestr(
                "META/system_ext_filesystem_config.txt",
                "system_ext/priv-app 0 0 755 capabilities=0x0\n",
            )
        return source

    def test_fr3_the_agent_is_installed_as_a_privileged_app(self):
        # The agent reads every app's tasks and starts and ends them, which only a privileged
        # app may; the framework refuses to boot if its privileged permissions are not listed.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self._target_files_with_system_ext(root)
            agent = root / "agent.apk"
            agent.write_bytes(b"PK agent")

            modified = root / "modified.zip"
            make_image.modify(source, modified, agent=agent)

            with zipfile.ZipFile(modified) as archive:
                self.assertEqual(archive.read("SYSTEM_EXT/priv-app/DsaAgent/DsaAgent.apk"), b"PK agent")
                permissions = archive.read("SYSTEM_EXT/etc/permissions/privapp-permissions-dsa-agent.xml").decode()
                self.assertIn('<privapp-permissions package="io.github.thisisthepy.dsa.agent">', permissions)
                fs_config = archive.read("META/system_ext_filesystem_config.txt").decode().splitlines()
                self.assertIn("system_ext/priv-app 0 0 755 capabilities=0x0", fs_config)
                self.assertIn("system_ext/priv-app/DsaAgent 0 0 755 capabilities=0x0", fs_config)
                self.assertIn("system_ext/priv-app/DsaAgent/DsaAgent.apk 0 0 644 capabilities=0x0", fs_config)
                self.assertIn(
                    "system_ext/etc/permissions/privapp-permissions-dsa-agent.xml 0 0 644 capabilities=0x0",
                    fs_config,
                )

    def test_fr3_the_agent_is_signed_with_the_platform_key(self):
        # A platform app may use the framework interfaces the agent needs. sign_target_files_apks
        # signs each app with the key apkcerts.txt names, and refuses an app it names none for.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self._target_files_with_system_ext(root)
            agent = root / "agent.apk"
            agent.write_bytes(b"PK agent")
            modified = root / "modified.zip"
            make_image.modify(source, modified, agent=agent)

            with zipfile.ZipFile(modified) as archive:
                lines = archive.read("META/apkcerts.txt").decode().splitlines()
            entry = [line for line in lines if line.startswith('name="DsaAgent.apk"')]
            self.assertEqual(
                entry,
                ['name="DsaAgent.apk" certificate="build/make/target/product/security/platform.x509.pem" '
                 'private_key="build/make/target/product/security/platform.pk8" partition="system_ext"'],
            )
            self.assertEqual(len(lines), 2)
            # The platform certificate is one of the standard ones -d replaces with ours, so
            # no -k option sends the agent to another key.
            args = make_image.key_arguments(modified, "/keys")
            overrides = [args[i + 1] for i, arg in enumerate(args) if arg == "-k"]
            self.assertFalse(any(o.startswith("build/make/target/product/security/platform=") for o in overrides))

    def test_fr3_the_privileged_permission_list_names_every_permission_the_agent_requests(self):
        agent = make_image.REPO / "dsa/guest/agent"
        manifest = (agent / "src/main/AndroidManifest.xml").read_text()
        listed = (agent / "privapp-permissions-dsa-agent.xml").read_text()
        requested = set(re.findall(r'<uses-permission android:name="([^"]+)"', manifest))
        granted = set(re.findall(r'<permission name="([^"]+)"', listed))
        self.assertTrue(requested)
        self.assertEqual(requested, granted)
        self.assertEqual(make_image.AGENT_PERMISSIONS_SOURCE.read_text(), listed)

    def test_fr3_an_agent_needs_a_system_ext_partition(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("META/apkcerts.txt", "")
            agent = root / "agent.apk"
            agent.write_bytes(b"PK agent")
            modified = root / "modified.zip"
            with self.assertRaises(ValueError):
                make_image.modify(source, modified, agent=agent)
            self.assertFalse(modified.exists())

    def test_fr3_without_an_agent_the_image_has_none(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self._target_files_with_system_ext(root)
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                self.assertFalse(any("DsaAgent" in name for name in archive.namelist()))
                self.assertNotIn("DsaAgent", archive.read("META/apkcerts.txt").decode())

    def test_fr3_the_dsa_hwcomposer_replaces_the_cuttlefish_composer(self):
        # Cuttlefish's composer composes the whole display; DSA's sends the host each task's
        # frames. Both composer APEXes go, so nothing else serves the composer HAL.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            hwcomposer = root / "dsa-hwcomposer"
            hwcomposer.write_bytes(b"\x7fELF hwcomposer")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("VENDOR/apex/com.android.hardware.graphics.composer.ranchu.apex", b"apex")
                archive.writestr("VENDOR/apex/com.android.hardware.graphics.composer.drm_hwcomposer.apex", b"apex")
                archive.writestr("VENDOR/apex/com.android.hardware.gatekeeper.nonsecure.apex", b"apex")
                archive.writestr(
                    "META/apexkeys.txt",
                    'name="com.android.hardware.graphics.composer.ranchu.apex" public_key="a"\n'
                    'name="com.android.hardware.gatekeeper.nonsecure.apex" public_key="b"\n',
                )
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
                archive.writestr("META/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")

            modified = root / "modified.zip"
            make_image.modify(source, modified, hwcomposer=hwcomposer)

            with zipfile.ZipFile(modified) as archive:
                names = archive.namelist()
                self.assertNotIn("VENDOR/apex/com.android.hardware.graphics.composer.ranchu.apex", names)
                self.assertNotIn("VENDOR/apex/com.android.hardware.graphics.composer.drm_hwcomposer.apex", names)
                self.assertIn("VENDOR/apex/com.android.hardware.gatekeeper.nonsecure.apex", names)
                apexkeys = archive.read("META/apexkeys.txt").decode()
                self.assertNotIn("graphics.composer", apexkeys)
                self.assertIn("gatekeeper", apexkeys)
                self.assertEqual(archive.read("VENDOR/bin/hw/dsa-hwcomposer"), b"\x7fELF hwcomposer")
                rc = archive.read("VENDOR/etc/init/dsa-hwcomposer.rc").decode()
                self.assertIn("service vendor.dsa-hwcomposer /vendor/bin/hw/dsa-hwcomposer", rc)
                self.assertIn("onrestart restart surfaceflinger", rc)
                manifest = archive.read("VENDOR/etc/vintf/manifest/dsa-hwcomposer.xml").decode()
                self.assertIn("<name>android.hardware.graphics.composer3</name>", manifest)
                self.assertIn("<version>4</version>", manifest)
                self.assertIn("<fqname>IComposer/default</fqname>", manifest)
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/bin/hw/dsa-hwcomposer 0 2000 755 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/init/dsa-hwcomposer.rc 0 0 644 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/vintf/manifest/dsa-hwcomposer.xml 0 0 644 capabilities=0x0", fs_config)
                for contexts in ["VENDOR/etc/selinux/vendor_file_contexts", "META/vendor_file_contexts"]:
                    self.assertIn(
                        "/vendor/bin/hw/dsa-hwcomposer u:object_r:hal_graphics_composer_default_exec:s0",
                        archive.read(contexts).decode(),
                    )

    def test_fr3_without_the_dsa_hwcomposer_the_cuttlefish_composer_stays(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("VENDOR/apex/com.android.hardware.graphics.composer.ranchu.apex", b"apex")
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                self.assertIn("VENDOR/apex/com.android.hardware.graphics.composer.ranchu.apex", archive.namelist())
                self.assertNotIn("VENDOR/bin/hw/dsa-hwcomposer", archive.namelist())

    def test_fr3_the_kernel_command_line_sets_up_the_dsa_hwcomposer(self):
        cmdline = make_image.kernel_cmdline("blkdevparts=sda:1@0(misc)", hwcomposer=True)
        # Cuttlefish's composer, and the settings it alone reads, are gone.
        self.assertNotIn("hwcomposer.mode=noop", cmdline)
        self.assertNotIn("display_finder_mode", cmdline)
        self.assertNotIn("androidboot.vendor.apex.com.android.hardware.graphics.composer=", cmdline)
        # The one freeform display every task window lives on.
        self.assertIn("androidboot.dsa.display_width=1920", cmdline)
        self.assertIn("androidboot.dsa.display_height=1080", cmdline)
        self.assertIn("androidboot.dsa.display_dpi=160", cmdline)
        self.assertIn(
            "androidboot.vendor.apex.com.android.hardware.gatekeeper=com.android.hardware.gatekeeper.nonsecure",
            cmdline,
        )

    def test_fr1_signing_finds_the_certificates_it_replaces(self):
        # sign_target_files_apks swaps AOSP's certificates for ours in mac_permissions.xml,
        # reading them at the paths META/apkcerts.txt names, relative to where it runs, and
        # silently skips any it cannot open. Then the platform apps get seinfo "default",
        # match nothing in seapp_contexts, and zygote aborts starting them.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            security = root / "otatools/build/target/product/security"
            security.mkdir(parents=True)
            (security / "platform.x509.pem").write_text("platform")
            (security / "platform.pk8").write_bytes(b"key")

            cwd = make_image.aosp_test_certificates(root / "otatools", root / "aosp")

            for prefix in ["build/make/target/product/security", "build/target/product/security"]:
                self.assertEqual((cwd / prefix / "platform.x509.pem").read_text(), "platform")

    def test_fr1_signing_finds_certificates_otatools_does_not_ship(self):
        # Bluetooth is signed with its own certificate, kept in packages/modules/Bluetooth
        # and not in otatools. mac_permissions.xml holds it for the bluetooth seinfo, so
        # it is written where apkcerts.txt names it.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            security = root / "otatools/build/target/product/security"
            security.mkdir(parents=True)
            (security / "platform.x509.pem").write_text("platform")
            bluetooth = b"aosp bluetooth certificate"
            target_files = root / "target-files.zip"
            with zipfile.ZipFile(target_files, "w") as archive:
                archive.writestr(
                    "META/apkcerts.txt",
                    'name="Bluetooth.apk" certificate="packages/modules/Bluetooth/android/app/certs/'
                    'com.android.bluetooth.x509.pem" private_key="packages/modules/Bluetooth/android/'
                    'app/certs/com.android.bluetooth.pk8"\n',
                )
                archive.writestr(
                    "SYSTEM/etc/selinux/plat_mac_permissions.xml",
                    f'<policy><signer signature="{bluetooth.hex()}"><seinfo value="bluetooth"/></signer></policy>',
                )

            cwd = make_image.aosp_test_certificates(root / "otatools", root / "aosp", target_files)

            pem = (cwd / "packages/modules/Bluetooth/android/app/certs/com.android.bluetooth.x509.pem").read_text()
            body = "".join(line for line in pem.splitlines() if "-----" not in line)
            self.assertEqual(base64.b64decode(body), bluetooth)
            self.assertIn("-----BEGIN CERTIFICATE-----", pem)

    def test_fr1_signing_fails_when_otatools_has_no_test_certificates(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "otatools").mkdir()
            with self.assertRaises(RuntimeError):
                make_image.aosp_test_certificates(root / "otatools", root / "aosp")

    def test_fr1_check_image_rejects_mac_permissions_with_the_old_platform_certificate(self):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            ours = base64.b64encode(b"our platform certificate").decode()
            (keys_dir / "platform.x509.pem").write_text(
                f"-----BEGIN CERTIFICATE-----\n{ours}\n-----END CERTIFICATE-----\n"
            )
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            aosp = b"aosp test platform certificate".hex()
            with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                archive.writestr("META/apkcerts.txt", "")
                archive.writestr(
                    "SYSTEM/etc/selinux/plat_mac_permissions.xml",
                    f'<policy><signer signature="{aosp}"><seinfo value="platform"/></signer></policy>',
                )

            proc = subprocess.run(
                [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                capture_output=True,
                text=True,
            )
            self.assertIn("plat_mac_permissions.xml does not name our platform certificate", proc.stdout)

    def test_pr3_check_image_verifies_graphics_stack(self):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (keys_dir / "test.x509.pem").write_text("-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n")

            manifest = {
                "files": {},
                "kernel_cmdline": "console=ttyS0 init=/init",
            }
            (img_dir / "image.json").write_text(json.dumps(manifest))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))

            with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                archive.writestr("META/apkcerts.txt", "")

            proc = subprocess.run(
                [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("vulkan.ranchu.so is missing", proc.stdout)
            self.assertIn("libOpenglSystemCommon.so is missing", proc.stdout)
            self.assertIn("androidboot.hardware.gltransport=hvsocket", proc.stdout)
            # Without the daemon nothing in the guest connects to the host's control channel.
            self.assertIn("VENDOR/bin/dsa-channeld is missing", proc.stdout)
            # Without the manifest and the label, the agent and the hwcomposer cannot reach it.
            self.assertIn("dsa-channel.xml is missing", proc.stdout)
            self.assertIn("vendor_service_contexts does not label", proc.stdout)
            # Without the agent the host never learns of a task, so no window opens.
            self.assertIn("SYSTEM_EXT/priv-app/DsaAgent/DsaAgent.apk is missing", proc.stdout)

    def test_fr3_check_image_rejects_an_agent_not_signed_with_the_platform_key(self):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            for role in ["platform", "releasekey"]:
                body = base64.b64encode(f"our {role} certificate".encode()).decode()
                (keys_dir / f"{role}.x509.pem").write_text(
                    f"-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n"
                )
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                archive.writestr("META/apkcerts.txt", "")
                archive.writestr("SYSTEM_EXT/priv-app/DsaAgent/DsaAgent.apk", b"signing block: our releasekey certificate")
                archive.writestr("SYSTEM_EXT/etc/permissions/privapp-permissions-dsa-agent.xml", "<permissions/>")

            proc = subprocess.run(
                [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                capture_output=True,
                text=True,
            )
            self.assertIn("DsaAgent.apk is not signed with our platform key", proc.stdout)
            self.assertNotIn("DsaAgent.apk is missing", proc.stdout)
            self.assertNotIn("privapp-permissions-dsa-agent.xml is missing", proc.stdout)
            # Without DSA's composer no task window ever gets a frame.
            self.assertIn("VENDOR/bin/hw/dsa-hwcomposer is missing", proc.stdout)
            self.assertIn("androidboot.dsa.display_width", proc.stdout)

    def test_fr3_check_image_rejects_the_wifi_trace_setup(self):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                archive.writestr("META/apkcerts.txt", "")
                archive.writestr("SYSTEM/etc/init/wifi.rc", self.WIFI_RC)

            proc = subprocess.run(
                [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("wifi.rc still sets up a kernel trace instance when boot completes", proc.stdout)

            with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                archive.writestr("META/apkcerts.txt", "")
                archive.writestr("SYSTEM/etc/init/wifi.rc", make_image.edit_wifi_rc(self.WIFI_RC))
            proc = subprocess.run(
                [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                capture_output=True,
                text=True,
            )
            self.assertNotIn("wifi.rc", proc.stdout)


if __name__ == "__main__":
    unittest.main()
