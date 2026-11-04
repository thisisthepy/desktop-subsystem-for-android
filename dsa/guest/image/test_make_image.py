"""Regression tests for the guest image layout helpers."""

import base64
import gzip
import hashlib
import importlib.util
import io
import json
import re
import struct
import subprocess
import sys
import tempfile
import unittest
import zipfile
import zlib
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
                self.assertIn("/system/bin/logcat -b main,system,crash -v threadtime -f /dev/ttyS0 *:I", rc)
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
                self.assertIn("priority -10", rc)
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
                # The agent's process carries MLS categories; without this the daemon cannot read
                # its name, refuses it as the sender, and every boot ends Stalled. Opening
                # /proc/<pid>/cmdline also checks getattr, and a refusal is logged on every call.
                for client in ["system_app", "platform_app"]:
                    self.assertIn(f"(allow dsa_channeld {client} (dir (search)))", policy)
                    self.assertIn(f"(allow dsa_channeld {client} (file (read open getattr)))", policy)
                self.assertIn("(typeattributeset mlstrustedsubject (dsa_channeld))", policy)

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

    def test_fr10_the_agent_may_put_the_guest_into_doze_and_set_its_battery(self):
        # The agent calls the deviceidle and battery shell commands over binder, handing
        # system_server the descriptors the commands write to.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self._target_files_with_system_ext(root)
            with zipfile.ZipFile(source, "a") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type platform_app)\n")
            agent = root / "agent.apk"
            agent.write_bytes(b"PK agent")
            modified = root / "modified.zip"
            make_image.modify(source, modified, agent=agent)
            with zipfile.ZipFile(modified) as archive:
                policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertIn("(allow platform_app deviceidle_service (service_manager (find)))", policy)
                self.assertIn("(allow platform_app battery_service (service_manager (find)))", policy)
                self.assertIn("(allow system_server platform_app (fd (use)))", policy)
                self.assertIn("(allow system_server platform_app (fifo_file (write getattr)))", policy)

    def test_fr5_surfaceflinger_may_write_its_layer_timings_into_the_agents_pipe(self):
        # The input trace reads each app layer's timings with SurfaceFlinger's dump, which
        # SurfaceFlinger writes into a pipe the agent hands it.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self._target_files_with_system_ext(root)
            with zipfile.ZipFile(source, "a") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type platform_app)\n")
            agent = root / "agent.apk"
            agent.write_bytes(b"PK agent")
            modified = root / "modified.zip"
            make_image.modify(source, modified, agent=agent)
            with zipfile.ZipFile(modified) as archive:
                policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertIn("(allow platform_app surfaceflinger_service (service_manager (find)))", policy)
                self.assertIn("(allow surfaceflinger platform_app (fd (use)))", policy)
                self.assertIn("(allow surfaceflinger platform_app (fifo_file (write getattr)))", policy)

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

    def test_pr4_dsa_power_and_health_hals_replace_cuttlefishs(self):
        # The host serves the guest's power hints and its battery; Cuttlefish's example power
        # HAL and its made-up battery go, so nothing else serves those HALs.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            hal = root / "power"
            hal.mkdir()
            (hal / "dsa-power").write_bytes(b"\x7fELF power")
            (hal / "dsa-health").write_bytes(b"\x7fELF health")
            channeld = root / "dsa-channeld"
            channeld.write_bytes(b"\x7fELF channeld")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("VENDOR/apex/com.android.hardware.power.apex", b"apex")
                archive.writestr("VENDOR/apex/com.google.cf.health.apex", b"apex")
                archive.writestr("VENDOR/apex/com.android.hardware.gatekeeper.nonsecure.apex", b"apex")
                archive.writestr(
                    "META/apexkeys.txt",
                    'name="com.android.hardware.power.apex" public_key="a"\n'
                    'name="com.google.cf.health.apex" public_key="b"\n'
                    'name="com.android.hardware.gatekeeper.nonsecure.apex" public_key="c"\n',
                )
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")

            modified = root / "modified.zip"
            make_image.modify(source, modified, channeld=channeld, power_hal=hal)

            with zipfile.ZipFile(modified) as archive:
                names = archive.namelist()
                self.assertNotIn("VENDOR/apex/com.android.hardware.power.apex", names)
                self.assertNotIn("VENDOR/apex/com.google.cf.health.apex", names)
                self.assertIn("VENDOR/apex/com.android.hardware.gatekeeper.nonsecure.apex", names)
                apexkeys = archive.read("META/apexkeys.txt").decode()
                self.assertNotIn("hardware.power", apexkeys)
                self.assertNotIn("cf.health", apexkeys)
                self.assertIn("gatekeeper", apexkeys)
                self.assertEqual(archive.read("VENDOR/bin/hw/dsa-power"), b"\x7fELF power")
                self.assertEqual(archive.read("VENDOR/bin/hw/dsa-health"), b"\x7fELF health")
                self.assertIn("service vendor.dsa-power /vendor/bin/hw/dsa-power",
                              archive.read("VENDOR/etc/init/dsa-power.rc").decode())
                self.assertIn("service vendor.dsa-health /vendor/bin/hw/dsa-health",
                              archive.read("VENDOR/etc/init/dsa-health.rc").decode())
                power = archive.read("VENDOR/etc/vintf/manifest/dsa-power.xml").decode()
                self.assertIn("<name>android.hardware.power</name>", power)
                self.assertIn("<version>5</version>", power)
                self.assertIn("<fqname>IPower/default</fqname>", power)
                health = archive.read("VENDOR/etc/vintf/manifest/dsa-health.xml").decode()
                self.assertIn("<name>android.hardware.health</name>", health)
                self.assertIn("<version>3</version>", health)
                self.assertIn("<fqname>IHealth/default</fqname>", health)
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/bin/hw/dsa-power 0 2000 755 capabilities=0x0", fs_config)
                self.assertIn("vendor/bin/hw/dsa-health 0 2000 755 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/init/dsa-power.rc 0 0 644 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/vintf/manifest/dsa-health.xml 0 0 644 capabilities=0x0", fs_config)
                contexts = archive.read("VENDOR/etc/selinux/vendor_file_contexts").decode()
                self.assertIn("/vendor/bin/hw/dsa-power u:object_r:hal_power_default_exec:s0", contexts)
                self.assertIn("/vendor/bin/hw/dsa-health u:object_r:hal_health_default_exec:s0", contexts)
                policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertIn("(allow hal_power_server dsa_channel_service (service_manager (find)))", policy)
                self.assertIn("(allow dsa_channeld hal_health_server (binder (call transfer)))", policy)

    def test_pr4_the_power_hals_need_the_channel_daemon(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
            with self.assertRaises(ValueError):
                make_image.modify(source, root / "modified.zip", power_hal=root)

    def test_pr4_the_codec_hal_is_installed_with_its_decoders_and_policy(self):
        # The host decodes the guest's video on its GPU: the image carries the codec HAL, the
        # list of its decoders, its service's label, and keeps codec buffers out of pools.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            codec2 = root / "dsa-codec2"
            codec2.write_bytes(b"\x7fELF codec2")
            channeld = root / "dsa-channeld"
            channeld.write_bytes(b"\x7fELF channeld")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("VENDOR/build.prop", "ro.debuggable=1\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_service_contexts", "")

            modified = root / "modified.zip"
            make_image.modify(source, modified, channeld=channeld, codec2=codec2)

            with zipfile.ZipFile(modified) as archive:
                self.assertEqual(archive.read("VENDOR/bin/hw/dsa-codec2"), b"\x7fELF codec2")
                self.assertIn("service vendor.dsa-codec2 /vendor/bin/hw/dsa-codec2",
                              archive.read("VENDOR/etc/init/dsa-codec2.rc").decode())
                manifest = archive.read("VENDOR/etc/vintf/manifest/dsa-codec2.xml").decode()
                self.assertIn("<name>android.hardware.media.c2</name>", manifest)
                self.assertIn("<fqname>IComponentStore/default</fqname>", manifest)
                codecs = archive.read("VENDOR/etc/media_codecs_c2.xml").decode()
                for name in ["c2.dsa.avc.decoder", "c2.dsa.hevc.decoder", "c2.dsa.vp9.decoder", "c2.dsa.av1.decoder"]:
                    self.assertIn(f'name="{name}"', codecs)
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/bin/hw/dsa-codec2 0 2000 755 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/media_codecs_c2.xml 0 0 644 capabilities=0x0", fs_config)
                self.assertIn("/vendor/bin/hw/dsa-codec2 u:object_r:mediacodec_exec:s0",
                              archive.read("VENDOR/etc/selinux/vendor_file_contexts").decode())
                self.assertIn("android.hardware.media.c2.IComponentStore/default u:object_r:hal_codec2_service:s0",
                              archive.read("VENDOR/etc/selinux/vendor_service_contexts").decode())
                self.assertIn("(allow mediacodec self (vsock_socket",
                              archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode())
                self.assertIn("debug.stagefright.c2-poolmask=262144", archive.read("VENDOR/build.prop").decode())

    def test_pr4_without_the_codec_hal_codec_buffers_stay_pooled(self):
        self.assertNotIn("debug.stagefright.c2-poolmask", make_image.edit_props("ro.debuggable=1\n"))

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
            # Without DSA's power and health HALs the host never hears the guest's power hints.
            self.assertIn("VENDOR/bin/hw/dsa-power is missing", proc.stdout)
            self.assertIn("VENDOR/bin/hw/dsa-health is missing", proc.stdout)
            # Without the codec HAL every video decodes on the guest's processors.
            self.assertIn("VENDOR/bin/hw/dsa-codec2 is missing", proc.stdout)
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


class GuestMemoryTests(unittest.TestCase):
    """The image keeps the guest's memory small, so the host gets back what it does not use."""

    def _modify(self, root, channeld=False, extra=None):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
            archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
            for name, data in (extra or {}).items():
                archive.writestr(name, data)
        daemon = None
        if channeld:
            daemon = root / "dsa-channeld"
            daemon.write_bytes(b"daemon")
        modified = root / "modified.zip"
        make_image.modify(source, modified, channeld=daemon)
        return zipfile.ZipFile(modified)

    def test_nfr1_the_guest_swaps_to_zram_of_half_its_memory(self):
        fstab = (Path(make_image.__file__).with_name("fstab.dsa")).read_text()
        self.assertIn("/dev/block/zram0 none swap defaults zramsize=50%", fstab)
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-memory.rc").decode()
            self.assertIn("swapon_all /vendor/etc/fstab.dsa", rc)
            self.assertIn("write /proc/sys/vm/swappiness 100", rc)
            self.assertIn("write /proc/sys/vm/page-cluster 0", rc)
            self.assertIn(
                "(allow vendor_init proc_page_cluster (file (write open getattr)))",
                archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode(),
            )
            self.assertIn(
                "vendor/etc/init/dsa-memory.rc 0 0 644 capabilities=0x0",
                archive.read("META/vendor_filesystem_config.txt").decode(),
            )
            for directory in make_image.FSTAB_DIRS:
                self.assertIn(b"zramsize=50%", archive.read(directory + "fstab.dsa"))

    def test_nfr1_swapon_all_does_not_stop_at_the_missing_max_comp_streams(self):
        # fs_mgr opens /sys/block/zram0/max_comp_streams for any value of 0 or more, its
        # default, and skips the device when the file is missing, as it is on the guest's
        # kernel. Only -1 lets it go on to the size, mkswap and swapon.
        fstab = (Path(make_image.__file__).with_name("fstab.dsa")).read_text()
        entries = [line.split() for line in fstab.splitlines() if line.startswith("/dev/block/zram0")]
        self.assertEqual(len(entries), 1)
        self.assertIn("max_comp_streams=-1", entries[0][4].split(","))
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            for directory in make_image.FSTAB_DIRS:
                self.assertIn(b"max_comp_streams=-1", archive.read(directory + "fstab.dsa"))

    def test_nfr1_the_kernel_has_what_zram_swap_needs(self):
        config = (make_image.REPO / "dsa/kernel/android.config").read_text().splitlines()
        for option in ("CONFIG_SWAP", "CONFIG_ZSMALLOC", "CONFIG_ZRAM",
                       "CONFIG_ZRAM_BACKEND_LZ4", "CONFIG_ZRAM_DEF_COMP_LZ4"):
            self.assertIn(f"{option}=y", config)

    def test_nfr1_lmkd_watches_the_zram_swap(self):
        props = make_image.edit_props("ro.debuggable=1\n")
        for line in [
            "ro.lmk.use_psi=true",
            "ro.lmk.use_minfree_levels=false",
            "ro.lmk.swap_free_low_percentage=10",
            "ro.lmk.swap_util_max=90",
            "ro.lmk.thrashing_limit=30",
        ]:
            self.assertIn(line + "\n", props)

    def test_nfr2_vendor_init_may_drop_caches_compact_and_tune_swap_only(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn('(genfscon proc "/sys/vm/compact_memory" (u object_r dsa_proc_vm_tunable', policy)
            self.assertIn('(genfscon proc "/sys/vm/swappiness" (u object_r dsa_proc_vm_tunable', policy)
            self.assertIn("(allow vendor_init dsa_proc_vm_tunable (file (write open getattr)))", policy)
            self.assertIn("(allow vendor_init proc_drop_caches (file (write open getattr)))", policy)
            # Nothing gains write access to plain proc.
            self.assertNotRegex(policy, r"\(allow \S+ proc \(file \([^)]*write")
            # The agent reads zram's statistics for its memory report.
            self.assertIn("(allow platform_app sysfs_zram (file (read open getattr)))", policy)

    def test_fr5_the_audio_hal_and_software_codecs_can_share_memory_through_memfds(self):
        # Refused, libcutils falls back to an ashmem device DSA's kernel does not have: audio
        # policy never starts, system_server retries ten times a second, and every click
        # decodes its sound effects again and fails, at the moment the app has to respond.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        for domain in ["hal_audio_server", "mediaswcodec"]:
            # The test memfd libcutils sizes, and the ashmem ioctls it asks of it.
            self.assertIn(f"(allow {domain} tmpfs (file (ioctl read write getattr map)))", policy)
            self.assertIn(f"(allowx {domain} tmpfs (ioctl file ((range 0x7701 0x770b))))", policy)
        for domain, memfd in [("audioserver", "tmpfs"), ("system_server", "tmpfs"),
                              ("hal_audio_server", "audioserver_tmpfs"),
                              ("hal_audio_server", "system_server_tmpfs"),
                              ("mediaswcodec", "system_server_tmpfs")]:
            self.assertIn(f"(allow {domain} {memfd} (file (read write getattr map)))", policy)

    def test_fr5_uwb_is_not_advertised_without_its_hal(self):
        # Declared, system_server's threads wait for the HAL for as long as the guest runs,
        # and servicemanager tries to start it about twice a second.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            xml = archive.read("VENDOR/etc/permissions/dsa-unavailable-features.xml").decode()
        self.assertIn('<unavailable-feature name="android.hardware.uwb" />', xml)

    def test_fr5_the_logger_of_a_console_hyper_v_does_not_have_is_removed(self):
        # It exits as soon as it starts, init starts it again every five seconds, and each
        # exit runs flags_health_check too.
        self.assertTrue(make_image.removed("VENDOR/etc/init/seriallogging.rc"))

    def test_nfr2_only_the_control_daemon_asks_init_to_reclaim(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            contexts = archive.read("VENDOR/etc/selinux/vendor_property_contexts").decode().splitlines()
            self.assertIn("vendor.dsa.memory.reclaim u:object_r:dsa_memory_prop:s0 exact bool", contexts)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn("(typeattributeset vendor_property_type (dsa_memory_prop))", policy)
            setters = re.findall(r"\(allow (\S+) dsa_memory_prop \(property_service \(set\)\)\)", policy)
            self.assertEqual(sorted(setters), ["dsa_channeld", "vendor_init"])
            rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode()
            trigger = rc[rc.index("on property:vendor.dsa.memory.reclaim=1"):]
            self.assertIn("write /proc/sys/vm/drop_caches 3", trigger)
            self.assertIn("write /proc/sys/vm/compact_memory 1", trigger)
            self.assertIn("setprop vendor.dsa.memory.reclaim 0", trigger)

    def test_nfr1_an_idle_reclaim_swaps_cold_pages_out_before_dropping_caches_and_waits_for_boot(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode()
            start = rc.index("on property:vendor.dsa.memory.reclaim=1 && property:sys.boot_completed=1")
            trigger = rc[start:rc.index("on property:vendor.dsa.memory.trim=1")]
            self.assertEqual(trigger.count('write /sys/fs/cgroup/memory.reclaim "256M swappiness=200"'), 4)
            self.assertLess(trigger.rindex("cgroup/memory.reclaim"), trigger.index("drop_caches"))
            self.assertLess(trigger.index("drop_caches"), trigger.index("compact_memory"))

    def test_nfr1_art_keeps_less_free_heap_without_lowering_what_an_app_may_use(self):
        props = make_image.edit_props("dalvik.vm.heapmaxfree=32m\ndalvik.vm.heapsize=512m\n").splitlines()
        self.assertIn("dalvik.vm.heaptargetutilization=0.75", props)
        self.assertIn("dalvik.vm.heapminfree=512k", props)
        self.assertIn("dalvik.vm.heapmaxfree=8m", props)
        self.assertNotIn("dalvik.vm.heapmaxfree=32m", props)
        self.assertIn("dalvik.vm.heapsize=512m", props)
        self.assertNotIn("ro.config.low_ram", "\n".join(props))

    def test_nfr1_the_secure_element_service_is_removed(self):
        self.assertTrue(make_image.removed("SYSTEM/app/SecureElement/SecureElement.apk"))

    def test_nfr2_a_gradual_trim_reclaims_a_bounded_slice_from_the_coldest_pages_and_reads_memory_pressure(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            contexts = archive.read("VENDOR/etc/selinux/vendor_property_contexts").decode().splitlines()
            self.assertIn("vendor.dsa.memory.trim u:object_r:dsa_memory_prop:s0 exact bool", contexts)
            rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode()
            trigger = rc[rc.index("on property:vendor.dsa.memory.trim=1"):]
            self.assertIn("write /sys/fs/cgroup/memory.reclaim 64M", trigger)
            self.assertNotIn("drop_caches", trigger)
            self.assertNotIn("compact_memory", trigger)
            self.assertIn("(allow vendor_init cgroup_v2 (file (write open getattr)))", archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode())
            self.assertIn("setprop vendor.dsa.memory.trim 0", trigger)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn("(allow dsa_channeld proc_pressure_mem (file (read open getattr)))", policy)

    def test_nfr2_the_reclaim_property_keeps_the_existing_vendor_contexts(self):
        existing = "vendor.example u:object_r:vendor_default_prop:s0\n"
        with tempfile.TemporaryDirectory() as tmp, self._modify(
            Path(tmp), extra={"VENDOR/etc/selinux/vendor_property_contexts": existing}
        ) as archive:
            contexts = archive.read("VENDOR/etc/selinux/vendor_property_contexts").decode().splitlines()
            self.assertEqual(contexts[0], existing.strip())
            self.assertEqual(contexts.count(make_image.RECLAIM_PROPERTY_CONTEXT), 1)
            self.assertEqual(contexts.count(make_image.TRIM_PROPERTY_CONTEXT), 1)
        all_of_them = (
            make_image.RECLAIM_PROPERTY_CONTEXT + "\n"
            + make_image.TRIM_PROPERTY_CONTEXT + "\n"
            + make_image.BOOT_PROPERTY_CONTEXT + "\n"
        )
        self.assertEqual(make_image.update_vendor_property_contexts(all_of_them), all_of_them)

    def test_fr2_init_tells_the_daemon_when_android_has_finished_booting(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            contexts = archive.read("VENDOR/etc/selinux/vendor_property_contexts").decode().splitlines()
            self.assertIn("vendor.dsa.boot_completed u:object_r:dsa_boot_prop:s0 exact bool", contexts)
            rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode()
            trigger = rc[rc.index("on property:sys.boot_completed=1\n"):]
            self.assertIn("setprop vendor.dsa.boot_completed 1", trigger.split("\n\n")[0])
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn("(typeattributeset vendor_property_type (dsa_boot_prop))", policy)
            setters = re.findall(r"\(allow (\S+) dsa_boot_prop \(property_service \(set\)\)\)", policy)
            self.assertEqual(setters, ["vendor_init"])
            self.assertIn("(allow dsa_channeld dsa_boot_prop (file (read open getattr map)))", policy)

    def test_nfr1_apps_a_desktop_has_no_use_for_are_removed_and_the_ones_apps_need_are_kept(self):
        removed = ["Dialer", "messaging", "Stk", "BasicDreams", "LiveWallpapersPicker", "QuickSearchBox"]
        kept = [
            "SYSTEM/app/webview/webview.apk",
            "SYSTEM/priv-app/ContactsProvider/ContactsProvider.apk",
            "SYSTEM/priv-app/PackageInstaller/PackageInstaller.apk",
            "SYSTEM/priv-app/DocumentsUI/DocumentsUI.apk",
            "SYSTEM/app/PrintSpooler/PrintSpooler.apk",
            "SYSTEM/priv-app/Settings/Settings.apk",
            "SYSTEM/priv-app/Telecom/Telecom.apk",
            "PRODUCT/app/DeskClock/DeskClock.apk",
        ]
        for app in removed:
            self.assertIn(app, make_image.REMOVED_APPS)
        extra = {f"PRODUCT/app/{app}/{app}.apk": b"apk" for app in removed}
        extra.update({name: b"apk" for name in kept})
        # Settings.apk is opened to edit its sound screen, so it has to be an APK.
        extra["SYSTEM/priv-app/Settings/Settings.apk"] = SettingsWithoutBluetoothTests.apk({})
        extra["META/apkcerts.txt"] = "".join(
            f'name="{Path(name).name}" certificate="c.x509.pem" private_key="c.pk8"\n'
            for name in list(extra) if name.endswith(".apk")
        )
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), extra=extra) as archive:
            names = archive.namelist()
            certs = archive.read("META/apkcerts.txt").decode()
            for app in removed:
                self.assertNotIn(f"PRODUCT/app/{app}/{app}.apk", names)
                self.assertNotIn(f'name="{app}.apk"', certs)
            for name in kept:
                self.assertIn(name, names)
                self.assertIn(f'name="{Path(name).name}"', certs)
        for needed in ["webview", "ContactsProvider", "PackageInstaller", "PermissionController",
                       "DocumentsUI", "PrintSpooler", "Settings", "Provision", "Telecom",
                       "TeleService", "DeskClock"]:
            self.assertNotIn(needed, make_image.REMOVED_APPS)


SETTINGS_NS = "http://schemas.android.com/apk/res-auto"
ANDROID_NS = "http://schemas.android.com/apk/res/android"
MEDIA_OUTPUT = "com.android.settings.sound.MediaOutputPreferenceController"
REMOTE_VOLUME = "com.android.settings.notification.RemoteVolumeGroupController"


def compiled_xml(root, utf8=True):
    """Compiles an element tree, (tag, [(namespace, name, value)], [children]), the way aapt2
    lays it out: a string pool, a namespace, then a start and an end chunk per element."""
    strings = []

    def index(text):
        if text not in strings:
            strings.append(text)
        return strings.index(text)

    def collect(node):
        tag, attributes, children = node
        index(tag)
        for namespace, name, value in attributes:
            index(namespace)
            index(name)
            index(value)
        for child in children:
            collect(child)

    index("settings")
    index(SETTINGS_NS)
    collect(root)

    data = b""
    offsets = []
    for text in strings:
        offsets.append(len(data))
        if utf8:
            encoded = text.encode("utf-8")
            data += bytes([len(text), len(encoded)]) + encoded + b"\0"
        else:
            data += struct.pack("<H", len(text)) + text.encode("utf-16-le") + b"\0\0"
    data += b"\0" * (-len(data) % 4)
    strings_start = 28 + 4 * len(strings)
    pool = struct.pack("<HHIIIIII", 0x0001, 28, strings_start + len(data), len(strings), 0,
                       0x100 if utf8 else 0, strings_start, 0)
    pool += b"".join(struct.pack("<I", offset) for offset in offsets) + data

    def node(chunk_type, body):
        return struct.pack("<HHIII", chunk_type, 16, 16 + len(body), 1, 0xFFFFFFFF) + body

    def element(item):
        tag, attributes, children = item
        body = struct.pack("<IIHHHHHH", 0xFFFFFFFF, index(tag), 20, 20, len(attributes), 0, 0, 0)
        for namespace, name, value in attributes:
            body += struct.pack("<IIIHBBI", index(namespace), index(name), index(value), 8, 0, 0x03,
                                index(value))
        end = node(0x0103, struct.pack("<II", 0xFFFFFFFF, index(tag)))
        return node(0x0102, body) + b"".join(element(child) for child in children) + end

    namespace = struct.pack("<II", index("settings"), index(SETTINGS_NS))
    tree = pool + node(0x0100, namespace) + element(root) + node(0x0101, namespace)
    return struct.pack("<HHI", 0x0003, 8, 8 + len(tree)) + tree


def elements(axml):
    """The (tag, key) of each element in compiled XML, in order."""
    strings = make_image.xml_strings(axml, 8)
    found = []
    offset = 8
    while offset < len(axml):
        chunk_type, header_size, size = struct.unpack_from("<HHI", axml, offset)
        if chunk_type == 0x0102:
            _, name, start, attribute_size, count = struct.unpack_from("<IIHHH", axml, offset + header_size)
            key = None
            for i in range(count):
                _, attribute, raw = struct.unpack_from(
                    "<III", axml, offset + header_size + start + i * attribute_size)
                if strings[attribute] == "key":
                    key = strings[raw]
            found.append((strings[name], key))
        offset += size
    return found


def preference(tag, key, controller=None, children=()):
    attributes = [(ANDROID_NS, "key", key)]
    if controller:
        attributes.append((SETTINGS_NS, "controller", controller))
    return (tag, attributes, list(children))


# The parts of Android 16's sound screen that matter here: the remote volume category, with
# a child to show the whole subtree goes, and the media output switcher among the sliders it
# shares a category with.
SOUND_SCREEN = preference("PreferenceScreen", "sound_settings", children=[
    preference("PreferenceCategory", "remote_media_group", REMOTE_VOLUME, children=[
        preference("Preference", "remote_session"),
    ]),
    preference("UntitledPreferenceCategory", "volume_controls_category", children=[
        preference("Preference", "media_output", MEDIA_OUTPUT),
        preference("VolumeSliderPreference", "media_volume",
                   "com.android.settings.sound.MediaVolumePreferenceController"),
    ]),
    preference("Preference", "alarm_ringtone"),
])


class SettingsWithoutBluetoothTests(unittest.TestCase):
    @staticmethod
    def apk(files):
        out = io.BytesIO()
        entries = [("AndroidManifest.xml", b"manifest", zipfile.ZIP_DEFLATED),
                   ("resources.arsc", b"table", zipfile.ZIP_STORED),
                   ("classes.dex", b"dex", zipfile.ZIP_DEFLATED)]
        entries += [(name, data, zipfile.ZIP_DEFLATED) for name, data in files.items()]
        with zipfile.ZipFile(out, "w") as archive:
            for name, data, compression in entries:
                # A fixed time, so the same files make the same bytes.
                info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
                info.compress_type = compression
                archive.writestr(info, data)
        return out.getvalue()

    def modified_settings(self, files):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM_EXT/priv-app/Settings/Settings.apk", self.apk(files))
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                return archive.read("SYSTEM_EXT/priv-app/Settings/Settings.apk")

    def test_fr1_the_sound_screen_opens_without_the_bluetooth_service(self):
        # Without Bluetooth, LocalMediaManager has no InfoMediaManager, and these two
        # controllers call it when the screen opens: Settings died with a
        # NullPointerException in MediaOutputPreferenceController.onStart.
        self.assertEqual(
            make_image.SETTINGS_CONTROLLERS_WITHOUT_BLUETOOTH["res/xml/sound_settings.xml"],
            [MEDIA_OUTPUT, REMOTE_VOLUME],
        )
        apk = self.modified_settings({"res/xml/sound_settings.xml": compiled_xml(SOUND_SCREEN)})
        with zipfile.ZipFile(io.BytesIO(apk)) as archive:
            screen = archive.read("res/xml/sound_settings.xml")
            infos = {info.filename: info for info in archive.infolist()}
            self.assertEqual(archive.read("classes.dex"), b"dex")
        self.assertEqual(elements(screen), [
            ("PreferenceScreen", "sound_settings"),
            ("UntitledPreferenceCategory", "volume_controls_category"),
            ("VolumeSliderPreference", "media_volume"),
            ("Preference", "alarm_ringtone"),
        ])
        self.assertEqual(struct.unpack_from("<I", screen, 4)[0], len(screen))
        # Signing aligns stored entries again, but only if they are still stored.
        self.assertEqual(infos["resources.arsc"].compress_type, zipfile.ZIP_STORED)
        self.assertEqual(infos["res/xml/sound_settings.xml"].compress_type, zipfile.ZIP_DEFLATED)

    def test_fr1_the_controllers_are_found_in_a_utf16_string_pool(self):
        edited, removed = make_image.remove_xml_elements(
            compiled_xml(SOUND_SCREEN, utf8=False), "controller", {MEDIA_OUTPUT, REMOTE_VOLUME})
        self.assertEqual(sorted(removed), sorted([MEDIA_OUTPUT, REMOTE_VOLUME]))
        self.assertNotIn(("Preference", "media_output"), elements(edited))
        self.assertNotIn(("Preference", "remote_session"), elements(edited))
        self.assertIn(("VolumeSliderPreference", "media_volume"), elements(edited))

    def test_fr1_a_settings_build_without_those_preferences_is_left_as_it_is(self):
        screen = preference("PreferenceScreen", "sound_settings", children=[
            preference("Preference", "alarm_ringtone"),
        ])
        files = {"res/xml/sound_settings.xml": compiled_xml(screen)}
        apk = self.apk(files)
        self.assertEqual(make_image.edit_settings_apk(apk, "Settings.apk"), apk)
        self.assertEqual(self.modified_settings({}), self.apk({}))

    def test_fr1_check_image_rejects_a_sound_screen_that_needs_bluetooth(self):
        check_script = Path(__file__).with_name("check-image.py")
        settings = self.apk({"res/xml/sound_settings.xml": compiled_xml(SOUND_SCREEN)})
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            outputs = []
            for apk in [settings, make_image.edit_settings_apk(settings, "Settings.apk")]:
                with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                    archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                    archive.writestr("META/apkcerts.txt", "")
                    archive.writestr("SYSTEM_EXT/priv-app/Settings/Settings.apk", apk)
                outputs.append(subprocess.run(
                    [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                    capture_output=True,
                    text=True,
                ).stdout)
        self.assertIn("crash Settings without the Bluetooth service", outputs[0])
        self.assertIn(MEDIA_OUTPUT, outputs[0])
        self.assertNotIn("crash Settings", outputs[1])

    def test_fr1_compiled_xml_that_is_not_xml_is_refused(self):
        with self.assertRaises(ValueError):
            make_image.remove_xml_elements(b"\x02\x00\x0c\x00" + b"\0" * 8, "controller", {MEDIA_OUTPUT})

    def test_fr1_the_connected_devices_screen_opens_without_the_bluetooth_adapter(self):
        # Settings died opening Connected devices: isAudioSharingSupported called
        # isLeAudioBroadcastSourceSupported on the null default adapter.
        self.assertEqual(make_image.SETTINGS_CODE_WITHOUT_BLUETOOTH[AUDIO_SHARING_SUPPORTED],
                         ("return false", None))
        dex = settings_dex()
        apk = self.modified_settings({"classes2.dex": dex})
        with zipfile.ZipFile(io.BytesIO(apk)) as archive:
            edited = archive.read("classes2.dex")
        code = dex_code(edited, AUDIO_SHARING_SUPPORTED)
        # const/4 v0, 0; return v0, then the call and its move-result as no-ops.
        self.assertEqual(code[:8], bytes.fromhex("12000f0000000000"))
        self.assertEqual(code[8:], dex_code(dex, AUDIO_SHARING_SUPPORTED)[8:])
        self.assert_dex_is_sealed(edited)
        self.assertEqual(make_image.edit_dex(edited, make_image.SETTINGS_CODE_WITHOUT_BLUETOOTH)[1], [])

    def test_fr1_the_accessibility_screen_opens_without_the_bluetooth_adapter(self):
        # The hearing aid controller cannot be made without Bluetooth, so the fragment's
        # use() returns null and onAttach called setFragmentManager on it.
        self.assertEqual(
            make_image.SETTINGS_CONTROLLERS_WITHOUT_BLUETOOTH["res/xml/accessibility_settings.xml"],
            [HEARING_AID],
        )
        self.assertEqual(make_image.SETTINGS_CODE_WITHOUT_BLUETOOTH[ACCESSIBILITY_ON_ATTACH],
                         ("skip call", SET_FRAGMENT_MANAGER))
        dex = settings_dex()
        apk = self.modified_settings({
            "classes3.dex": dex,
            "res/xml/accessibility_settings.xml": compiled_xml(ACCESSIBILITY_SCREEN),
        })
        with zipfile.ZipFile(io.BytesIO(apk)) as archive:
            edited = archive.read("classes3.dex")
            screen = archive.read("res/xml/accessibility_settings.xml")
        original = dex_code(dex, ACCESSIBILITY_ON_ATTACH)
        code = dex_code(edited, ACCESSIBILITY_ON_ATTACH)
        # Only the setFragmentManager call (three code units, from unit 13) is gone.
        self.assertEqual(code[:26], original[:26])
        self.assertEqual(code[26:32], bytes(6))
        self.assertEqual(code[32:], original[32:])
        self.assert_dex_is_sealed(edited)
        self.assertEqual(elements(screen), [
            ("PreferenceScreen", "accessibility_settings_screen"),
            ("PreferenceCategory", "audio_category"),
            ("Preference", "flash_notifications_preference_accessibility"),
        ])

    def test_fr1_a_dex_edit_that_would_break_the_method_is_refused(self):
        dex = settings_dex()
        with self.assertRaises(ValueError):
            make_image.edit_dex(dex, {ACCESSIBILITY_ON_ATTACH: ("return false", None)})
        with self.assertRaises(ValueError):
            make_image.edit_dex(dex, {ACCESSIBILITY_ON_ATTACH: ("skip call", FRAGMENT_USE)})

    def test_fr1_check_image_rejects_settings_code_that_needs_bluetooth(self):
        check_script = Path(__file__).with_name("check-image.py")
        settings = self.apk({"classes2.dex": settings_dex()})
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            outputs = []
            for apk in [settings, make_image.edit_settings_apk(settings, "Settings.apk")]:
                with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                    archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                    archive.writestr("META/apkcerts.txt", "")
                    archive.writestr("SYSTEM_EXT/priv-app/Settings/Settings.apk", apk)
                outputs.append(subprocess.run(
                    [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                    capture_output=True,
                    text=True,
                ).stdout)
        self.assertIn("crashes Settings without the Bluetooth adapter", outputs[0])
        self.assertIn(AUDIO_SHARING_SUPPORTED, outputs[0])
        self.assertIn(ACCESSIBILITY_ON_ATTACH, outputs[0])
        self.assertNotIn("crashes Settings", outputs[1])

    def assert_dex_is_sealed(self, dex):
        self.assertEqual(dex[12:32], hashlib.sha1(dex[32:]).digest())
        self.assertEqual(struct.unpack_from("<I", dex, 8)[0], zlib.adler32(dex[12:]))


HEARING_AID = "com.android.settings.accessibility.AccessibilityHearingAidPreferenceController"
AUDIO_SHARING_SUPPORTED = "Lcom/android/settingslib/bluetooth/BluetoothUtils;->isAudioSharingSupported"
ACCESSIBILITY_ON_ATTACH = "Lcom/android/settings/accessibility/AccessibilitySettings;->onAttach"
SET_FRAGMENT_MANAGER = ("Lcom/android/settings/accessibility/AccessibilityHearingAidPreferenceController;"
                        "->setFragmentManager")
FRAGMENT_USE = "Lcom/android/settings/dashboard/DashboardFragment;->use"

# The part of Android 16's accessibility screen that matters here: the hearing devices entry,
# beside a preference in its category that stays.
ACCESSIBILITY_SCREEN = preference("PreferenceScreen", "accessibility_settings_screen", children=[
    preference("PreferenceCategory", "audio_category", children=[
        preference("Preference", "flash_notifications_preference_accessibility",
                   "com.android.settings.accessibility.FlashNotificationsPreferenceController"),
        preference("Preference", "hearing_aid_preference", HEARING_AID),
    ]),
])


def uleb128(value):
    out = b""
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out += bytes([byte | 0x80])
        else:
            return out + bytes([byte])


def dex_file(methods, code):
    """A dex file with the methods [(class, name, shorty)] it refers to, and the code
    {method index: (registers, instructions)} of those it defines, laid out as d8 lays it out:
    the header, the id tables, then the data."""
    strings, types, protos = [], [], []

    def string(text):
        if text not in strings:
            strings.append(text)
        return strings.index(text)

    def type_(descriptor):
        string(descriptor)
        if descriptor not in types:
            types.append(descriptor)
        return types.index(descriptor)

    def proto(shorty):
        string(shorty)
        type_(shorty[0])
        if shorty not in protos:
            protos.append(shorty)
        return protos.index(shorty)

    method_ids = [(type_(cls), proto(shorty), string(name)) for cls, name, shorty in methods]
    classes = {}
    for index in sorted(code):
        classes.setdefault(method_ids[index][0], []).append(index)

    string_off = 0x70
    type_off = string_off + 4 * len(strings)
    proto_off = type_off + 4 * len(types)
    method_off = proto_off + 12 * len(protos)
    class_off = method_off + 8 * len(method_ids)
    data_off = class_off + 32 * len(classes)

    data = b""
    string_data = []
    for text in strings:
        string_data.append(data_off + len(data))
        data += uleb128(len(text)) + text.encode() + b"\0"
    data += b"\0" * (-len(data) % 4)
    code_offs = {}
    for index, (registers, instructions) in sorted(code.items()):
        code_offs[index] = data_off + len(data)
        data += struct.pack("<HHHHII", registers, 0, 2, 0, 0, len(instructions) // 2) + instructions
        data += b"\0" * (-len(data) % 4)
    class_data = {}
    for cls, indices in classes.items():
        class_data[cls] = data_off + len(data)
        data += uleb128(0) + uleb128(0) + uleb128(len(indices)) + uleb128(0)
        previous = 0
        for index in indices:
            data += uleb128(index - previous) + uleb128(0x9) + uleb128(code_offs[index])
            previous = index

    tables = b"".join(struct.pack("<I", offset) for offset in string_data)
    tables += b"".join(struct.pack("<I", strings.index(t)) for t in types)
    tables += b"".join(struct.pack("<III", strings.index(s), types.index(s[0]), 0) for s in protos)
    tables += b"".join(struct.pack("<HHI", *ids) for ids in method_ids)
    tables += b"".join(struct.pack("<8I", cls, 1, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, class_data[cls], 0)
                       for cls in classes)
    size = data_off + len(data)
    header = struct.pack("<8sI20s20I", b"dex\n035\0", 0, bytes(20), size, 0x70, 0x12345678, 0, 0, 0,
                         len(strings), string_off, len(types), type_off, len(protos), proto_off,
                         0, 0, len(method_ids), method_off, len(classes), class_off,
                         len(data), data_off)
    dex = bytearray(header + tables + data)
    dex[12:32] = hashlib.sha1(bytes(dex[32:])).digest()
    dex[8:12] = struct.pack("<I", zlib.adler32(bytes(dex[12:])))
    return bytes(dex)


def settings_dex():
    """The two methods of Android 16's Settings.apk that dereference the missing adapter, as
    d8 compiled them, cut down to the instructions that matter."""
    methods = [
        ("Lcom/android/settingslib/bluetooth/BluetoothUtils;", "isAudioSharingSupported", "Z"),
        ("Landroid/bluetooth/BluetoothAdapter;", "getDefaultAdapter", "L"),
        ("Landroid/bluetooth/BluetoothAdapter;", "isLeAudioBroadcastSourceSupported", "I"),
        ("Lcom/android/settings/accessibility/AccessibilitySettings;", "onAttach", "VL"),
        ("Lcom/android/settings/dashboard/DashboardFragment;", "onAttach", "VL"),
        ("Lcom/android/settings/dashboard/DashboardFragment;", "use", "LL"),
        ("Landroidx/fragment/app/Fragment;", "getFragmentManager", "L"),
        ("Lcom/android/settings/accessibility/AccessibilityHearingAidPreferenceController;",
         "setFragmentManager", "VL"),
    ]
    audio_sharing_supported = bytes.fromhex(
        "71000100 0000"  # invoke-static {}, BluetoothAdapter.getDefaultAdapter
        "0c00"  # move-result-object v0
        "1201"  # const/4 v1, 0
        "6e100200 0000"  # invoke-virtual {v0}, isLeAudioBroadcastSourceSupported
        "0a02"  # move-result v2
        "0f02"  # return v2
    )
    on_attach = bytes.fromhex(
        "6f200400 1000"  # invoke-super {v0, v1}, DashboardFragment.onAttach
        "1c010000"  # const-class v1, AccessibilityHearingAidPreferenceController
        "6e200500 1000"  # invoke-virtual {v0, v1}, use
        "0c01"  # move-result-object v1
        "6e100600 0000"  # invoke-virtual {v0}, getFragmentManager
        "0c00"  # move-result-object v0
        "6e200700 0100"  # invoke-virtual {v1, v0}, setFragmentManager
        "0e00"  # return-void
    )
    return dex_file(methods, {0: (3, audio_sharing_supported), 3: (2, on_attach)})


def dex_code(dex, method):
    """The instructions of a method a dex file defines."""
    defined, _ = make_image.dex_methods(dex)
    (code_off, _), = defined[method]
    count = struct.unpack_from("<I", dex, code_off + 12)[0]
    return dex[code_off + 16:code_off + 16 + 2 * count]


class TouchLatencyTests(unittest.TestCase):
    """What the image changes so a press shows sooner, and what it logs to measure the rest."""

    def _modify(self, root, extra=None, **kw):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
            archive.writestr("META/filesystem_config.txt", "system/bin/sh 0 2000 755 capabilities=0x0\n")
            archive.writestr("META/apkcerts.txt", 'name="Settings.apk" certificate="c.x509.pem" private_key="c.pk8"\n')
            archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
            for name, data in (extra or {}).items():
                archive.writestr(name, data)
        modified = root / "modified.zip"
        make_image.modify(source, modified, **kw)
        return zipfile.ZipFile(modified)

    def test_fr5_a_press_on_a_list_row_is_not_held_back_by_the_tap_timeout(self):
        # Settings' rows sit in a RecyclerView, so View.onTouchEvent posts setPressed after
        # config_tapTimeoutMillis (100 ms) instead of calling it on the press.
        self.assertEqual(make_image.FRAMEWORK_OVERLAY_INTEGERS, {"config_tapTimeoutMillis": 0})
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_image.framework_overlay_sources(root / "src")
            manifest = (root / "src/AndroidManifest.xml").read_text()
            values = (root / "src/res/values/dsa.xml").read_text()
            self.assertIn('android:targetPackage="android"', manifest)
            self.assertIn('android:isStatic="true"', manifest)
            self.assertIn('android:hasCode="false"', manifest)
            self.assertIn('android:targetSdkVersion="35"', manifest)
            self.assertIn('<integer name="config_tapTimeoutMillis">0</integer>', values)

            overlay = root / "DsaFrameworkOverlay.apk"
            overlay.write_bytes(b"overlay")
            with self._modify(root, framework_overlay=overlay) as archive:
                self.assertEqual(archive.read("VENDOR/overlay/DsaFrameworkOverlay.apk"), b"overlay")
                certs = archive.read("META/apkcerts.txt").decode()
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                contexts = archive.read("VENDOR/etc/selinux/vendor_file_contexts").decode()
            self.assertEqual(certs.count('name="DsaFrameworkOverlay.apk"'), 1)
            self.assertIn('partition="vendor"', certs)
            self.assertIn("vendor/overlay/DsaFrameworkOverlay.apk 0 0 644 capabilities=0x0", fs_config)
            self.assertIn(make_image.VENDOR_OVERLAY_CONTEXT, contexts)

    def test_fr5_an_existing_overlay_label_is_not_specified_twice(self):
        existing = "/(vendor|system/vendor)/overlay(/.*)? u:object_r:vendor_overlay_file:s0\n"
        contexts = make_image.update_vendor_file_contexts(existing)
        self.assertEqual(contexts.count("overlay(/.*)?"), 1)

    def test_fr5_the_overlay_is_built_against_the_images_own_framework(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(make_image, "run") as run:
            root = Path(tmp)
            apk = make_image.build_framework_overlay(Path("/sdk/aapt2"), root / "framework-res.apk", root)
            compile_call, link_call = (call.args for call in run.call_args_list)
            self.assertEqual(compile_call[:2], (Path("/sdk/aapt2"), "compile"))
            self.assertEqual(link_call[:2], (Path("/sdk/aapt2"), "link"))
            self.assertIn(root / "framework-res.apk", link_call)
            self.assertEqual(link_call[link_call.index("-o") + 1], apk)
            self.assertEqual(link_call[-1], compile_call[compile_call.index("-o") + 1])

    def test_fr5_without_aapt2_the_image_has_no_overlay(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            self.assertNotIn("VENDOR/overlay/DsaFrameworkOverlay.apk", archive.namelist())
            self.assertNotIn("DsaFrameworkOverlay", archive.read("META/apkcerts.txt").decode())

    def test_fr5_aapt2_is_found_in_the_newest_build_tools(self):
        with tempfile.TemporaryDirectory() as tmp:
            sdk = Path(tmp)
            for version in ["34.0.0", "36.0.0", "9.0.0"]:
                (sdk / "build-tools" / version).mkdir(parents=True)
                (sdk / "build-tools" / version / "aapt2").write_text("")
            with patch.dict("os.environ", {"ANDROID_HOME": str(sdk)}, clear=True):
                self.assertEqual(make_image.find_aapt2(), sdk / "build-tools/36.0.0/aapt2")
            with patch.dict("os.environ", {}, clear=True):
                self.assertIsNone(make_image.find_aapt2())

    def test_fr5_the_input_trace_turns_on_main_thread_timing(self):
        rc = (Path(make_image.__file__).with_name("dsa-trace.rc")).read_text()
        self.assertIn("on property:ro.boot.hardware.dsa_input_trace=1\n    setprop log.looper.any.main.slow 16\n", rc)
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            self.assertEqual(archive.read("SYSTEM/etc/init/dsa-trace.rc").decode(), rc)
            self.assertIn("system/etc/init/dsa-trace.rc 0 0 644 capabilities=0x0",
                          archive.read("META/filesystem_config.txt").decode())

    def test_fr5_check_image_rejects_an_image_without_them(self):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            outputs = []
            for complete in [False, True]:
                with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                    props = "ro.debuggable=0\n"
                    archive.writestr("META/apkcerts.txt", "")
                    if complete:
                        archive.writestr("VENDOR/overlay/DsaFrameworkOverlay.apk", b"overlay")
                        archive.writestr("SYSTEM/etc/init/dsa-trace.rc", b"rc")
                    archive.writestr("SYSTEM/build.prop", props)
                outputs.append(subprocess.run(
                    [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                    capture_output=True, text=True,
                ).stdout)
        for missing in ["DsaFrameworkOverlay.apk is missing", "dsa-trace.rc is missing"]:
            self.assertIn(missing, outputs[0])
            self.assertNotIn(missing, outputs[1])


if __name__ == "__main__":
    unittest.main()
