"""Regression tests for the guest image layout helpers."""

import importlib.util
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


if __name__ == "__main__":
    unittest.main()
