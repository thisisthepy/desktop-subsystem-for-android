"""Regression tests for the guest image layout helpers."""

import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("make_image", Path(__file__).with_name("make-image.py"))
make_image = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(make_image)


class ImageLayoutTests(unittest.TestCase):
    def test_fr1_verified_boot_partition_needs_a_first_stage_entry(self):
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


if __name__ == "__main__":
    unittest.main()
