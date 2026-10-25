"""Tests for the check that the guest Vulkan driver can load on the image it ships in."""

import importlib.util
import unittest
from pathlib import Path


SPEC = importlib.util.spec_from_file_location(
    "check_guest_driver", Path(__file__).with_name("check-guest-driver.py")
)
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)


def library(needed=(), defined=(), undefined=(), weak=()):
    return check.Library(needed=list(needed), defined=set(defined), undefined=set(undefined), weak=set(weak))


class GuestDriverCheckTests(unittest.TestCase):
    def test_pr3_guest_vulkan_driver_imports_resolve_against_the_image(self):
        driver = library(
            needed=["libc.so", "libnativewindow.so"],
            defined={"HMI"},
            undefined={"malloc", "AHardwareBuffer_lock"},
        )
        image = {
            "libc.so": library(defined={"malloc"}),
            "libnativewindow.so": library(defined={"AHardwareBuffer_lock"}),
        }
        self.assertEqual(check.problems(driver, image), [])

    def test_pr3_guest_vulkan_driver_with_an_unresolved_import_is_rejected(self):
        driver = library(
            needed=["libc.so"],
            defined={"HMI"},
            undefined={"malloc", "renderControlGetTransport"},
        )
        image = {"libc.so": library(defined={"malloc"})}
        self.assertEqual(
            check.problems(driver, image),
            ["renderControlGetTransport is not defined by any library the driver loads"],
        )

    def test_pr3_guest_vulkan_driver_built_against_another_libcxx_is_rejected(self):
        ndk_string = "_ZN7android4base11GetPropertyERKNSt6__ndk112basic_stringIcEES5_"
        platform_string = "_ZN7android4base11GetPropertyERKNSt3__112basic_stringIcEES5_"
        driver = library(needed=["libbase.so"], defined={"HMI"}, undefined={ndk_string})
        image = {"libbase.so": library(defined={platform_string})}
        self.assertEqual(
            check.problems(driver, image),
            [f"{ndk_string} is not defined by any library the driver loads"],
        )

    def test_pr3_guest_vulkan_driver_needs_a_library_the_image_has(self):
        driver = library(needed=["libmissing.so"], defined={"HMI"})
        self.assertEqual(check.problems(driver, {}), ["the image has no libmissing.so"])

    def test_pr3_guest_vulkan_driver_must_export_the_hal_module(self):
        driver = library(defined={"vk_icdGetInstanceProcAddr"})
        self.assertEqual(
            check.problems(driver, {}),
            ["the driver does not export HMI, the module the Android Vulkan loader looks up"],
        )

    def test_pr3_weak_imports_may_stay_unresolved(self):
        driver = library(defined={"HMI"}, undefined={"__cxa_thread_atexit_impl"}, weak={"__cxa_thread_atexit_impl"})
        self.assertEqual(check.problems(driver, {}), [])

    def test_pr3_imports_resolve_through_dependencies_of_dependencies(self):
        driver = library(needed=["libcutils.so"], defined={"HMI"}, undefined={"__android_log_print"})
        image = {
            "libcutils.so": library(needed=["liblog.so"]),
            "liblog.so": library(defined={"__android_log_print"}),
        }
        self.assertEqual(check.problems(driver, image), [])


if __name__ == "__main__":
    unittest.main()
