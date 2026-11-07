"""Tests for assemble-digitalis.py with a fake build output and fake images."""

import importlib.util
import io
import stat
import tempfile
import unittest
import zipfile
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("assemble", Path(__file__).with_name("assemble-digitalis.py"))
assemble = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(assemble)

ARTIFACTS = [
    "system/bin/arm64/app_process64",
    "system/bin/arm64/linker64",
    "system/bin/berberis_program_runner_arm64",
    "system/etc/binfmt_misc/arm64_exe",
    "system/etc/ld.config.arm64.txt",
    "system/lib64/libberberis_arm64.so",
    "system/lib64/libberberis_proxy_libEGL.so",
    "system/lib64/arm64/libc.so",
    "system/lib64/arm64/libEGL.so",
    "system/lib64/arm64/libicuuc.so",
    "system/lib64/arm64/libcrypto.so",
    "system/lib64/arm64/libsqlite.so",
]
CONFIG_MK = """# region digitalis
BERBERIS_PRODUCT_PACKAGES_ARM64_TO_X86_64 := \\
    libberberis_arm64

BERBERIS_DISTRIBUTION_ARTIFACTS_ARM64 := \\
""" + "".join(f"    {a} \\\n" for a in ARTIFACTS[:-1]) + f"    {ARTIFACTS[-1]}\n# endregion\n\n" + \
    "BERBERIS_DISTRIBUTION_ARTIFACTS_RISCV64 := \\\n    system/lib64/libberberis_riscv64.so\n"


def elf(machine):
    header = bytearray(64)
    header[:4] = b"\x7fELF"
    header[18:20] = machine.to_bytes(2, "little")
    return bytes(header)


def write_build(root):
    config = Path(root) / "berberis_config.mk"
    config.write_text(CONFIG_MK)
    product = Path(root) / "out/target/product/dsa_translator"
    for rel in ARTIFACTS:
        if assemble.from_image(rel):
            continue
        path = product / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        if rel.startswith(assemble.GUEST_DIRS):
            path.write_bytes(elf(assemble.EM_AARCH64))
        elif rel.startswith("system/etc/"):
            path.write_text(rel)
        else:
            path.write_bytes(elf(assemble.EM_X86_64))
    return config, product


def fake_apex(payload, compressed=False):
    inner = io.BytesIO()
    with zipfile.ZipFile(inner, "w") as z:
        z.writestr("apex_manifest.pb", b"")
        z.writestr("apex_payload.img", payload)
    if not compressed:
        return inner.getvalue()
    outer = io.BytesIO()
    with zipfile.ZipFile(outer, "w") as z:
        z.writestr("original_apex", inner.getvalue())
    return outer.getvalue()


def fake_target_files(root, machine):
    """A target_files with SQLite in lib64, ICU in a compressed APEX, BoringSSL in an APEX
    reached through a symbolic link, and the framework libraries in lib64."""
    path = Path(root) / f"target_files-{machine}.zip"
    payloads = {}
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("SYSTEM/lib64/libsqlite.so", elf(machine))
        for name in assemble.FRAMEWORK_LIBS:
            if assemble.FRAMEWORK_LIBS[name] is None:
                z.writestr(f"SYSTEM/lib64/{name}", elf(machine))
        link = zipfile.ZipInfo("SYSTEM/lib64/libcrypto.so")
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        z.writestr(link, "/apex/com.android.conscrypt/lib64/libcrypto.so")
        for apex, compressed in [("com.android.i18n", True), ("com.android.conscrypt", False),
                                 ("com.android.neuralnetworks", False)]:
            payload = f"payload of {apex}".encode()
            payloads[payload] = apex
            suffix = ".capex" if compressed else ".apex"
            z.writestr(f"SYSTEM/apex/{apex}{suffix}", fake_apex(payload, compressed))

    def reader(image, inner_path):
        # Stands in for debugfs and fsck.erofs: every APEX holds every library asked of it.
        assert image in payloads, image
        assert inner_path.startswith("lib64/"), inner_path
        return elf(machine)

    return path, reader


class AssembleTests(unittest.TestCase):
    def test_fr25_the_build_makes_every_bundle_file_except_the_image_s_guest_libraries(self):
        with tempfile.TemporaryDirectory() as tmp:
            config, _ = write_build(tmp)
            targets = assemble.build_targets(config, "out/target/product/dsa_translator/")
            self.assertIn("out/target/product/dsa_translator/system/lib64/libberberis_arm64.so", targets)
            self.assertIn("out/target/product/dsa_translator/system/lib64/arm64/libc.so", targets)
            self.assertIn("out/target/product/dsa_translator/system/lib64/arm64/libEGL.so", targets)
            for name in ("libicuuc.so", "libcrypto.so", "libsqlite.so"):
                self.assertNotIn(f"out/target/product/dsa_translator/system/lib64/arm64/{name}", targets)
            self.assertFalse(any("riscv64" in t for t in targets))
            self.assertEqual(len(targets), len(ARTIFACTS) - 3)

    def test_fr25_the_replaced_guest_libraries_are_never_taken_from_the_image(self):
        # The image's own libc or libEGL would run the real library instead of the bridge's.
        for name in ("libc.so", "libm.so", "libdl.so", "libEGL.so", "libvulkan.so", "libandroid.so"):
            self.assertNotIn(name, assemble.GUEST_LIBS_FROM_IMAGE)

    def test_fr25_guest_and_framework_libraries_come_out_of_lib64_and_apexes(self):
        with tempfile.TemporaryDirectory() as tmp:
            arm64, reader = fake_target_files(tmp, assemble.EM_AARCH64)
            assemble.take_libraries(arm64, assemble.GUEST_LIBS_FROM_IMAGE, assemble.EM_AARCH64,
                                    Path(tmp) / "guest", reader)
            self.assertEqual(sorted(p.name for p in (Path(tmp) / "guest").iterdir()),
                             sorted(assemble.GUEST_LIBS_FROM_IMAGE))
            x86_64, reader = fake_target_files(tmp, assemble.EM_X86_64)
            assemble.take_libraries(x86_64, assemble.FRAMEWORK_LIBS, assemble.EM_X86_64,
                                    Path(tmp) / "framework", reader)
            self.assertEqual(len(list((Path(tmp) / "framework").iterdir())), len(assemble.FRAMEWORK_LIBS))
            # An image of the other architecture is refused, so a wrong pin cannot slip through.
            with self.assertRaises(SystemExit):
                assemble.take_libraries(x86_64, assemble.GUEST_LIBS_FROM_IMAGE, assemble.EM_AARCH64,
                                        Path(tmp) / "wrong", reader)

    def test_fr25_the_bundle_has_every_artifact_with_the_right_architecture(self):
        with tempfile.TemporaryDirectory() as tmp:
            config, product = write_build(tmp)
            guest = Path(tmp) / "guest"
            guest.mkdir()
            for name in assemble.GUEST_LIBS_FROM_IMAGE:
                (guest / name).write_bytes(elf(assemble.EM_AARCH64))
            license_file = Path(tmp) / "LICENSE"
            license_file.write_text("Apache License")
            out = Path(tmp) / "bundle"
            taken = assemble.collect(config, product, guest, out, license_file=license_file)
            self.assertEqual(taken, {"build": len(ARTIFACTS) - 3, "image": 3})
            for rel in ARTIFACTS:
                self.assertTrue((out / rel).is_file(), rel)
            self.assertEqual((out / "system/etc/digitalis/LICENSE").read_text(), "Apache License")
            self.assertEqual(assemble.elf_machine((out / "system/lib64/arm64/libicuuc.so").read_bytes()),
                             assemble.EM_AARCH64)

    def test_fr25_a_missing_or_wrong_architecture_file_stops_the_bundle(self):
        with tempfile.TemporaryDirectory() as tmp:
            config, product = write_build(tmp)
            guest = Path(tmp) / "guest"
            guest.mkdir()
            for name in assemble.GUEST_LIBS_FROM_IMAGE:
                (guest / name).write_bytes(elf(assemble.EM_AARCH64))
            (product / "system/lib64/arm64/libc.so").unlink()
            with self.assertRaises(SystemExit) as raised:
                assemble.collect(config, product, guest, Path(tmp) / "a")
            self.assertIn("system/lib64/arm64/libc.so", str(raised.exception))
            (product / "system/lib64/arm64/libc.so").write_bytes(elf(assemble.EM_X86_64))
            with self.assertRaises(SystemExit):
                assemble.collect(config, product, guest, Path(tmp) / "b")
            (product / "system/lib64/arm64/libc.so").write_bytes(elf(assemble.EM_AARCH64))
            (product / "system/lib64/libberberis_arm64.so").write_bytes(elf(assemble.EM_AARCH64))
            with self.assertRaises(SystemExit):
                assemble.collect(config, product, guest, Path(tmp) / "c")


if __name__ == "__main__":
    unittest.main()
