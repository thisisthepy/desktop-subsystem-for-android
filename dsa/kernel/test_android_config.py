"""Regression tests for the Android-specific guest kernel configuration."""

import unittest
from pathlib import Path


CONFIG = Path(__file__).with_name("android.config")


class AndroidKernelConfigTests(unittest.TestCase):
    def test_fr1_binder_device_nodes_are_created_for_android_init(self):
        config = CONFIG.read_text()
        self.assertIn('CONFIG_ANDROID_BINDER_DEVICES="binder,hwbinder,vndbinder"', config)


if __name__ == "__main__":
    unittest.main()
