"""Tests that every import of a gralloc buffer from its handle passes a method the platform
takes. AHardwareBuffer_createFromHandle refuses any method but REGISTER and CLONE with
BAD_VALUE, before the mapper sees the buffer and without a log line. The hwcomposer passed 1
for "clone", so nothing a CPU wrote into a buffer (a video decoder's pictures, a camera's
frames) was ever copied up to the host, and every video in an app's SurfaceView was green.
The shared numbers are in include/dsa_hardware_buffer_import.h, tested with the hwcomposer's
rules in dsa/guest/hwcomposer/tests/cpu_upload_policy_test.cpp."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
GUEST = HERE.parent / "guest"
HEADER = HERE / "include/dsa_hardware_buffer_import.h"
# The platform's own header, as this repository keeps it for the guest driver's build.
PLATFORM = HERE / "cmake/include/vndk/hardware_buffer.h"


def platform_methods():
    text = PLATFORM.read_text(encoding="utf-8")
    return {
        name: int(value)
        for name, value in re.findall(
            r"AHARDWAREBUFFER_CREATE_FROM_HANDLE_METHOD_(\w+)\s*=\s*(\d+)", text
        )
    }


class HardwareBufferImportTest(unittest.TestCase):
    def test_fr26_the_shared_methods_are_the_platforms_numbers(self):
        methods = platform_methods()
        self.assertEqual(methods, {"REGISTER": 2, "CLONE": 3})
        header = HEADER.read_text(encoding="utf-8")
        self.assertRegex(header, rf"kAhbCreateFromHandleRegister = {methods['REGISTER']};")
        self.assertRegex(header, rf"kAhbCreateFromHandleClone = {methods['CLONE']};")

    def test_fr26_no_guest_component_spells_a_method_of_its_own(self):
        calls = []
        # The components' own sources, not the trees a build fetches beside them.
        for source in sorted(GUEST.glob("*/src/*.cpp")):
            text = source.read_text(encoding="utf-8", errors="replace")
            # A call through the function or a pointer to it, up to its closing parenthesis.
            for match in re.finditer(r"(?:AHardwareBuffer_createFromHandle|createFromHandle)\(([^;]*?)\);", text):
                args = [arg.strip() for arg in match.group(1).split(",")]
                # A declaration names types; a call passes values.
                if len(args) != 4 or args[0].startswith("const "):
                    continue
                calls.append((source.relative_to(GUEST).as_posix(), args[2]))
            # A constant of its own standing in for the platform's.
            self.assertNotRegex(
                text,
                r"constexpr\s+int32_t\s+kCreateFromHandle\w*\s*=",
                f"{source.name} numbers an import method itself",
            )
        files = {name for name, _ in calls}
        self.assertIn("hwcomposer/src/cpu_upload.cpp", files)
        self.assertIn("camera/src/graphic_buffer.cpp", files)
        for name, method in calls:
            self.assertRegex(
                method,
                r"^(?:dsa::kAhbCreateFromHandle(?:Clone|Register)|import\.method|"
                r"AHARDWAREBUFFER_CREATE_FROM_HANDLE_METHOD_(?:CLONE|REGISTER))$",
                f"{name} passes {method!r} as the import method",
            )


if __name__ == "__main__":
    unittest.main()
