"""Regression tests for the Android-specific guest kernel configuration."""

import unittest
from pathlib import Path


CONFIG = Path(__file__).with_name("android.config")


class AndroidKernelConfigTests(unittest.TestCase):
    def test_fr1_ashmem_is_off_and_memfd_is_on(self):
        config = CONFIG.read_text()
        self.assertNotIn("CONFIG_ASHMEM", config)
        self.assertIn("CONFIG_MEMFD_CREATE=y", config)

    def test_fr1_binder_device_nodes_are_created_for_android_init(self):
        config = CONFIG.read_text()
        self.assertIn('CONFIG_ANDROID_BINDER_DEVICES="binder,hwbinder,vndbinder"', config)

    def test_fr1_fbe_file_name_encryption_supports_aes_hctr2(self):
        config = CONFIG.read_text()
        self.assertIn("CONFIG_CRYPTO_HCTR2=y", config)

    def test_pr3_hyper_v_socket_transport_is_built_into_the_guest_kernel(self):
        config = CONFIG.read_text()
        self.assertIn("CONFIG_VSOCKETS=y", config)
        self.assertIn("CONFIG_HYPERV_VSOCKETS=y", config)


if __name__ == "__main__":
    unittest.main()
