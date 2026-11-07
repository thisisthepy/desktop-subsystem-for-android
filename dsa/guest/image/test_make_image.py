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
import xml.etree.ElementTree as ET
import zlib
from pathlib import Path
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("make_image", Path(__file__).with_name("make-image.py"))
make_image = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(make_image)


class ImageLayoutTests(unittest.TestCase):
    def test_fr21_channeld_may_clear_the_pstore_records_the_host_collected(self):
        # init leaves /sys/fs/pstore read only to system; the daemon makes it writable to
        # itself and unlinks each collected record, which erases it from the pstore disk.
        cil = (Path(__file__).parent / "sepolicy" / "channeld.cil").read_text()
        self.assertRegex(cil, r"\(allow dsa_channeld pstorefs \(dir \([^)]*\bsetattr\b[^)]*\bremove_name\b")
        self.assertRegex(cil, r"\(allow dsa_channeld pstorefs \(file \([^)]*\bunlink\b")

    # The SELinux type and class of every kernel file dsa-channeld opens, as the guest's kernel
    # labels it. A path the daemon opens that is not listed here fails the test below, so a
    # new read cannot ship without its rule (the refused /proc/vmstat read went unnoticed).
    CHANNELD_KERNEL_PATHS = {
        "/proc/bootconfig": [("proc_bootconfig", "file")],
        "/sys/fs/pstore": [("pstorefs", "dir"), ("pstorefs", "file")],
        "/proc/meminfo": [("proc_meminfo", "file")],
        "/proc/pressure/memory": [("proc_pressure_mem", "file")],
        "/proc/vmstat": [("proc_vmstat", "file")],
        # The process listing and each app process's /proc/<pid>/cmdline.
        "/proc": [("proc", "dir"), ("appdomain", "dir"), ("appdomain", "file")],
        "/proc/{}/cmdline": [("appdomain", "file")],
    }
    # The modes init runs the same binary in, each in a domain of its own.
    CHANNELD_SOURCE_DOMAINS = {
        "share_mount.rs": ("dsa_sharemount", "files.cil"),
        "android_files.rs": ("dsa_files", "files.cil"),
        "webdav.rs": ("dsa_files", "files.cil"),
    }

    def test_nfr2_every_kernel_file_channeld_opens_has_an_allow_rule(self):
        here = Path(__file__).parent
        src = here.parent / "channeld" / "src"
        literal = re.compile(r'"(/(?:proc|sys)(?:/[^"]*)?)"')
        found = 0
        for source in sorted(src.glob("*.rs")):
            domain, policy_file = self.CHANNELD_SOURCE_DOMAINS.get(
                source.name, ("dsa_channeld", "channeld.cil")
            )
            policy = (here / "sepolicy" / policy_file).read_text()
            for number, line in enumerate(source.read_text().splitlines(), 1):
                if line.strip().startswith("//"):
                    continue
                for path in literal.findall(line):
                    if path.startswith("/proc/self/"):
                        # A process's own /proc entries are every domain's to read.
                        continue
                    where = f"{source.name}:{number} opens {path}"
                    self.assertIn(
                        path,
                        self.CHANNELD_KERNEL_PATHS,
                        f"{where}, which this test does not know: add its SELinux type here "
                        f"and an allow rule for {domain} to sepolicy/{policy_file}",
                    )
                    found += 1
                    for target, cls in self.CHANNELD_KERNEL_PATHS[path]:
                        perm = "search" if cls == "dir" else "read"
                        self.assertRegex(
                            policy,
                            rf"\(allow\s+{domain}\s+{target}\s+\(\s*{cls}\s+\([^)]*\b{perm}\b",
                            f"{where}, but sepolicy/{policy_file} does not let {domain} "
                            f"{perm} {target} ({cls})",
                        )
        self.assertGreaterEqual(found, 5, "the scan found none of the daemon's kernel reads")

    # Every folder on the way down to a place on /data the daemon uses, with the label the
    # Android 16 and 17 file_contexts give it. /data/media is media_userdir_file, not the
    # storage's own label; a policy without it refused every request to the file server.
    CHANNELD_DATA_PATHS = {
        "/data/media/0": ["system_data_root_file", "media_userdir_file", "media_rw_data_file"],
        "/data/vendor/dsa/arm-translation": ["system_data_root_file", "vendor_data_file"],
        "/data/vendor/dsa/gapps": ["system_data_root_file", "vendor_data_file"],
    }
    # Paths the daemon names but never opens, so no folder on their way needs a rule. The
    # Google apps mount point is only compared with the mount points /proc/self/mounts lists.
    CHANNELD_NAMED_NOT_OPENED = {"/mnt/dsa-gapps"}

    # What each file system call in the file sharing jobs asks of the storage's label
    # (media_rw_data_file), as {class: permissions}. A call not listed here fails the test, so
    # a new operation cannot ship without its rule.
    STORAGE_CALLS = {
        "dsa_files": {
            "fs::symlink_metadata": {"dir": "getattr", "file": "getattr", "lnk_file": "getattr"},
            "fs::metadata": {"dir": "search getattr", "file": "getattr"},
            ".is_dir": {"dir": "getattr", "file": "getattr"},
            ".exists": {"dir": "getattr", "file": "getattr"},
            "fs::read_link": {"lnk_file": "read"},
            "fs::read_dir": {"dir": "search read open"},
            "File::open": {"file": "read open"},
            "File::create": {"dir": "search write add_name", "file": "create write open"},
            "fs::create_dir": {"dir": "create write add_name"},
            "fs::remove_file": {"dir": "write remove_name", "file": "unlink", "lnk_file": "unlink"},
            "fs::remove_dir_all": {
                "dir": "search read open write remove_name rmdir",
                "file": "unlink",
                "lnk_file": "unlink",
            },
            "fs::rename": {
                "dir": "write add_name remove_name rename reparent",
                "file": "rename",
                "lnk_file": "rename",
            },
            "fs::copy": {"dir": "write add_name", "file": "create read write open getattr setattr"},
            "File::from": {},
        },
        "dsa_sharemount": {
            ".is_dir": {"dir": "getattr"},
            "fs::remove_dir": {"dir": "write remove_name rmdir"},
            "DirBuilder::new": {"dir": "search create write add_name"},
            "chown": {"dir": "setattr"},
            "libc::mount": {"dir": "mounton"},
            "fs::read_to_string": {},
            "File::from": {},
        },
    }

    @staticmethod
    def _production_lines(source):
        """The lines of a Rust source above its test module, without comments."""
        for number, line in enumerate(source.read_text().splitlines(), 1):
            if line.strip().startswith("#[cfg(test)]"):
                return
            if not line.strip().startswith("//"):
                yield number, line

    @staticmethod
    def _granted(policy, domain, target):
        """{class: set of permissions} the policy allows `domain` on `target`."""
        granted = {}
        rule = rf"\(allow\s+{domain}\s+{target}\s+\(\s*(\w+)\s+\(([^)]*)\)\s*\)\s*\)"
        for cls, perms in re.findall(rule, policy):
            granted.setdefault(cls, set()).update(perms.split())
        return granted

    def test_fr29_every_place_on_data_channeld_uses_can_be_reached(self):
        here = Path(__file__).parent
        literal = re.compile(r'"(/(?:data|storage|sdcard|mnt)(?:/[^"]*)?)"')
        found = 0
        for source in sorted((here.parent / "channeld" / "src").glob("*.rs")):
            domain, policy_file = self.CHANNELD_SOURCE_DOMAINS.get(
                source.name, ("dsa_channeld", "channeld.cil")
            )
            policy = (here / "sepolicy" / policy_file).read_text()
            for number, line in self._production_lines(source):
                for path in literal.findall(line):
                    if path in self.CHANNELD_NAMED_NOT_OPENED:
                        continue
                    where = f"{source.name}:{number} uses {path}"
                    known = [p for p in self.CHANNELD_DATA_PATHS if path.startswith(p)]
                    self.assertTrue(
                        known,
                        f"{where}, which this test does not know: add the label of every "
                        f"folder on its way here and allow {domain} to search them in "
                        f"sepolicy/{policy_file}",
                    )
                    found += 1
                    for target in self.CHANNELD_DATA_PATHS[known[0]]:
                        self.assertIn(
                            "search",
                            self._granted(policy, domain, target).get("dir", set()),
                            f"{where}, but sepolicy/{policy_file} does not let {domain} "
                            f"search {target} (dir) on the way",
                        )
        self.assertGreaterEqual(found, 2, "the scan found none of the daemon's places on /data")

    def test_fr29_the_file_sharing_jobs_are_allowed_exactly_the_storage_calls_they_make(self):
        here = Path(__file__).parent
        policy = (here / "sepolicy" / "files.cil").read_text()
        call = re.compile(r"(\b(?:fs|File|DirBuilder|libc)::\w+|\.is_dir|\.exists|\bchown)\(")
        for domain, calls in self.STORAGE_CALLS.items():
            needed = {}
            sources = [s for s, (d, _) in self.CHANNELD_SOURCE_DOMAINS.items() if d == domain]
            for name in sources:
                source = here.parent / "channeld" / "src" / name
                for number, line in self._production_lines(source):
                    if line.lstrip().startswith("use "):
                        continue
                    for used in call.findall(line):
                        if used.startswith("libc::") and used != "libc::mount":
                            # Sockets and unmounts, which other rules cover.
                            continue
                        self.assertIn(
                            used,
                            calls,
                            f"{name}:{number} calls {used}, which this test does not know: say "
                            f"what it asks of media_rw_data_file and allow it to {domain} in "
                            "sepolicy/files.cil",
                        )
                        for cls, perms in calls[used].items():
                            needed.setdefault(cls, set()).update(perms.split())
            granted = self._granted(policy, domain, "media_rw_data_file")
            granted.pop("filesystem", None)
            for cls in sorted(set(needed) | set(granted)):
                self.assertEqual(
                    granted.get(cls, set()),
                    needed.get(cls, set()),
                    f"sepolicy/files.cil gives {domain} on media_rw_data_file ({cls}) other "
                    "permissions than its calls need",
                )

    def test_fr1_selects_vendor_apexes_without_the_cuttlefish_launcher(self):
        cmdline = make_image.kernel_cmdline(f"blkdevparts={make_image.SYSTEM_DISK_GUID}:1@0(misc)")
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
            f"blkdevparts={make_image.SYSTEM_DISK_GUID}:1048576@1048576(misc:811d3bbb-a7ff-437a-857f-f54af996b190),"
            "1048576@69206016(vbmeta_a),1610612736@104857600(super)",
        )

    def test_fr1_the_system_disk_is_named_by_its_gpt_guid_not_its_probe_order(self):
        # Hyper-V's SCSI controller probes disks in an order that changes when the host
        # attaches another (the pstore disk), so sda is not always the system disk.
        cmdline = make_image.command_line_partitions({"super": (1 << 20, 1 << 30)})
        self.assertTrue(cmdline.startswith(f"blkdevparts={make_image.SYSTEM_DISK_GUID}:"))
        self.assertEqual(make_image.kernel_disk_names(cmdline), [])
        self.assertRegex(make_image.SYSTEM_DISK_GUID,
                         r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
        self.assertNotIn(make_image.SYSTEM_DISK_GUID, (make_image.SUPER_UUID, make_image.MISC_UUID))

    def test_fr1_the_system_disk_gpt_carries_the_guid_the_command_line_names(self):
        calls = []
        rows = [
            f"   {n}   {2048 * n}   {2048 * n + 2047}   1.0 MiB   8300  {name}"
            for n, (name, _) in enumerate(make_image.SYSTEM_SMALL_PARTITIONS + [("super", 0)], start=1)
        ]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            images = root / "images"
            images.mkdir()
            (images / "super.img").write_bytes(bytes(4096))

            def fake_run(*args, **kwargs):
                calls.append([str(a) for a in args])

            def fake_subprocess_run(cmd, *args, **kwargs):
                return subprocess.CompletedProcess(cmd, 0, stdout="\n".join(rows), stderr="")

            with patch.object(make_image, "run", side_effect=fake_run), \
                 patch.object(make_image, "make_raw_disk", side_effect=lambda path, size: path.write_bytes(b"")), \
                 patch.object(make_image, "materialize_backup_gpt"), \
                 patch.object(make_image, "verified_boot_partitions", return_value=[]), \
                 patch.object(make_image.subprocess, "run", side_effect=fake_subprocess_run):
                cmdline = make_image.system_disk(images, root, root, root / "system.vhdx")
        sgdisk = next(c for c in calls if "--clear" in c)
        self.assertIn(f"--disk-guid={make_image.SYSTEM_DISK_GUID}", sgdisk)
        self.assertTrue(cmdline.startswith(f"blkdevparts={make_image.SYSTEM_DISK_GUID}:"))

    def test_fr1_kernel_disk_names_are_found_and_stable_identities_are_not(self):
        found = make_image.kernel_disk_names(
            "/dev/block/sda1 /x ext4 defaults wait root=/dev/sdb2 blkdevparts=sdc:1@0(misc) /dev/vda")
        self.assertEqual(found, ["/dev/block/sda1", "/dev/sdb2", "blkdevparts=sdc:", "/dev/vda"])
        self.assertEqual(make_image.kernel_disk_names(
            "/dev/block/by-name/userdata /dev/block/by-uuid/x /dev/block/zram0 /dev/block/dm-0 /sdcard "
            f"blkdevparts={make_image.SYSTEM_DISK_GUID}:1@0(misc)"), [])

    def test_fr1_no_mount_table_or_init_script_names_a_disk_by_probe_order(self):
        here = Path(SPEC.origin).parent
        for path in [here / "fstab.dsa", *sorted(here.glob("*.rc"))]:
            self.assertEqual(make_image.kernel_disk_names(path.read_text()), [], path.name)
        cmdline = make_image.kernel_cmdline(make_image.command_line_partitions({"super": (1 << 20, 1 << 30)}))
        self.assertEqual(make_image.kernel_disk_names(cmdline), [])

    def test_fr1_the_builds_own_mount_tables_lose_their_entries_that_name_a_disk_by_probe_order(self):
        table = ("system /system erofs ro wait,logical,first_stage_mount\n"
                 "/dev/block/vdc1 /sdcard vfat defaults voldmanaged=sdcard:auto\n"
                 "/dev/block/by-name/userdata /data f2fs noatime wait\n")
        edited = make_image.without_kernel_disk_entries(table)
        self.assertEqual(make_image.kernel_disk_names(edited), [])
        self.assertIn("/dev/block/by-name/userdata /data", edited)
        self.assertIn("system /system erofs", edited)

    def test_fr1_check_image_rejects_a_disk_named_by_probe_order(self):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (keys_dir / "test.x509.pem").write_text("-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n")
            manifest = {"files": {}, "kernel_cmdline": "console=ttyS0 blkdevparts=sda:1048576@1048576(misc)"}
            (img_dir / "image.json").write_text(json.dumps(manifest))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                archive.writestr("META/apkcerts.txt", "")
                archive.writestr("VENDOR/etc/fstab.dsa", "/dev/block/sdb3 /data ext4 defaults wait\n")
                archive.writestr("VENDOR/etc/init/disk.rc", "on fs\n    write /dev/block/sdc 0\n")
            proc = subprocess.run(
                [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                capture_output=True,
                text=True,
            )
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("the kernel command line names a disk by its probe order (blkdevparts=sda:)", proc.stdout)
        self.assertIn("system disk by its GUID", proc.stdout)
        self.assertIn("VENDOR/etc/fstab.dsa names a disk by its probe order (/dev/block/sdb3)", proc.stdout)
        self.assertIn("VENDOR/etc/init/disk.rc names a disk by its probe order (/dev/block/sdc)", proc.stdout)

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

    def test_fr3_the_agent_reaches_the_control_channel_in_android_17s_per_sdk_app_domain(self):
        rules = make_image.CHANNELD_POLICY.read_text()
        android17 = "(type platform_app)\n(type platform_app_36)\n"
        patched = make_image.with_versioned_app_domains(rules, android17)
        self.assertIn("(allow platform_app_36 dsa_channel_service (service_manager (find)))", patched)
        self.assertIn("(allow platform_app dsa_channel_service (service_manager (find)))", patched)
        self.assertNotIn("platform_app_36_36", patched)
        # Android 16 has no such domain, so nothing is added that its policy cannot compile.
        self.assertEqual(make_image.with_versioned_app_domains(rules, "(type platform_app)\n"), rules)

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

    def test_pr3_system_server_can_reach_the_host_renderer(self):
        # system_server draws the pointer location overlay and its dialogs with HWUI, through
        # ANGLE and the Vulkan driver. Without vsock its RenderThread aborts with
        # EGL_NOT_INITIALIZED and the framework restarts in a loop.
        plat_cil = "(type system_server)\n(type gpu_device)\n"
        patched = make_image.patch_plat_sepolicy(plat_cil, make_image.SEPOLICY_FRAGMENT.read_text())
        rule = next((l for l in patched.splitlines()
                     if l.startswith("(allow system_server self (vsock_socket")), None)
        self.assertIsNotNone(rule, "system_server has no vsock rule")
        self.assertIn("connect", rule)
        self.assertIn("(allow system_server gpu_device (chr_file (open read write ioctl getattr map)))", patched)

    def test_pr3_check_image_rejects_a_system_server_without_vsock(self):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (keys_dir / "test.x509.pem").write_text("-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n")
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                archive.writestr("META/apkcerts.txt", "")
                archive.writestr(
                    "SYSTEM/etc/selinux/plat_sepolicy.cil",
                    "(allow appdomain self (vsock_socket (create connect read write)))\n",
                )
            proc = subprocess.run(
                [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("does not allow system_server to connect vsock_socket", proc.stdout)
            self.assertNotIn("does not allow appdomain to connect", proc.stdout)

    def test_pr3_vendor_init_may_check_the_drm_cards(self):
        # The cards are relabelled gpu_device; Cuttlefish's init script waits for card0 as
        # vendor_init, and a refused stat makes the wait time out after five seconds.
        patched = make_image.patch_plat_sepolicy("(type vendor_init)\n(type gpu_device)\n",
                                                 make_image.SEPOLICY_FRAGMENT.read_text())
        self.assertIn("(allow vendor_init gpu_device (chr_file (getattr)))", patched)

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
        cmdline = make_image.kernel_cmdline(f"blkdevparts={make_image.SYSTEM_DISK_GUID}:1@0(misc)")
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
        cmdline = make_image.kernel_cmdline(f"blkdevparts={make_image.SYSTEM_DISK_GUID}:1@0(misc)")
        self.assertIn("androidboot.hardware.hwcomposer.mode=noop", cmdline)
        self.assertIn("androidboot.hardware.hwcomposer.display_finder_mode=noop", cmdline)

    def test_fr1_lights_hal_listens_on_an_unprivileged_vsock_port(self):
        # Without a port the Cuttlefish lights HAL binds vsock port 0, which needs
        # CAP_NET_BIND_SERVICE, and aborts; system_server waits for it and never boots.
        cmdline = make_image.kernel_cmdline(f"blkdevparts={make_image.SYSTEM_DISK_GUID}:1@0(misc)")
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

    NETWORK_FILES = {
        "VENDOR/build.prop": b"ro.vendor.wifi_impl=mac80211_hwsim_virtio\nro.product.vendor.name=cf\n",
        "VENDOR/etc/init/hw/init.vendor.rc":
            b"on post-fs-data && property:ro.vendor.wifi_impl=virt_wifi\n    start setup_wifi\n",
    }

    @staticmethod
    def wifi_apex(service):
        # Cuttlefish's Wi-Fi APEX: setup_wifi and its init service are in the payload image.
        out = io.BytesIO()
        with zipfile.ZipFile(out, "w") as apex:
            apex.writestr("apex_payload.img", b"\0" * 64 + service + b"\0" * 64)
        return out.getvalue()

    def network_files(self):
        files = dict(self.NETWORK_FILES)
        files["VENDOR/apex/com.google.cf.wifi.apex"] = self.wifi_apex(
            b"service setup_wifi /apex/com.google.cf.wifi/bin/setup_wifi --interface=${ro.vendor.virtwifi.port}")
        files["VENDOR/build.prop"] = make_image.edit_props(
            files["VENDOR/build.prop"].decode(), vendor=True, network=True).encode()
        return files

    def test_fr28_the_vendor_props_make_eth0_android_wifi(self):
        props = make_image.edit_props(self.NETWORK_FILES["VENDOR/build.prop"].decode(),
                                      vendor=True, network=True)
        self.assertIn("ro.vendor.wifi_impl=virt_wifi\n", props)
        self.assertIn("ro.vendor.virtwifi.port=eth0\n", props)
        self.assertIn("ro.vendor.disable_rename_eth0=1\n", props)
        self.assertNotIn("mac80211_hwsim_virtio", props)
        # Other prop files do not take vendor properties.
        self.assertNotIn("ro.vendor.wifi_impl", make_image.edit_props("ro.x=1\n"))

    def test_fr28_the_kernel_cmdline_gives_setup_wifi_its_mac_prefix_and_no_static_address(self):
        cmdline = make_image.kernel_cmdline(f"blkdevparts={make_image.SYSTEM_DISK_GUID}:1@0(misc)")
        self.assertIn(f"androidboot.wifi_mac_prefix={make_image.WIFI_MAC_PREFIX}", cmdline.split())
        self.assertNotIn("auto_eth_guest_addr", cmdline)

    def test_fr28_the_image_checker_accepts_dhcp_wifi(self):
        files = self.network_files()
        cmdline = make_image.kernel_cmdline("blkdevparts=x:1@0(misc)")
        self.assertEqual([], make_image.network_problems(list(files), files.__getitem__, cmdline))

    def test_fr28_the_image_checker_finds_what_breaks_dhcp_and_dns(self):
        cmdline = make_image.kernel_cmdline("blkdevparts=x:1@0(misc)")
        # The Cuttlefish properties as they came: no virt_wifi.
        raw = dict(self.NETWORK_FILES)
        problems = make_image.network_problems(list(raw), raw.__getitem__, cmdline)
        self.assertTrue(any("ro.vendor.wifi_impl" in p for p in problems), problems)
        # A pinned DNS server and a static address.
        files = self.network_files()
        files["VENDOR/build.prop"] += b"net.dns1=8.8.8.8\n"
        problems = make_image.network_problems(
            list(files), files.__getitem__, cmdline + " androidboot.auto_eth_guest_addr=192.168.98.2")
        self.assertTrue(any("net.dns1" in p for p in problems), problems)
        self.assertTrue(any("static address" in p for p in problems), problems)
        # No setup_wifi, and no prefix for it.
        del files["VENDOR/apex/com.google.cf.wifi.apex"]
        problems = make_image.network_problems(list(files), files.__getitem__, "console=ttyS0")
        self.assertTrue(any("com.google.cf.wifi.apex is missing" in p for p in problems), problems)
        self.assertTrue(any("wifi_mac_prefix" in p for p in problems), problems)
        # An APEX without the setup_wifi service.
        files = self.network_files()
        files["VENDOR/apex/com.google.cf.wifi.apex"] = self.wifi_apex(b"service other /bin/other")
        problems = make_image.network_problems(list(files), files.__getitem__, cmdline)
        self.assertTrue(any("no setup_wifi service" in p for p in problems), problems)

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


    def test_fr21_android_log_stays_in_memory_and_the_console_quiets_after_boot(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            ueventd_text = "# seriallogging\n/dev/hvc2 0660 system logd\n"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("VENDOR/etc/ueventd.rc", ueventd_text)
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/etc/ueventd.rc 0 0 644 capabilities=0x0\n")
                archive.writestr("META/filesystem_config.txt", "system/etc 0 0 755 capabilities=0x0\n")

            modified = root / "modified.zip"
            make_image.modify(source, modified)

            with zipfile.ZipFile(modified) as archive:
                names = archive.namelist()
                # Nothing writes Android's log to the serial console or to a file.
                self.assertFalse(any("console-logcat" in name for name in names), names)
                for name in names:
                    if name.endswith(".rc"):
                        self.assertNotIn("/system/bin/logcat", archive.read(name).decode(), name)
                self.assertEqual(archive.read("VENDOR/etc/ueventd.rc").decode(), ueventd_text)
                console = archive.read("SYSTEM/etc/init/dsa-console.rc").decode()
                self.assertIn("on property:sys.boot_completed=1", console)
                self.assertIn("write /proc/sys/kernel/printk 1", console)
                fs_config = archive.read("META/filesystem_config.txt").decode()
                self.assertIn("system/etc/init/dsa-console.rc 0 0 644 capabilities=0x0", fs_config)
                vendor_fs = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertNotIn("console-logcat", vendor_fs)
        # logd keeps the kernel's log, which `dsa logcat -b kernel` reads.
        self.assertIn("ro.logd.kernel=true", make_image.edit_props("").splitlines())

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

    def test_fr11_the_agent_holds_the_storage_permissions_that_open_android_obb_to_it(self):
        # The agent puts the expansion files of an app the host installs in the app's folder
        # under Android/obb/, which Android opens to an installer only while it holds the
        # storage permissions. They are runtime permissions, so the image grants them.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self._target_files_with_system_ext(root)
            agent = root / "agent.apk"
            agent.write_bytes(b"PK agent")
            modified = root / "modified.zip"
            make_image.modify(source, modified, agent=agent)
            with zipfile.ZipFile(modified) as archive:
                granted = archive.read(
                    "SYSTEM_EXT/etc/default-permissions/default-permissions-dsa-agent.xml"
                ).decode()
                fs_config = archive.read("META/system_ext_filesystem_config.txt").decode().splitlines()
        self.assertIn('<exception package="io.github.thisisthepy.dsa.agent">', granted)
        for permission in ["READ_EXTERNAL_STORAGE", "WRITE_EXTERNAL_STORAGE"]:
            self.assertIn(f'<permission name="android.permission.{permission}"', granted)
        self.assertIn("system_ext/etc/default-permissions 0 0 755 capabilities=0x0", fs_config)
        self.assertIn(
            "system_ext/etc/default-permissions/default-permissions-dsa-agent.xml 0 0 644 capabilities=0x0",
            fs_config,
        )
        manifest = (make_image.REPO / "dsa/guest/agent/src/main/AndroidManifest.xml").read_text()
        self.assertIn('android.permission.WRITE_EXTERNAL_STORAGE', manifest)

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
        # Runtime permissions are not privileged: the image grants them by default instead.
        defaults = (agent / "default-permissions-dsa-agent.xml").read_text()
        runtime = set(re.findall(r'<permission name="([^"]+)"', defaults))
        self.assertTrue(requested)
        self.assertEqual(requested - runtime, granted)
        self.assertFalse(runtime & granted)
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

    def test_fr26_dsa_gnss_hal_replaces_cuttlefishs(self):
        # The guest's location is the host's: Cuttlefish's GNSS HAL goes, as an APEX or as
        # plain files, so nothing else claims IGnss/default.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            gnss = root / "dsa-gnss"
            gnss.write_bytes(b"\x7fELF gnss")
            channeld = root / "dsa-channeld"
            channeld.write_bytes(b"\x7fELF channeld")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("VENDOR/apex/com.google.cf.gnss.apex", b"apex")
                archive.writestr("VENDOR/bin/hw/android.hardware.gnss-service.example", b"\x7fELF example")
                archive.writestr("VENDOR/etc/init/android.hardware.gnss-service.example.rc", "service gnss\n")
                archive.writestr("VENDOR/etc/vintf/manifest/android.hardware.gnss-service.example.xml", "<manifest/>")
                archive.writestr("VENDOR/bin/hw/android.hardware.sensors-service.example", b"\x7fELF sensors")
                archive.writestr(
                    "META/apexkeys.txt",
                    'name="com.google.cf.gnss.apex" public_key="a"\n'
                    'name="com.android.hardware.gatekeeper.nonsecure.apex" public_key="c"\n',
                )
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")

            modified = root / "modified.zip"
            make_image.modify(source, modified, channeld=channeld, gnss_hal=gnss)

            with zipfile.ZipFile(modified) as archive:
                names = archive.namelist()
                self.assertNotIn("VENDOR/apex/com.google.cf.gnss.apex", names)
                self.assertNotIn("VENDOR/bin/hw/android.hardware.gnss-service.example", names)
                self.assertNotIn("VENDOR/etc/init/android.hardware.gnss-service.example.rc", names)
                self.assertNotIn("VENDOR/etc/vintf/manifest/android.hardware.gnss-service.example.xml", names)
                self.assertIn("VENDOR/bin/hw/android.hardware.sensors-service.example", names)
                apexkeys = archive.read("META/apexkeys.txt").decode()
                self.assertNotIn("cf.gnss", apexkeys)
                self.assertIn("gatekeeper", apexkeys)
                self.assertEqual(archive.read("VENDOR/bin/hw/dsa-gnss"), b"\x7fELF gnss")
                self.assertIn("service vendor.dsa-gnss /vendor/bin/hw/dsa-gnss",
                              archive.read("VENDOR/etc/init/dsa-gnss.rc").decode())
                manifest = archive.read("VENDOR/etc/vintf/manifest/dsa-gnss.xml").decode()
                self.assertIn("<name>android.hardware.gnss</name>", manifest)
                self.assertIn("<version>4</version>", manifest)
                self.assertIn("<fqname>IGnss/default</fqname>", manifest)
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/bin/hw/dsa-gnss 0 2000 755 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/init/dsa-gnss.rc 0 0 644 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/vintf/manifest/dsa-gnss.xml 0 0 644 capabilities=0x0", fs_config)
                contexts = archive.read("VENDOR/etc/selinux/vendor_file_contexts").decode()
                self.assertIn("/vendor/bin/hw/dsa-gnss u:object_r:hal_gnss_default_exec:s0", contexts)
                policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertIn("(allow hal_gnss_server dsa_channel_service (service_manager (find)))", policy)
                self.assertIn("(allow dsa_channeld hal_gnss_server (binder (call transfer)))", policy)

    def test_fr26_the_gnss_hal_needs_the_channel_daemon(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
            with self.assertRaises(ValueError):
                make_image.modify(source, root / "modified.zip", gnss_hal=root / "dsa-gnss")

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

    def test_fr26_the_sensors_hal_replaces_cuttlefishs_with_its_policy(self):
        # The guest reads the host's sensors: the image carries DSA's sensors HAL, labelled
        # in the sensors domain with leave to reach the host over vsock, and no other.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            sensors = root / "dsa-sensors"
            sensors.write_bytes(b"\x7fELF sensors")
            other = ('<manifest version="1.0" type="device"><hal format="aidl">'
                     "<name>android.hardware.sensors</name><fqname>ISensors/default</fqname>"
                     "</hal></manifest>")
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
                archive.writestr("VENDOR/apex/com.android.hardware.sensors.apex", b"apex")
                archive.writestr("VENDOR/apex/com.google.cf.sensors.apex", b"apex")
                archive.writestr("VENDOR/etc/vintf/manifest/sensors-default.xml", other)
                archive.writestr("VENDOR/etc/vintf/manifest/other.xml", "<manifest/>")
                archive.writestr("META/apexkeys.txt",
                                 'name="com.google.cf.sensors.apex" public_key="k"\n'
                                 'name="com.android.adbd.apex" public_key="k"\n')

            modified = root / "modified.zip"
            make_image.modify(source, modified, sensors=sensors)

            with zipfile.ZipFile(modified) as archive:
                names = set(archive.namelist())
                self.assertEqual(archive.read("VENDOR/bin/hw/dsa-sensors"), b"\x7fELF sensors")
                self.assertIn("service vendor.dsa-sensors /vendor/bin/hw/dsa-sensors",
                              archive.read("VENDOR/etc/init/dsa-sensors.rc").decode())
                manifest = archive.read("VENDOR/etc/vintf/manifest/dsa-sensors.xml").decode()
                self.assertIn("<name>android.hardware.sensors</name>", manifest)
                self.assertIn("<version>2</version>", manifest)
                self.assertIn("<fqname>ISensors/default</fqname>", manifest)
                # Cuttlefish's sensors HAL goes, in an APEX or declared beside it.
                self.assertNotIn("VENDOR/apex/com.android.hardware.sensors.apex", names)
                self.assertNotIn("VENDOR/apex/com.google.cf.sensors.apex", names)
                self.assertNotIn("VENDOR/etc/vintf/manifest/sensors-default.xml", names)
                self.assertIn("VENDOR/etc/vintf/manifest/other.xml", names)
                apexkeys = archive.read("META/apexkeys.txt").decode()
                self.assertNotIn("com.google.cf.sensors", apexkeys)
                self.assertIn("com.android.adbd", apexkeys)
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/bin/hw/dsa-sensors 0 2000 755 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/init/dsa-sensors.rc 0 0 644 capabilities=0x0", fs_config)
                self.assertIn("/vendor/bin/hw/dsa-sensors u:object_r:hal_sensors_default_exec:s0",
                              archive.read("VENDOR/etc/selinux/vendor_file_contexts").decode())
                self.assertIn("(allow hal_sensors_server self (vsock_socket",
                              archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode())

    def test_fr26_without_the_sensors_hal_cuttlefishs_stays(self):
        self.assertNotIn("com.google.cf.sensors", make_image.removed_apexes())
        self.assertFalse(make_image.removed("VENDOR/apex/com.google.cf.sensors.apex"))
        self.assertTrue(make_image.removed("VENDOR/apex/com.google.cf.sensors.apex", sensors=True))

    def test_pr4_without_the_codec_hal_codec_buffers_stay_pooled(self):
        self.assertNotIn("debug.stagefright.c2-poolmask", make_image.edit_props("ro.debuggable=1\n"))

    def _audio_source(self, root):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n(typeattribute hal_audio_server)\n")
            archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
            archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
            archive.writestr("META/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
            archive.writestr(
                "VENDOR/etc/audio_policy_configuration.xml",
                '<audioPolicyConfiguration version="7.0" xmlns:xi="http://www.w3.org/2001/XInclude">\n'
                "    <modules>\n"
                "        <!-- Primary Audio HAL -->\n"
                '        <xi:include href="primary_audio_policy_configuration.xml"/>\n'
                '        <xi:include href="r_submix_audio_policy_configuration.xml"/>\n'
                "    </modules>\n"
                "</audioPolicyConfiguration>\n",
            )
        return source

    def test_pr4_the_dsa_audio_module_serves_the_primary_output(self):
        # The image's audio service makes a module for each one its policy file lists, and
        # makes the primary one (IModule/default) play nowhere on Hyper-V. With dsa-audio, the
        # primary module leaves that file, so the service keeps the remote submix and
        # Bluetooth modules and dsa-audio alone registers IModule/default.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self._audio_source(root)
            audio = root / "dsa-audio"
            audio.write_bytes(b"\x7fELF audio")
            modified = root / "modified.zip"
            make_image.modify(source, modified, audio=audio)

            with zipfile.ZipFile(modified) as archive:
                self.assertEqual(archive.read("VENDOR/bin/hw/dsa-audio"), b"\x7fELF audio")
                rc = archive.read("VENDOR/etc/init/dsa-audio.rc").decode()
                self.assertIn("service vendor.dsa-audio /vendor/bin/hw/dsa-audio", rc)
                self.assertIn("user audioserver", rc)
                self.assertIn("onrestart restart audioserver", rc)
                policy = archive.read("VENDOR/etc/audio_policy_configuration.xml").decode()
                self.assertNotIn("primary_audio_policy_configuration.xml", policy)
                self.assertIn("r_submix_audio_policy_configuration.xml", policy)
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/bin/hw/dsa-audio 0 2000 755 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/init/dsa-audio.rc 0 0 644 capabilities=0x0", fs_config)
                for contexts in ["VENDOR/etc/selinux/vendor_file_contexts", "META/vendor_file_contexts"]:
                    self.assertIn(
                        "/vendor/bin/hw/dsa-audio u:object_r:hal_audio_default_exec:s0",
                        archive.read(contexts).decode(),
                    )
                plat = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertIn("(allow hal_audio_server self (vsock_socket", plat)

    def test_pr4_without_dsa_audio_the_image_keeps_its_primary_module(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self._audio_source(root)
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                self.assertNotIn("VENDOR/bin/hw/dsa-audio", archive.namelist())
                policy = archive.read("VENDOR/etc/audio_policy_configuration.xml").decode()
                self.assertIn("primary_audio_policy_configuration.xml", policy)
                plat = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertNotIn("(allow hal_audio_server self (vsock_socket", plat)

    def test_fr26_the_camera_hal_replaces_the_emulated_cameras(self):
        # The host's cameras are Android's: the image carries the camera provider HAL, its
        # label, its service's label, its policy and the camera features, and drops
        # Cuttlefish's emulated cameras.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            camera = root / "dsa-camera"
            camera.write_bytes(b"\x7fELF camera")
            emulated = "VENDOR/apex/com.google.emulated.camera.provider.hal.apex"
            variant = "VENDOR/apex/com.google.emulated.camera.provider.hal.fastscenecycle.apex"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr(variant, b"apex")
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_service_contexts", "")
                archive.writestr("META/apexkeys.txt",
                                 'name="com.google.emulated.camera.provider.hal.apex" public_key="k"\n')
                archive.writestr(emulated, b"apex")

            modified = root / "modified.zip"
            make_image.modify(source, modified, camera=camera)

            with zipfile.ZipFile(modified) as archive:
                names = archive.namelist()
                self.assertNotIn(emulated, names)
                # The other variant declares the same provider instance; left in, apexd
                # activates it and the vendor VINTF manifest fails.
                self.assertNotIn(variant, names)
                self.assertNotIn(b"emulated.camera", archive.read("META/apexkeys.txt"))
                self.assertEqual(archive.read("VENDOR/bin/hw/dsa-camera"), b"\x7fELF camera")
                self.assertIn("service vendor.dsa-camera /vendor/bin/hw/dsa-camera",
                              archive.read("VENDOR/etc/init/dsa-camera.rc").decode())
                manifest = archive.read("VENDOR/etc/vintf/manifest/dsa-camera.xml").decode()
                self.assertIn("<name>android.hardware.camera.provider</name>", manifest)
                self.assertIn("<fqname>ICameraProvider/internal/0</fqname>", manifest)
                features = archive.read("VENDOR/etc/permissions/dsa-camera-features.xml").decode()
                for feature in ["android.hardware.camera.any", "android.hardware.camera.front",
                                "android.hardware.camera.external"]:
                    self.assertIn(f'name="{feature}"', features)
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                self.assertIn("vendor/bin/hw/dsa-camera 0 2000 755 capabilities=0x0", fs_config)
                self.assertIn("vendor/etc/permissions/dsa-camera-features.xml 0 0 644 capabilities=0x0", fs_config)
                self.assertIn("/vendor/bin/hw/dsa-camera u:object_r:hal_camera_default_exec:s0",
                              archive.read("VENDOR/etc/selinux/vendor_file_contexts").decode())
                self.assertIn(
                    "android.hardware.camera.provider.ICameraProvider/internal/0 u:object_r:hal_camera_service:s0",
                    archive.read("VENDOR/etc/selinux/vendor_service_contexts").decode())
                self.assertIn("(allow hal_camera_server self (vsock_socket",
                              archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode())

    def test_fr26_without_the_camera_hal_the_emulated_cameras_stay(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            emulated = "VENDOR/apex/com.google.emulated.camera.provider.hal.apex"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr(emulated, b"apex")
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                self.assertIn(emulated, archive.namelist())
                self.assertNotIn("VENDOR/bin/hw/dsa-camera", archive.namelist())

    def test_fr26_the_kernel_command_line_selects_no_emulated_camera_with_the_camera_hal(self):
        selection = "androidboot.vendor.apex.com.google.emulated.camera.provider.hal="
        self.assertIn(selection, make_image.kernel_cmdline("blkdevparts=sda:1@0(misc)"))
        self.assertNotIn(selection, make_image.kernel_cmdline("blkdevparts=sda:1@0(misc)", camera=True))
        self.assertNotIn(selection, make_image.kernel_cmdline("blkdevparts=sda:1@0(misc)", hwcomposer=True,
                                                              camera=True))

    @staticmethod
    def vendor_apex(module, payload=b""):
        """A vendor APEX of `module` whose payload holds `payload`, as apexd and libvintf see it."""
        name = module.encode()
        manifest = b"\x0a" + bytes([len(name)]) + name + b"\x10\x01"
        data = io.BytesIO()
        with zipfile.ZipFile(data, "w") as apex:
            apex.writestr("apex_manifest.pb", manifest)
            apex.writestr("apex_payload.img", b"\0" * 64 + payload + b"\0" * 64)
        return data.getvalue()

    CAMERA_FRAGMENT = (b'<manifest version="1.0" type="device"><hal format="aidl">'
                       b'<name>android.hardware.camera.provider</name><version>3</version>'
                       b'<fqname>ICameraProvider/internal/0</fqname></hal></manifest>')

    def test_fr26_every_variant_of_a_replaced_apex_module_goes(self):
        # Cuttlefish ships variants of one module under other file names. Removing the one a
        # DSA HAL replaces made apexd activate the next, whose HAL declaration collided with
        # DSA's and failed the vendor VINTF manifest, so no vendor HAL could register.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            camera = root / "dsa-camera"
            camera.write_bytes(b"\x7fELF camera")
            module = "com.google.emulated.camera.provider.hal"
            listed = f"VENDOR/apex/{module}.apex"
            # A variant no list names, as a later Cuttlefish build may add.
            variant = f"VENDOR/apex/{module}.legacy.apex"
            unrelated = "VENDOR/apex/com.google.cf.light.apex"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_service_contexts", "")
                archive.writestr("META/apexkeys.txt",
                                 f'name="{module}.apex" public_key="k"\n'
                                 f'name="{module}.legacy.apex" public_key="k"\n'
                                 'name="com.google.cf.light.apex" public_key="k"\n')
                archive.writestr(listed, self.vendor_apex(module, self.CAMERA_FRAGMENT))
                archive.writestr(variant, self.vendor_apex(module, self.CAMERA_FRAGMENT))
                archive.writestr(unrelated, self.vendor_apex("com.google.cf.light"))
            modified = root / "modified.zip"
            make_image.modify(source, modified, camera=camera)
            with zipfile.ZipFile(modified) as archive:
                names = archive.namelist()
                self.assertNotIn(listed, names)
                self.assertNotIn(variant, names)
                self.assertIn(unrelated, names)
                keys = archive.read("META/apexkeys.txt").decode()
                self.assertNotIn("emulated.camera", keys)
                self.assertIn("com.google.cf.light.apex", keys)

    def test_fr26_a_hal_instance_declared_twice_is_a_problem(self):
        # The first PC boot: DSA's camera fragment and the emulated cameras' fastscenecycle
        # APEX both declared ICameraProvider/internal/0.
        files = {
            "VENDOR/bin/hw/dsa-camera": b"\x7fELF",
            "VENDOR/etc/vintf/manifest/dsa-camera.xml": (make_image.CAMERA_DIR / "dsa-camera.xml").read_bytes(),
            "VENDOR/apex/com.google.emulated.camera.provider.hal.fastscenecycle.apex":
                self.vendor_apex("com.google.emulated.camera.provider.hal", self.CAMERA_FRAGMENT),
        }
        problems = make_image.vendor_vintf_problems(list(files), files.__getitem__, "console=ttyS0")
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("ICameraProvider/internal/0", problems[0])
        self.assertIn("fastscenecycle", problems[0])

    def test_fr26_only_the_selected_variant_of_an_apex_module_declares_its_hals(self):
        keymint = (b'<manifest version="1.0" type="device"><hal format="aidl">'
                   b'<name>android.hardware.security.keymint</name>'
                   b'<fqname>IKeyMintDevice/default</fqname></hal></manifest>')
        files = {
            "VENDOR/apex/com.android.hardware.keymint.rust_nonsecure.apex":
                self.vendor_apex("com.android.hardware.keymint", keymint),
            "VENDOR/apex/com.android.hardware.keymint.rust_cf_remote.apex":
                self.vendor_apex("com.android.hardware.keymint", keymint),
        }
        selected = "androidboot.vendor.apex.com.android.hardware.keymint=com.android.hardware.keymint.rust_nonsecure"
        self.assertEqual(make_image.vendor_vintf_problems(list(files), files.__getitem__, selected), [])
        # With nothing selected apexd may activate either, so the two count as declaring it twice.
        self.assertEqual(len(make_image.vendor_vintf_problems(list(files), files.__getitem__, "")), 1)

    def test_fr26_a_dsa_service_without_a_declaration_is_a_problem(self):
        # servicemanager refuses a service no manifest declares, and the HAL exits with status 1.
        # dsa-audio has no fragment of its own: the audio APEX declares the primary module.
        audio = (b'<manifest version="1.0" type="device"><hal format="aidl">'
                 b'<name>android.hardware.audio.core</name><version>3</version>'
                 b'<fqname>IModule/default</fqname><fqname>IModule/r_submix</fqname></hal></manifest>')
        files = {"VENDOR/bin/hw/dsa-audio": b"\x7fELF"}
        problems = make_image.vendor_vintf_problems(list(files), files.__getitem__, "")
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("IModule/default", problems[0])
        files["VENDOR/apex/com.android.hardware.audio.apex"] = self.vendor_apex("com.android.hardware.audio", audio)
        self.assertEqual(make_image.vendor_vintf_problems(list(files), files.__getitem__, ""), [])

    def test_fr26_every_dsa_hal_fragment_declares_the_version_its_service_is_built_from(self):
        # A fragment's version must be the frozen AIDL version build.sh generates the service
        # from, and the instance the service registers.
        guest = Path(__file__).parents[1]
        fragments = {
            "camera/dsa-camera.xml": "camera",
            "codec2/dsa-codec2.xml": "codec2",
            "gnss/dsa-gnss.xml": "gnss",
            "hwcomposer/dsa-hwcomposer.xml": "hwcomposer",
            "power/dsa-power.xml": "power",
            "power/dsa-health.xml": "power",
            "sensors/dsa-sensors.xml": "sensors",
        }
        for fragment, component in fragments.items():
            text = (guest / fragment).read_text()
            package = re.search(r"<name>([^<]+)</name>", text).group(1)
            version = re.search(r"<version>([0-9]+)</version>", text).group(1)
            build = (guest / component / "build.sh").read_text()
            built = re.findall(r"aidl_api/" + re.escape(package) + r"/([0-9]+)", build)
            self.assertEqual(built, [version], f"{fragment} declares {package} {version}, build.sh uses {built}")
            binary = "VENDOR/bin/hw/" + Path(fragment).stem
            served = make_image.DSA_SERVED_INSTANCES[binary]
            self.assertIn(served, make_image.vintf_instances(text.encode()), fragment)

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
        cmdline = make_image.kernel_cmdline(f"blkdevparts={make_image.SYSTEM_DISK_GUID}:1@0(misc)", hwcomposer=True)
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
            # Without the GNSS HAL the guest never gets the host's location.
            self.assertIn("VENDOR/bin/hw/dsa-gnss is missing", proc.stdout)

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
            # Without the sensors HAL apps see none of the host's sensors.
            self.assertIn("VENDOR/bin/hw/dsa-sensors is missing", proc.stdout)
            # Without the camera HAL apps never see the host's cameras.
            self.assertIn("VENDOR/bin/hw/dsa-camera is missing", proc.stdout)
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

    def test_nfr1_free_memory_is_compacted_sooner_so_page_reporting_hands_it_back(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-memory.rc").decode()
            self.assertIn("write /proc/sys/vm/compaction_proactiveness 40", rc)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn(
                '(genfscon proc "/sys/vm/compaction_proactiveness" (u object_r dsa_proc_vm_tunable',
                policy,
            )

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
            # And the online memory blocks, which plain sysfs would keep from it.
            self.assertIn(
                '(genfscon sysfs "/devices/system/memory" (u object_r dsa_sysfs_memory', policy
            )
            for app in ("system_app", "platform_app"):
                self.assertIn(f"(allow {app} dsa_sysfs_memory (dir (search read open getattr)))", policy)
                self.assertIn(f"(allow {app} dsa_sysfs_memory (file (read open getattr)))", policy)

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

    def test_nfr1_device_lock_is_not_advertised_so_its_controller_and_service_do_not_run(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            xml = archive.read("VENDOR/etc/permissions/dsa-unavailable-features.xml").decode()
        self.assertIn('<unavailable-feature name="android.software.device_lock" />', xml)

    def test_nfr1_cuttlefishs_host_monitoring_service_is_removed(self):
        self.assertIn("CuttlefishService", make_image.REMOVED_APPS)

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

    def test_fr29_the_policy_of_the_file_sharing_jobs_goes_into_the_image_with_the_daemon(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn("(typeattributeset domain (dsa_sharemount))", policy)
            self.assertIn("(typeattributeset domain (dsa_files))", policy)

    def test_nfr1_an_idle_reclaim_swaps_cold_pages_out_before_dropping_caches_and_waits_for_boot(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode()
            start = rc.index("on property:vendor.dsa.memory.reclaim=1 && property:sys.boot_completed=1")
            trigger = rc[start:rc.index("on property:vendor.dsa.memory.trim=1")]
            self.assertEqual(
                trigger.count('write /sys/fs/cgroup/memory.reclaim "${vendor.dsa.memory.reclaim_step}M swappiness=200"'), 4
            )
            self.assertNotIn("256M", trigger)
            contexts = archive.read("VENDOR/etc/selinux/vendor_property_contexts").decode().splitlines()
            self.assertIn("vendor.dsa.memory.reclaim_step u:object_r:dsa_memory_prop:s0 exact int", contexts)
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

    def test_nfr2_a_trim_reclaims_the_coldest_pages_and_compacts_so_they_are_reported_at_once(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            contexts = archive.read("VENDOR/etc/selinux/vendor_property_contexts").decode().splitlines()
            self.assertIn("vendor.dsa.memory.trim u:object_r:dsa_memory_prop:s0 exact bool", contexts)
            rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode()
            trigger = rc[rc.index("on property:vendor.dsa.memory.trim=1"):]
            self.assertIn("vendor.dsa.memory.trim_size u:object_r:dsa_memory_prop:s0 exact int", contexts)
            self.assertIn('write /sys/fs/cgroup/memory.reclaim "${vendor.dsa.memory.trim_size}M swappiness=0"', trigger)
            self.assertNotIn("drop_caches", trigger)
            # What the trim freed is scattered: compacting gathers it into whole blocks that free
            # page reporting hands back now, not when background compaction gets to it.
            self.assertLess(trigger.index("cgroup/memory.reclaim"), trigger.index("write /proc/sys/vm/compact_memory 1"))
            self.assertLess(trigger.index("compact_memory"), trigger.index("setprop vendor.dsa.memory.trim 0"))
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
            + make_image.RECLAIM_STEP_PROPERTY_CONTEXT + "\n"
            + make_image.TRIM_PROPERTY_CONTEXT + "\n"
            + make_image.TRIM_SIZE_PROPERTY_CONTEXT + "\n"
            + make_image.BOOT_PROPERTY_CONTEXT + "\n"
            + make_image.ADB_PROPERTY_CONTEXT + "\n"
            + make_image.SHARES_PROPERTY_CONTEXT + "\n"
            + make_image.FILES_PROPERTY_CONTEXT + "\n"
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
        removed = ["Dialer", "messaging", "Stk", "BasicDreams", "LiveWallpapersPicker", "QuickSearchBox",
                   "Calendar", "Gallery2", "Music"]
        kept = [
            "SYSTEM/app/webview/webview.apk",
            "SYSTEM/priv-app/ContactsProvider/ContactsProvider.apk",
            "SYSTEM/priv-app/CalendarProvider/CalendarProvider.apk",
            "SYSTEM/priv-app/DownloadProvider/DownloadProvider.apk",
            "SYSTEM/priv-app/MusicFX/MusicFX.apk",
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
                       "TeleService", "DeskClock", "CalendarProvider", "DownloadProvider",
                       "MusicFX"]:
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

    def test_fr1_the_connected_devices_screen_opens_without_saved_bluetooth_devices(self):
        # Settings died opening Connected devices: the previously connected devices list
        # asked the null default adapter isEnabled() in SavedBluetoothDeviceUpdater.forceUpdate,
        # from PreviouslyConnectedDevicePreferenceController.displayPreference. Its category goes,
        # and with it the controller, so the fragment's use() returns null and onAttach must not
        # call init on it.
        self.assertIn(
            PREVIOUSLY_CONNECTED,
            make_image.SETTINGS_CONTROLLERS_WITHOUT_BLUETOOTH["res/xml/connected_devices.xml"],
        )
        self.assertIn(("skip call", PREVIOUSLY_CONNECTED_INIT),
                      make_image.SETTINGS_CODE_WITHOUT_BLUETOOTH[CONNECTED_DEVICES_ON_ATTACH])
        dex = connected_devices_dex()
        apk = self.modified_settings({
            "classes2.dex": dex,
            "res/xml/connected_devices.xml": compiled_xml(CONNECTED_DEVICES_SCREEN),
        })
        with zipfile.ZipFile(io.BytesIO(apk)) as archive:
            edited = archive.read("classes2.dex")
            screen = archive.read("res/xml/connected_devices.xml")
        original = dex_code(dex, CONNECTED_DEVICES_ON_ATTACH)
        code = dex_code(edited, CONNECTED_DEVICES_ON_ATTACH)
        # Only the init call (three code units, from unit 11) is gone; use() and its cast stay.
        self.assertEqual(code[:22], original[:22])
        self.assertEqual(code[22:28], bytes(6))
        self.assertEqual(code[28:], original[28:])
        self.assert_dex_is_sealed(edited)
        self.assertEqual(make_image.edit_dex(edited, make_image.SETTINGS_CODE_WITHOUT_BLUETOOTH)[1], [])
        self.assertEqual(elements(screen), [
            ("PreferenceScreen", "connected_devices_screen"),
            ("PreferenceCategory", "connected_device_list"),
            ("UntitledPreferenceCategory", None),
            ("Preference", "connection_preferences"),
        ])

    def test_fr1_the_saved_and_hearing_devices_pages_open_without_bluetooth(self):
        # Search still opens both pages. The saved devices list calls the same forceUpdate on
        # the null adapter, and the available hearing devices list registers with the null
        # LocalBluetoothManager in onStart. Both go from their screens, and their fragments'
        # onAttach must not call init on the use() lookup that now returns null.
        for xml, controller, fragment, init in [
            ("res/xml/previously_connected_devices.xml", SAVED_DEVICES, SAVED_DEVICES_ON_ATTACH,
             SAVED_DEVICES_INIT),
            ("res/xml/accessibility_hearing_aids.xml", AVAILABLE_HEARING, HEARING_AIDS_ON_ATTACH,
             AVAILABLE_HEARING_INIT),
        ]:
            with self.subTest(xml=xml):
                self.assertIn(controller, make_image.SETTINGS_CONTROLLERS_WITHOUT_BLUETOOTH[xml])
                self.assertIn(("skip call", init), make_image.SETTINGS_CODE_WITHOUT_BLUETOOTH[fragment])
                dex = on_attach_dex(fragment, init)
                screen = preference("PreferenceScreen", "screen", children=[
                    preference("PreferenceCategory", "devices", controller),
                    preference("Preference", "stays"),
                ])
                apk = self.modified_settings({"classes.dex": dex, xml: compiled_xml(screen)})
                with zipfile.ZipFile(io.BytesIO(apk)) as archive:
                    edited = archive.read("classes.dex")
                    edited_screen = archive.read(xml)
                code = dex_code(edited, fragment)
                original = dex_code(dex, fragment)
                self.assertEqual(code[:22], original[:22])
                self.assertEqual(code[22:28], bytes(6))
                self.assertEqual(code[28:], original[28:])
                self.assert_dex_is_sealed(edited)
                self.assertEqual(elements(edited_screen),
                                 [("PreferenceScreen", "screen"), ("Preference", "stays")])

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


PREVIOUSLY_CONNECTED = "com.android.settings.connecteddevice.PreviouslyConnectedDevicePreferenceController"
PREVIOUSLY_CONNECTED_INIT = ("Lcom/android/settings/connecteddevice/PreviouslyConnectedDevicePreferenceController;"
                             "->init")
CONNECTED_DEVICES_ON_ATTACH = ("Lcom/android/settings/connecteddevice/ConnectedDeviceDashboardFragment;"
                               "->onAttach")

# The part of Android 16's connected devices screen that matters here: the previously
# connected devices category with its "see all" entry, between entries that stay.
CONNECTED_DEVICES_SCREEN = preference("PreferenceScreen", "connected_devices_screen", children=[
    preference("PreferenceCategory", "connected_device_list",
               "com.android.settings.connecteddevice.ConnectedDeviceGroupController"),
    preference("PreferenceCategory", "previously_connected_devices", PREVIOUSLY_CONNECTED, children=[
        preference("Preference", "previously_connected_devices_see_all"),
    ]),
    ("UntitledPreferenceCategory", [], [
        preference("Preference", "connection_preferences",
                   "com.android.settings.connecteddevice.AdvancedConnectedDeviceController"),
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


def dex_file(methods, code, fields=()):
    """A dex file with the methods [(class, name, shorty)] and fields [(class, name, type)] it
    refers to, and the code {method index: (registers, instructions)} of those it defines,
    laid out as d8 lays it out: the header, the id tables, then the data."""
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
    field_ids = [(type_(cls), type_(kind), string(name)) for cls, name, kind in fields]
    classes = {}
    for index in sorted(code):
        classes.setdefault(method_ids[index][0], []).append(index)

    string_off = 0x70
    type_off = string_off + 4 * len(strings)
    proto_off = type_off + 4 * len(types)
    field_off = proto_off + 12 * len(protos)
    method_off = field_off + 8 * len(field_ids)
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
    tables += b"".join(struct.pack("<HHI", *ids) for ids in field_ids)
    tables += b"".join(struct.pack("<HHI", *ids) for ids in method_ids)
    tables += b"".join(struct.pack("<8I", cls, 1, 0xFFFFFFFF, 0, 0xFFFFFFFF, 0, class_data[cls], 0)
                       for cls in classes)
    size = data_off + len(data)
    header = struct.pack("<8sI20s20I", b"dex\n035\0", 0, bytes(20), size, 0x70, 0x12345678, 0, 0, 0,
                         len(strings), string_off, len(types), type_off, len(protos), proto_off,
                         len(field_ids), field_off, len(method_ids), method_off, len(classes), class_off,
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


SAVED_DEVICES = "com.android.settings.connecteddevice.SavedDeviceGroupController"
SAVED_DEVICES_INIT = "Lcom/android/settings/connecteddevice/SavedDeviceGroupController;->init"
SAVED_DEVICES_ON_ATTACH = ("Lcom/android/settings/connecteddevice/PreviouslyConnectedDeviceDashboardFragment;"
                           "->onAttach")
AVAILABLE_HEARING = "com.android.settings.accessibility.AvailableHearingDevicePreferenceController"
AVAILABLE_HEARING_INIT = ("Lcom/android/settings/accessibility/AvailableHearingDevicePreferenceController;"
                          "->init")
HEARING_AIDS_ON_ATTACH = "Lcom/android/settings/accessibility/AccessibilityHearingAidsFragment;->onAttach"


def on_attach_dex(fragment, init):
    """A fragment's onAttach in Android 16's Settings.apk as d8 compiled it, cut down to the
    use(Controller.class).init(this) call that reaches a controller."""
    cls, name = fragment.split("->")
    init_cls, init_name = init.split("->")
    methods = [
        (cls, name, "VL"),
        ("Lcom/android/settings/dashboard/DashboardFragment;", "onAttach", "VL"),
        ("Lcom/android/settings/dashboard/DashboardFragment;", "use", "LL"),
        (init_cls, init_name, "VL"),
    ]
    on_attach = bytes.fromhex(
        "6f200100 1000"  # invoke-super {v0, v1}, DashboardFragment.onAttach
        "1c010000"  # const-class v1, the controller
        "6e200200 1000"  # invoke-virtual {v0, v1}, use
        "0c01"  # move-result-object v1
        "1f010000"  # check-cast v1, the controller
        "6e200300 0100"  # invoke-virtual {v1, v0}, init
        "0e00"  # return-void
    )
    return dex_file(methods, {0: (2, on_attach)})


def connected_devices_dex():
    """ConnectedDeviceDashboardFragment.onAttach, cut down to the call that reaches the
    previously connected devices controller."""
    return on_attach_dex(CONNECTED_DEVICES_ON_ATTACH, PREVIOUSLY_CONNECTED_INIT)


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

    def test_fr5_settings_back_button_circle_is_centred_on_the_button_its_hover_is_drawn_around(self):
        # SettingsLib's icon puts a 40 dp circle 16 dp into a 56 dp drawable; the framework
        # centres the navigation button's hover layer and ripple on the 56 dp button.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_image.settings_overlay_sources(root / "src")
            manifest = (root / "src/AndroidManifest.xml").read_text()
            self.assertIn('android:targetPackage="com.android.settings"', manifest)
            self.assertIn('android:isStatic="true"', manifest)
            self.assertIn('android:hasCode="false"', manifest)
            for folder, theme in [("drawable", "light"), ("drawable-night", "dark")]:
                icon = ET.fromstring((root / "src/res" / folder / "settingslib_expressive_icon_back.xml").read_text())
                ns = "{http://schemas.android.com/apk/res/android}"
                circle, arrow = icon.findall("item")
                size = circle.find("shape/size")
                width = int(size.get(ns + "width").removesuffix("dp"))
                start = int(circle.get(ns + "start").removesuffix("dp"))
                end = int(circle.get(ns + "end").removesuffix("dp"))
                # The same 56 dp as the button, with the circle's centre on the button's centre.
                self.assertEqual(start + width + end, 56)
                self.assertEqual(start, end)
                self.assertEqual(arrow.get(ns + "start"), arrow.get(ns + "end"))
                self.assertEqual(arrow.get(ns + "gravity"), "center")
                self.assertIn(theme, circle.find("shape/solid").get(ns + "color"))
                self.assertIn(theme, arrow.find("vector").get(ns + "tint"))

            overlay = root / "DsaSettingsOverlay.apk"
            overlay.write_bytes(b"settings")
            with self._modify(root, settings_overlay=overlay) as archive:
                self.assertEqual(archive.read("VENDOR/overlay/DsaSettingsOverlay.apk"), b"settings")
                certs = archive.read("META/apkcerts.txt").decode()
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
            self.assertEqual(certs.count('name="DsaSettingsOverlay.apk"'), 1)
            self.assertIn("vendor/overlay/DsaSettingsOverlay.apk 0 0 644 capabilities=0x0", fs_config)
            self.assertEqual(fs_config.count("vendor/overlay 0 0 755"), 1)

    def test_fr22_settings_lists_keep_room_below_their_last_row_without_a_navigation_bar(self):
        # The window reports no bottom inset, and Settings' list keeps no space of its own:
        # androidx.preference's preference_recyclerview has paddingBottom 0dp.
        ns = "{http://schemas.android.com/apk/res/android}"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_image.settings_overlay_sources(root / "src")
            manifest = ET.fromstring((root / "src/AndroidManifest.xml").read_text())
            overlay = manifest.find("overlay")
            self.assertEqual(overlay.get(ns + "targetPackage"), "com.android.settings")
            resources_map = overlay.get(ns + "resourcesMap")
            self.assertTrue(resources_map.startswith("@xml/"))

            listing = ET.fromstring((root / "src/res/layout/preference_recyclerview.xml").read_text())
            self.assertEqual(listing.tag, "androidx.recyclerview.widget.RecyclerView")
            self.assertGreaterEqual(int(listing.get(ns + "paddingBottom").removesuffix("dp")), 24)
            self.assertEqual(listing.get(ns + "paddingTop"), "0dp")
            # The rows scroll into the space, so only the end of the list stops short.
            self.assertEqual(listing.get(ns + "clipToPadding"), "false")
            self.assertEqual(listing.get(ns + "layout_height"), "match_parent")
            # Settings' own id and list style, as in the layout this replaces.
            self.assertEqual(listing.get(ns + "id"), "@+id/recycler_view")
            self.assertEqual(listing.get("style"), "?attr/preferenceFragmentListStyle")

            values = ET.fromstring((root / "src/res/values/dsa.xml").read_text())
            self.assertIn("preferenceFragmentListStyle", [a.get("name") for a in values.findall("attr")])

            # Only a resource map makes the framework rewrite the overlay's references to its id
            # and attribute into Settings' ids; matching by name does not.
            mapping = ET.fromstring((root / "src/res/xml" / (resources_map.removeprefix("@xml/") + ".xml")).read_text())
            items = {item.get("target"): item.get("value") for item in mapping.findall("item")}
            for target in ["layout/preference_recyclerview", "id/recycler_view",
                           "attr/preferenceFragmentListStyle", "drawable/settingslib_expressive_icon_back"]:
                self.assertEqual(items.get(target), "@" + target)

    def test_fr5_the_settings_overlay_is_built_against_the_images_own_framework(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(make_image, "run") as run:
            root = Path(tmp)
            apk = make_image.build_settings_overlay(Path("/sdk/aapt2"), root / "framework-res.apk", root)
            compile_call, link_call = (call.args for call in run.call_args_list)
            self.assertEqual(compile_call[:2], (Path("/sdk/aapt2"), "compile"))
            self.assertIn(root / "framework-res.apk", link_call)
            self.assertEqual(link_call[link_call.index("-o") + 1], apk)

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
                        archive.writestr("VENDOR/overlay/DsaSettingsOverlay.apk", b"overlay")
                        archive.writestr("SYSTEM/etc/init/dsa-trace.rc", b"rc")
                    archive.writestr("SYSTEM/build.prop", props)
                outputs.append(subprocess.run(
                    [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                    capture_output=True, text=True,
                ).stdout)
        for missing in ["DsaFrameworkOverlay.apk is missing", "DsaSettingsOverlay.apk is missing",
                        "dsa-trace.rc is missing"]:
            self.assertIn(missing, outputs[0])
            self.assertNotIn(missing, outputs[1])

    def test_fr10_check_image_rejects_a_policy_that_leaves_wakeup_sources_unlabelled(self):
        # The suspend service reads each wakeup source's statistics; unlabelled, every read
        # is refused and logged, thousands of lines a boot.
        check_script = Path(__file__).with_name("check-image.py")
        labelled = (
            '(genfscon sysfs "/devices/virtual/wakeup" (u object_r sysfs_wakeup ((s0) (s0))))\n'
            '(genfscon sysfs "/class/wakeup" (u object_r sysfs_wakeup ((s0) (s0))))\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            outputs = []
            for policy in ["", labelled]:
                with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                    archive.writestr("META/apkcerts.txt", "")
                    archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                    archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", policy)
                outputs.append(subprocess.run(
                    [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                    capture_output=True, text=True,
                ).stdout)
        for path in ["/devices/virtual/wakeup", "/class/wakeup"]:
            missing = f"does not label sysfs {path} sysfs_wakeup"
            self.assertIn(missing, outputs[0])
            self.assertNotIn(missing, outputs[1])


class BootLogTests(unittest.TestCase):
    """What a normal boot logs: no refused property, audit flood or stale package state."""

    SYSTEM_PROPS = (
        "ro.build.version.incremental=15581820\n"
        "ro.system.build.version.incremental=15581820\n"
        "ro.system.build.fingerprint=generic/aosp_cf/vsoc:16/BP4A/15581820:userdebug/test-keys\n"
        "ro.build.id=BP4A\n"
    )

    def test_fr14_a_new_image_changes_every_fingerprint(self):
        # PackageManager reparses the apps, compiles them again and grants new permissions
        # only when a partition's fingerprint changes.
        props = make_image.edit_props(self.SYSTEM_PROPS, build_tag="dsa0123456789ab").splitlines()
        self.assertIn("ro.build.version.incremental=15581820.dsa0123456789ab", props)
        self.assertIn("ro.system.build.version.incremental=15581820.dsa0123456789ab", props)
        self.assertIn("ro.system.build.fingerprint="
                      "DSA/dsa_x86_64/dsa_x86_64:16/BP4A/15581820.dsa0123456789ab:user/test-keys", props)
        self.assertIn("ro.build.id=BP4A", props)

    def test_fr14_vendor_fingerprint_carries_the_same_tag(self):
        vendor = ("ro.vendor.build.fingerprint=generic/aosp_cf/vsoc:16/BP4A/15581820:userdebug/test-keys\n"
                  "ro.vendor.build.version.incremental=15581820\n")
        props = make_image.edit_props(vendor, vendor=True, build_tag="dsaab").splitlines()
        self.assertIn("ro.vendor.build.fingerprint=DSA/dsa_x86_64/dsa_x86_64:16/BP4A/15581820.dsaab:user/test-keys", props)
        self.assertIn("ro.vendor.build.version.incremental=15581820.dsaab", props)

    def test_fr14_the_build_tag_follows_the_image_content(self):
        one = make_image.image_build_tag([b"agent 1", None])
        self.assertEqual(one, make_image.image_build_tag([b"agent 1", None]))
        self.assertNotEqual(one, make_image.image_build_tag([b"agent 2", None]))
        self.assertRegex(one, r"^dsa[0-9a-f]{12}$")

    def test_fr14_props_are_unchanged_without_a_build_tag(self):
        props = make_image.edit_props(self.SYSTEM_PROPS)
        self.assertIn("ro.build.version.incremental=15581820\n", props)

    def test_fr1_vendor_build_props_set_only_what_vendor_init_may(self):
        # init reads the vendor and odm build.prop as vendor_init, which may not set these;
        # each line was a denial on every boot.
        props = make_image.edit_props("ro.debuggable=1\nro.adb.secure=0\n", vendor=True)
        for key in ("ro.debuggable", "ro.secure", "ro.adb.secure", "ro.force.debuggable", "sys.use_memfd"):
            self.assertNotIn(f"{key}=", props)
        self.assertIn("ro.hardware.egl=angle\n", props)
        self.assertIn("ro.vulkan.apex=\n", props)
        self.assertIn("ro.lmk.use_psi=true\n", props)
        self.assertEqual(make_image.VENDOR_PROP_FILES, {"VENDOR/build.prop", "ODM/etc/build.prop"})

    def test_fr1_system_build_prop_keeps_the_user_build_settings(self):
        props = make_image.edit_props("ro.debuggable=1\n")
        self.assertIn("ro.debuggable=0\n", props)
        self.assertIn("sys.use_memfd=true\n", props)

    def test_fr1_kernel_does_not_audit_every_syscall(self):
        # audit=1 made the kernel audit each iptables change and BPF program load, with
        # syscall records, and overflow its queue for auditd before logd started.
        self.assertNotIn("audit=1", make_image.kernel_cmdline(f"blkdevparts={make_image.SYSTEM_DISK_GUID}:1@0(misc)").split())

    def test_fr1_unused_devtmpfs_nodes_are_not_logged_as_denials(self):
        policy = make_image.KERNEL_POLICY.read_text()
        self.assertIn("(dontaudit kernel device (dir (write add_name create search)))", policy)
        self.assertNotIn("(allow ", policy)
        source = Path(make_image.__file__).read_text()
        self.assertIn('plat_patched += "\\n" + KERNEL_POLICY.read_text()', source)


class DeveloperModeTests(unittest.TestCase):
    def test_fr20_the_image_stays_a_user_build_whose_adbd_needs_no_authorisation(self):
        props = make_image.edit_props("ro.debuggable=1\nro.adb.secure=1\nro.build.type=userdebug\n").splitlines()
        self.assertIn("ro.debuggable=0", props)
        self.assertIn("ro.adb.secure=0", props)
        self.assertIn("ro.build.type=user", props)

    def test_fr20_adbd_listens_on_vsock_only_and_the_usb_configuration_does_not_start_it(self):
        props = make_image.edit_props(
            "persist.adb.tcp.port=5555\nservice.adb.tcp.port=5555\npersist.sys.usb.config=adb\n"
        ).splitlines()
        self.assertIn("service.adb.listen_addrs=vsock:5555", props)
        self.assertIn("persist.sys.usb.config=none", props)
        self.assertFalse([p for p in props if p.startswith(("persist.adb.tcp.port=", "service.adb.tcp.port="))])

    def test_fr20_developer_mode_starts_and_stops_adbd_through_a_property_only_the_daemon_sets(self):
        rc = (Path(__file__).with_name("dsa-channeld.rc")).read_text()
        self.assertIn("on property:vendor.dsa.adb=1\n    start adbd", rc)
        self.assertIn("on property:vendor.dsa.adb=0\n    stop adbd", rc)
        self.assertIn("on property:sys.usb.config=none && property:vendor.dsa.adb=1\n    start adbd", rc)
        contexts = make_image.update_vendor_property_contexts("")
        self.assertIn("vendor.dsa.adb u:object_r:dsa_adb_prop:s0 exact bool", contexts)
        channeld = (Path(__file__).parent / "sepolicy/channeld.cil").read_text()
        self.assertIn("(allow dsa_channeld dsa_adb_prop (property_service (set)))", channeld)
        adb = (Path(__file__).parent / "sepolicy/adb.cil").read_text()
        self.assertNotRegex(adb, r"\(allow (?!vendor_init)\S+ dsa_adb_prop \(property_service")

    def test_fr29_the_host_folders_are_mounted_and_the_storage_served_through_properties_only_the_daemon_sets(self):
        rc = (Path(__file__).with_name("dsa-channeld.rc")).read_text()
        self.assertIn("on property:vendor.dsa.shares=* && property:sys.boot_completed=1\n    start dsa_sharemount", rc)
        self.assertIn("service dsa_sharemount /vendor/bin/dsa-channeld --share-mount", rc)
        self.assertIn("seclabel u:r:dsa_sharemount:s0", rc)
        self.assertIn("on property:vendor.dsa.files=* && property:sys.boot_completed=1\n    restart dsa_files", rc)
        files_service = rc[rc.index("service dsa_files"):]
        self.assertIn("user media_rw", files_service)
        contexts = make_image.update_vendor_property_contexts("")
        self.assertIn("vendor.dsa.shares u:object_r:dsa_shares_prop:s0 exact string", contexts)
        self.assertIn("vendor.dsa.files u:object_r:dsa_files_prop:s0 exact string", contexts)
        files = (Path(__file__).parent / "sepolicy/files.cil").read_text()
        setters = re.findall(r"\(allow (\S+) dsa_(?:shares|files)_prop \(property_service \(set\)\)\)", files)
        self.assertEqual(sorted(set(setters)), ["dsa_channeld"])
        # The host's folders carry the shared storage's label, so the media provider serves them.
        self.assertIn('(genfscon 9p "/" (u object_r media_rw_data_file ((s0) (s0))))', files)

    def test_fr20_adbd_may_listen_on_vsock(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n(type adbd)\n")
                archive.writestr("SYSTEM/build.prop", "persist.adb.tcp.port=5555\n")
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertIn("(allow adbd self (vsock_socket (create bind listen accept", policy)
                self.assertIn("(type dsa_adb_prop)", policy)
                props = archive.read("SYSTEM/build.prop").decode()
                self.assertNotIn("persist.adb.tcp.port", props)
                self.assertIn("service.adb.listen_addrs=vsock:5555", props)


class DeviceIdentityTests(unittest.TestCase):
    """The image reports DSA's device identity, and takes the one a user chose at boot."""

    CUTTLEFISH = (
        "ro.product.system.brand=Android\n"
        "ro.product.system.model=mainline\n"
        "ro.product.vendor.manufacturer=Google\n"
        "ro.product.vendor.model=Cuttlefish x86_64 phone 64-bit only\n"
        "ro.product.vendor.device=vsoc_x86_64_only\n"
        "ro.product.vendor.name=aosp_cf_x86_64_only_phone\n"
        "ro.product.board=cutf\n"
        "ro.product.first_api_level=36\n"
        "ro.build.product=vsoc_x86_64_only\n"
        "ro.build.flavor=aosp_cf_x86_64_only_phone-userdebug\n"
        "ro.build.description=aosp_cf_x86_64_only_phone-userdebug 16 BP4A 15581820 test-keys\n"
        "ro.build.display.id=aosp_cf_x86_64_only_phone-userdebug 16 BP4A 15581820\n"
        "ro.build.characteristics=default\n"
        "ro.build.fingerprint=generic/aosp_cf_x86_64_only_phone/vsoc_x86_64_only:16/BP4A/15581820:userdebug/test-keys\n"
    )

    def props(self, text):
        return dict(line.split("=", 1) for line in text.splitlines()
                    if "=" in line and not line.startswith(("#", "import ")))

    def test_fr17_every_identity_property_is_dsas(self):
        props = self.props(make_image.edit_props(self.CUTTLEFISH))
        self.assertEqual(props["ro.product.system.brand"], "DSA")
        self.assertEqual(props["ro.product.system.model"], "Desktop Subsystem for Android")
        self.assertEqual(props["ro.product.vendor.manufacturer"], "thisisthepy")
        self.assertEqual(props["ro.product.vendor.device"], "dsa_x86_64")
        self.assertEqual(props["ro.product.vendor.name"], "dsa_x86_64")
        self.assertEqual(props["ro.build.product"], "dsa_x86_64")
        self.assertEqual(props["ro.build.flavor"], "dsa_x86_64-user")
        self.assertEqual(props["ro.build.description"], "dsa_x86_64-user 16 BP4A 15581820 test-keys")
        self.assertEqual(props["ro.build.display.id"], "BP4A")
        self.assertEqual(props["ro.build.characteristics"], "tablet")
        self.assertEqual(props["ro.build.fingerprint"],
                         "DSA/dsa_x86_64/dsa_x86_64:16/BP4A/15581820:user/test-keys")
        # What is not identity stays.
        self.assertEqual(props["ro.product.board"], "cutf")
        self.assertEqual(props["ro.product.first_api_level"], "36")
        for value in props.values():
            for old in ("Cuttlefish", "vsoc_", "aosp_cf_", "userdebug"):
                self.assertNotIn(old, value)

    def test_fr17_dlkm_and_ramdisk_props_get_the_identity_and_the_tag(self):
        text = ("ro.product.bootimage.model=Cuttlefish x86_64 phone 64-bit only\n"
                "ro.bootimage.build.fingerprint=generic/aosp_cf/vsoc:16/BP4A/1:userdebug/test-keys\n")
        props = self.props(make_image.edit_identity(text, build_tag="dsaab"))
        self.assertEqual(props["ro.product.bootimage.model"], "Desktop Subsystem for Android")
        self.assertEqual(props["ro.bootimage.build.fingerprint"],
                         "DSA/dsa_x86_64/dsa_x86_64:16/BP4A/1.dsaab:user/test-keys")
        self.assertIn("VENDOR_DLKM/etc/build.prop", make_image.IDENTITY_PROP_FILES)
        self.assertIn("INIT_BOOT/RAMDISK/system/etc/ramdisk/build.prop", make_image.IDENTITY_PROP_FILES)
        self.assertIn("BOOT/RAMDISK/system/etc/ramdisk/build.prop", make_image.IDENTITY_PROP_FILES)

    def test_fr17_the_product_props_import_the_identity_the_boot_carries(self):
        text = make_image.edit_props("ro.product.product.brand=Android\n", product=True)
        lines = text.splitlines()
        imports = [line for line in lines if line.startswith("import ")]
        prop = "/second_stage_resources/system/etc/ramdisk/build.prop"
        for key in ["ro.product.*", "ro.build.characteristics", "ro.build.fingerprint",
                    "ro.system.build.fingerprint", "ro.vendor.build.fingerprint", "ro.soc.*"]:
            self.assertIn(f"import {prop} {key}", imports)
        # The imports come last, so they win over every value read before them.
        self.assertTrue(all(line.startswith(("import ", "#")) for line in lines[-len(imports):]))
        self.assertIn("ro.build.characteristics=tablet", lines)
        self.assertEqual(make_image.PRODUCT_PROP_FILE, "PRODUCT/etc/build.prop")
        self.assertIn(make_image.PRODUCT_PROP_FILE, make_image.PROP_FILES)
        self.assertNotIn("import ", make_image.edit_props("ro.product.system.brand=Android\n"))

    def test_fr17_image_json_names_the_build_the_host_composes_fingerprints_from(self):
        system = ("ro.build.version.release_or_codename=16\nro.build.id=BP4A\n"
                  "ro.build.version.incremental=15581820.dsaab\nro.build.tags=release-keys\n")
        self.assertEqual(make_image.image_identity(system),
                         {"release": "16", "id": "BP4A", "incremental": "15581820.dsaab",
                          "tags": "release-keys"})


BOOTCLASSPATH = ("/apex/com.android.art/javalib/core-oj.jar:/system/framework/framework.jar:"
                 "/apex/com.android.adservices/javalib/framework-adservices.jar:"
                 "/apex/com.android.wifi/javalib/framework-wifi.jar")
SETTINGS_CONTEXT = ("PCL[]{PCL[/system/framework/org.apache.http.legacy.jar*3144028450]#"
                    "PCL[/system_ext/framework/androidx.window.extensions.jar*1635530414]}")


def fake_odex(**keys):
    """An odex whose oat header holds the given key-value store, padded like the real ones."""
    store = b"".join(k.encode() + b"\0" + v.encode() + b"\0" + b"\0" * 7 for k, v in sorted(keys.items()))
    return b"\x7fELF" + b"\0" * 60 + b"oat\n275\0" + b"\0" * 64 + store


def fake_vdex(checksums):
    """A vdex with only its checksum section."""
    header = b"vdex027\0" + struct.pack("<I", 1)
    offset = len(header) + 12
    return header + struct.pack("<III", 0, offset, 4 * len(checksums)) + struct.pack(
        f"<{len(checksums)}I", *checksums)


def fake_apk(*dexes):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as z:
        for i, content in enumerate(dexes):
            z.writestr("classes.dex" if i == 0 else f"classes{i + 1}.dex", content)
        z.writestr("AndroidManifest.xml", b"manifest")
    return buffer.getvalue()


class CompiledSystemAppTests(unittest.TestCase):
    """The system apps users open are compiled when the image is made, so their screens do
    not open interpreted (the first visible response to a click)."""

    def test_fr5_settings_and_the_apps_users_open_are_compiled(self):
        for apk in ["SYSTEM_EXT/priv-app/Settings/Settings.apk",
                    "SYSTEM/priv-app/DocumentsUI/DocumentsUI.apk",
                    "SYSTEM/priv-app/PackageInstaller/PackageInstaller.apk",
                    "PRODUCT/app/webview/webview.apk",
                    make_image.AGENT_APK]:
            self.assertIn(apk, make_image.COMPILED_APPS)

    def test_fr5_settings_is_compiled_with_speed_and_others_follow_their_profile(self):
        self.assertEqual(make_image.compiler_filter(make_image.SETTINGS_APP, True), "speed")
        self.assertEqual(make_image.compiler_filter(make_image.SETTINGS_APP, False), "speed")
        docs = "SYSTEM/priv-app/DocumentsUI/DocumentsUI.apk"
        self.assertEqual(make_image.compiler_filter(docs, True), "speed-profile")
        self.assertEqual(make_image.compiler_filter(docs, False), "speed")

    def test_fr5_oat_header_keys_are_read(self):
        keys = make_image.oat_key_values(fake_odex(**{
            "bootclasspath": BOOTCLASSPATH, "bootclasspath-checksums": "i;2/cb8fc4a6:i;2/f8c5165e",
            "classpath": SETTINGS_CONTEXT, "compiler-filter": "speed", "compilation-reason": "prebuilt"}))
        self.assertEqual(keys["compiler-filter"], "speed")
        self.assertEqual(keys["classpath"], SETTINGS_CONTEXT)
        self.assertEqual(keys["bootclasspath"], BOOTCLASSPATH)
        self.assertEqual(keys["bootclasspath-checksums"], "i;2/cb8fc4a6:i;2/f8c5165e")

    def test_fr5_vdex_checksums_match_the_apk_dex_crcs(self):
        apk = fake_apk(b"dex\n035\0first", b"dex\n035\0second", b"dex\n035\0third")
        crcs = [zlib.crc32(b"dex\n035\0first"), zlib.crc32(b"dex\n035\0second"), zlib.crc32(b"dex\n035\0third")]
        self.assertEqual(make_image.apk_dex_checksums(apk), crcs)
        self.assertEqual(make_image.vdex_checksums(fake_vdex(crcs)), crcs)

    def test_fr5_odex_sits_beside_the_apk_for_x86_64(self):
        self.assertEqual(make_image.oat_files(make_image.SETTINGS_APP), (
            "SYSTEM_EXT/priv-app/Settings/oat/x86_64/Settings.odex",
            "SYSTEM_EXT/priv-app/Settings/oat/x86_64/Settings.vdex"))
        self.assertEqual(make_image.factory_odex(make_image.SETTINGS_APP),
                         "SYSTEM_OTHER/system_ext/priv-app/Settings/oat/x86_64/Settings.odex")
        self.assertEqual(make_image.device_path("PRODUCT/app/webview/webview.apk"),
                         "/product/app/webview/webview.apk")

    def test_fr5_compiled_files_are_in_the_partition_filesystem_config(self):
        config, rules = make_image.oat_filesystem_config("SYSTEM/priv-app/DocumentsUI/DocumentsUI.apk")
        self.assertEqual(config, "META/filesystem_config.txt")
        self.assertEqual(rules, [
            "system/priv-app/DocumentsUI/oat 0 0 755 capabilities=0x0",
            "system/priv-app/DocumentsUI/oat/x86_64 0 0 755 capabilities=0x0",
            "system/priv-app/DocumentsUI/oat/x86_64/DocumentsUI.odex 0 0 644 capabilities=0x0",
            "system/priv-app/DocumentsUI/oat/x86_64/DocumentsUI.vdex 0 0 644 capabilities=0x0",
        ])
        config, _ = make_image.oat_filesystem_config(make_image.SETTINGS_APP)
        self.assertEqual(config, "META/system_ext_filesystem_config.txt")

    def test_fr5_class_loader_context_opens_host_copies_and_stores_device_paths(self):
        host, stored = make_image.class_loader_contexts(SETTINGS_CONTEXT, Path("/w/root"))
        self.assertEqual(stored, "PCL[]{PCL[/system/framework/org.apache.http.legacy.jar]#"
                                 "PCL[/system_ext/framework/androidx.window.extensions.jar]}")
        self.assertEqual(host, "PCL[]{PCL[/w/root/system/framework/org.apache.http.legacy.jar]#"
                               "PCL[/w/root/system_ext/framework/androidx.window.extensions.jar]}")
        self.assertEqual(make_image.class_loader_contexts("PCL[]", Path("/w/root")), ("PCL[]", "PCL[]"))

    def test_fr5_boot_image_is_the_primary_image_and_its_extension(self):
        self.assertEqual(
            make_image.boot_image_locations(BOOTCLASSPATH, "i;2/cb8fc4a6:i;2/f8c5165e"),
            ["/system/framework/boot.art", "/system/framework/boot-framework-adservices.art"])
        with self.assertRaises(ValueError):
            make_image.boot_image_locations(BOOTCLASSPATH, "")

    def test_fr5_boot_class_path_apex_jars_come_from_their_apexes(self):
        names = {"SYSTEM/apex/com.android.art.capex", "SYSTEM/apex/com.android.adservices.apex",
                 "SYSTEM/apex/com.android.wifi.apex"}
        self.assertEqual(make_image.apex_jar_sources(BOOTCLASSPATH, names), {
            "com.android.art": "SYSTEM/apex/com.android.art.capex",
            "com.android.adservices": "SYSTEM/apex/com.android.adservices.apex",
            "com.android.wifi": "SYSTEM/apex/com.android.wifi.apex",
        })
        with self.assertRaises(ValueError):
            make_image.apex_jar_sources(BOOTCLASSPATH, names - {"SYSTEM/apex/com.android.wifi.apex"})

    def test_fr5_dex2oat_targets_the_guest_x86_64_boot_image_as_a_factory_build(self):
        contexts = make_image.class_loader_contexts(SETTINGS_CONTEXT, Path("/r"))
        cmd = make_image.dex2oat_command(
            "dex2oatd64", "/o/Settings.apk", "/system_ext/priv-app/Settings/Settings.apk",
            "/o/Settings.odex", Path("/r"), BOOTCLASSPATH,
            ["/system/framework/boot.art", "/system/framework/boot-framework-adservices.art"],
            contexts, "speed", extra=["--runtime-arg", "-Xgc:CMC"])
        self.assertIn("--instruction-set=x86_64", cmd)
        self.assertIn("--compiler-filter=speed", cmd)
        self.assertIn("--compilation-reason=prebuilt", cmd)
        self.assertIn("--dex-location=/system_ext/priv-app/Settings/Settings.apk", cmd)
        self.assertIn("--oat-location=/system_ext/priv-app/Settings/oat/x86_64/Settings.odex", cmd)
        self.assertIn("-Xbootclasspath-locations:" + BOOTCLASSPATH, cmd)
        self.assertIn("-Xbootclasspath:/r/apex/com.android.art/javalib/core-oj.jar:/r/system/framework/framework.jar:"
                      "/r/apex/com.android.adservices/javalib/framework-adservices.jar:"
                      "/r/apex/com.android.wifi/javalib/framework-wifi.jar", cmd)
        self.assertIn("--boot-image=/r/system/framework/boot.art:/r/system/framework/boot-framework-adservices.art", cmd)
        self.assertIn(f"--stored-class-loader-context={contexts[1]}", cmd)
        self.assertIn("-Xgc:CMC", cmd)
        # dex2oat's heap must fit in the low 4 GiB: -Xmx2g aborted on the CI runner.
        self.assertIn("-Xmx512m", cmd)
        self.assertFalse(any(a.startswith("--profile-file") for a in cmd))
        with_profile = make_image.dex2oat_command(
            "dex2oatd64", "/o/D.apk", "/system/priv-app/D/D.apk", "/o/D.odex", Path("/r"), BOOTCLASSPATH,
            ["/system/framework/boot.art"], ("PCL[]", "PCL[]"), "speed-profile", profile="/o/D.prof")
        self.assertIn("--profile-file=/o/D.prof", with_profile)

    def _image(self, apk, odex, vdex, framework_checksums="i;2/cb8fc4a6:i;2/f8c5165e", factory=SETTINGS_CONTEXT):
        buffer = io.BytesIO()
        odex_name, vdex_name = make_image.oat_files(make_image.SETTINGS_APP)
        with zipfile.ZipFile(buffer, "w") as z:
            z.writestr(make_image.SETTINGS_APP, apk)
            z.writestr(make_image.FRAMEWORK_ODEX, fake_odex(**{
                "bootclasspath": BOOTCLASSPATH, "bootclasspath-checksums": framework_checksums}))
            z.writestr(make_image.factory_odex(make_image.SETTINGS_APP), fake_odex(classpath=factory, **{
                "compiler-filter": "verify"}))
            if odex is not None:
                z.writestr(odex_name, odex)
            if vdex is not None:
                z.writestr(vdex_name, vdex)
        return zipfile.ZipFile(io.BytesIO(buffer.getvalue()))

    def _settings_odex(self, filter="speed", checksums="i;2/cb8fc4a6:i;2/f8c5165e", context=SETTINGS_CONTEXT):
        return fake_odex(**{"bootclasspath": BOOTCLASSPATH, "bootclasspath-checksums": checksums,
                            "classpath": context, "compiler-filter": filter,
                            "compilation-reason": "prebuilt"})

    def test_fr5_check_accepts_settings_compiled_for_its_own_dex(self):
        apk = fake_apk(b"dex\n035\0edited", b"dex\n035\0two")
        vdex = fake_vdex(make_image.apk_dex_checksums(apk))
        self.assertEqual(make_image.compiled_app_problems(self._image(apk, self._settings_odex(), vdex)), [])

    def test_fr5_check_rejects_settings_without_an_odex(self):
        apk = fake_apk(b"dex\n035\0edited")
        problems = make_image.compiled_app_problems(self._image(apk, None, None))
        self.assertTrue(any("not compiled" in p for p in problems), problems)

    def test_fr5_check_rejects_an_odex_made_for_the_unedited_dex(self):
        apk = fake_apk(b"dex\n035\0edited")
        vdex = fake_vdex([zlib.crc32(b"dex\n035\0original")])
        problems = make_image.compiled_app_problems(self._image(apk, self._settings_odex(), vdex))
        self.assertTrue(any("dex checksums" in p for p in problems), problems)

    def test_fr5_check_rejects_verify_another_boot_image_or_other_libraries(self):
        apk = fake_apk(b"dex\n035\0edited")
        vdex = fake_vdex(make_image.apk_dex_checksums(apk))
        for odex, words in [
            (self._settings_odex(filter="verify"), "not speed"),
            (self._settings_odex(filter="speed-profile"), "not speed"),
            (self._settings_odex(checksums="i;2/00000000:i;2/f8c5165e"), "boot image"),
            (self._settings_odex(context="PCL[]"), "shared libraries"),
        ]:
            problems = make_image.compiled_app_problems(self._image(apk, odex, vdex))
            self.assertTrue(any(words in p for p in problems), (words, problems))

    def test_fr5_inputs_pin_the_build_dexpreopt_tools(self):
        inputs = json.loads((Path(make_image.__file__).parent / "inputs.json").read_text())
        for image in inputs["images"]:
            tools = image["files"]["dexpreopt_tools"]
            self.assertEqual(tools["name"], "dexpreopt_tools.zip")
            self.assertRegex(tools["sha256"], r"^[0-9a-f]{64}$")
        fetch = (Path(make_image.__file__).parent / "fetch.sh").read_text()
        self.assertIn(".files | keys[]", fetch)


ANDROID_ID = 0x010100D0
ANDROID_PADDING_TOP = 0x010100D7
ANDROID_PADDING_BOTTOM = 0x010100D9
ANDROID_VISIBILITY = 0x010100DC
ANDROID_CLIP_TO_PADDING = 0x010100EB
RECYCLER_VIEW = "androidx.recyclerview.widget.RecyclerView"
TYPE_REFERENCE, TYPE_DIMENSION, TYPE_INT_DEC, TYPE_BOOLEAN = 0x01, 0x05, 0x10, 0x12


def compiled_layout(root):
    """Compiles a layout, (tag, [(name, resource id, type, data)], [children]), the way aapt2
    lays one out: the framework attribute names first in the string pool, with their
    resource ids in the resource map, and each element's attributes in resource id order."""
    attributes = {}

    def collect(node):
        tag, attrs, children = node
        for name, resource_id, _, _ in attrs:
            attributes[resource_id] = name
        for child in children:
            collect(child)

    collect(root)
    strings = [attributes[i] for i in sorted(attributes)]

    def index(text):
        if text not in strings:
            strings.append(text)
        return strings.index(text)

    index("android")
    index(ANDROID_NS)
    data, offsets = b"", []

    def element(node):
        tag, attrs, children = node
        body = struct.pack("<IIHHHHHH", 0xFFFFFFFF, index(tag), 20, 20, len(attrs),
                           1 + [a[1] for a in sorted(attrs, key=lambda a: a[1])].index(ANDROID_ID)
                           if any(a[1] == ANDROID_ID for a in attrs) else 0, 0, 0)
        for name, resource_id, kind, value in sorted(attrs, key=lambda a: a[1]):
            body += struct.pack("<IIIHBBI", index(ANDROID_NS), index(name), 0xFFFFFFFF, 8, 0, kind, value)
        start = struct.pack("<HHIII", 0x0102, 16, 16 + len(body), 1, 0xFFFFFFFF) + body
        end_body = struct.pack("<II", 0xFFFFFFFF, index(tag))
        end = struct.pack("<HHIII", 0x0103, 16, 16 + len(end_body), 1, 0xFFFFFFFF) + end_body
        return start + b"".join(element(child) for child in children) + end

    elements_data = element(root)
    for text in strings:
        offsets.append(len(data))
        encoded = text.encode("utf-8")
        data += bytes([len(text), len(encoded)]) + encoded + b"\0"
    data += b"\0" * (-len(data) % 4)
    strings_start = 28 + 4 * len(strings)
    pool = struct.pack("<HHIIIIII", 0x0001, 28, strings_start + len(data), len(strings), 0, 0x100,
                       strings_start, 0)
    pool += b"".join(struct.pack("<I", offset) for offset in offsets) + data
    ids = [i for i in sorted(attributes)]
    resource_map = struct.pack("<HHI", 0x0180, 8, 8 + 4 * len(ids)) + struct.pack(f"<{len(ids)}I", *ids)
    namespace = struct.pack("<II", index("android"), index(ANDROID_NS))
    ns_start = struct.pack("<HHIII", 0x0100, 16, 24, 1, 0xFFFFFFFF) + namespace
    ns_end = struct.pack("<HHIII", 0x0101, 16, 24, 1, 0xFFFFFFFF) + namespace
    tree = pool + resource_map + ns_start + elements_data + ns_end
    return struct.pack("<HHI", 0x0003, 8, 8 + len(tree)) + tree


def layout_attributes(axml):
    """[(tag, id index, [(name, type, data)])] of each element in a compiled layout, in order."""
    strings = make_image.xml_strings(axml, 8)
    found = []
    offset = 8
    while offset < len(axml):
        chunk_type, header_size, size = struct.unpack_from("<HHI", axml, offset)
        if chunk_type == 0x0102:
            _, name, start, attribute_size, count, id_index = struct.unpack_from("<IIHHHH", axml, offset + header_size)
            attrs = []
            for i in range(count):
                _, attribute, _, _, _, kind, value = struct.unpack_from(
                    "<IIIHBBI", axml, offset + header_size + start + i * attribute_size)
                attrs.append((strings[attribute], kind, value))
            found.append((strings[name], id_index, attrs))
        offset += size
    return found


def dp(value):
    return value << 8 | 0x01


ALL_APPS_LAYOUT = ("androidx.constraintlayout.widget.ConstraintLayout", [], [
    (RECYCLER_VIEW, [("id", ANDROID_ID, TYPE_REFERENCE, 0x7F0A00DA),
                     ("visibility", ANDROID_VISIBILITY, TYPE_INT_DEC, 1),
                     ("clipToPadding", ANDROID_CLIP_TO_PADDING, TYPE_BOOLEAN, 0)], []),
    ("LinearLayout", [("id", ANDROID_ID, TYPE_REFERENCE, 0x7F0A07E9),
                      ("paddingTop", ANDROID_PADDING_TOP, TYPE_DIMENSION, dp(12)),
                      ("paddingBottom", ANDROID_PADDING_BOTTOM, TYPE_DIMENSION, dp(12))], []),
])
PREFERENCE_LIST_LAYOUT = (RECYCLER_VIEW, [("id", ANDROID_ID, TYPE_REFERENCE, 0x7F0A03AB),
                                          ("paddingTop", ANDROID_PADDING_TOP, TYPE_DIMENSION, dp(0)),
                                          ("paddingBottom", ANDROID_PADDING_BOTTOM, TYPE_DIMENSION, dp(0)),
                                          ("clipToPadding", ANDROID_CLIP_TO_PADDING, TYPE_BOOLEAN, 0)], [])

TOP_LEVEL_SETTINGS = "Lcom/android/settings/homepage/TopLevelSettings;"
DOCUMENTS = "Lcom/android/documentsui"
SAVE_LAYOUT_HEIGHT = f"{DOCUMENTS}/dirlist/DirectoryFragment;->getSaveLayoutHeight"


def top_level_settings_dex():
    """Android 16's and Android 17's TopLevelSettings methods that set the home list's padding,
    as d8 compiled them in Settings.apk, cut down to the instructions that matter."""
    methods = [
        (TOP_LEVEL_SETTINGS, "onCreateRecyclerView", "LLLL"),
        ("Landroidx/preference/PreferenceFragmentCompat;", "onCreateRecyclerView", "LLLL"),
        ("Landroid/view/ViewGroup;", "setVerticalScrollBarEnabled", "VZ"),
        ("Landroid/view/ViewGroup;", "setPadding", "VIIII"),
        (TOP_LEVEL_SETTINGS, "isRunningInFreeformWindow", "ZL"),
        ("Landroid/view/View;", "getRootWindowInsets", "L"),
        ("Landroid/view/WindowInsets$Type;", "captionBar", "I"),
        ("Landroid/view/WindowInsets;", "isVisible", "ZI"),
    ]
    on_create_recycler_view = bytes.fromhex(
        "6f400100 1032"  # invoke-super {v0, v1, v2, v3}, PreferenceFragmentCompat.onCreateRecyclerView
        "0c01"  # move-result-object v1
        "1202"  # const/4 v2, 0
        "6e200200 2100"  # invoke-virtual {v1, v2}, setVerticalScrollBarEnabled
        "6e520300 0102"  # invoke-virtual {v1, v0, v2, v0, v2}, setPadding (0, 0, 0, 0 on Android 16)
        "1101"  # return-object v1
    )
    is_running_in_freeform_window = bytes.fromhex(
        "6e100500 0100"  # invoke-virtual {v1}, getRootWindowInsets
        "0c00"  # move-result-object v0
        "38000b00"  # if-eqz v0, :none
        "71000600 0000"  # invoke-static {}, captionBar
        "0a01"  # move-result v1
        "6e200700 1000"  # invoke-virtual {v0, v1}, isVisible
        "0a00"  # move-result v0
        "0f00"  # return v0
        "1200"  # :none const/4 v0, 0
        "0f00"  # return v0
    )
    return dex_file(methods, {0: (4, on_create_recycler_view), 4: (2, is_running_in_freeform_window)})


DOCUMENTS_METHODS = [
    (f"{DOCUMENTS}/dirlist/DirectoryFragment;", "getSaveLayoutHeight", "I"),
    (f"{DOCUMENTS}/util/FlagUtils;", "isUseMaterial3FlagEnabled", "Z"),
    (f"{DOCUMENTS}/dirlist/DirectoryFragment;", "getBaseActivity", "L"),
    (f"{DOCUMENTS}/util/Material3Config;", "getRes", "II"),
    ("Landroidx/appcompat/app/AppCompatActivity;", "findViewById", "LI"),
    ("Landroid/view/View;", "getHeight", "I"),
    ("Landroidx/fragment/app/Fragment;", "getResources", "L"),
    ("Landroid/content/res/Resources;", "getDisplayMetrics", "L"),
]
DOCUMENTS_FIELDS = [
    (f"{DOCUMENTS}/R$id;", "bottom_container", "I"),
    (f"{DOCUMENTS}/R$id;", "container_save", "I"),
    ("Landroid/util/DisplayMetrics;", "density", "F"),
]


def documentsui_dex(code=None):
    """DirectoryFragment.getSaveLayoutHeight in Android 16's DocumentsUI.apk, as d8 compiled it:
    the height of the bar below the list (the Material 3 bottom bar, or the save bar), or 0
    without one."""
    def bar(field):
        return (
            "6e100200 0200"  # invoke-virtual {v2}, getBaseActivity
            "0c02"  # move-result-object v2
            f"6000{field:02x}00"  # sget v0, the bar's id
            "71100300 0000"  # invoke-static {v0}, Material3Config.getRes
            "0a00"  # move-result v0
            "6e200400 0200"  # invoke-virtual {v2, v0}, findViewById
            "0c02"  # move-result-object v2
            "39020300"  # if-nez v2, :bar
            "0f01"  # return v1 (0)
            "6e100500 0200"  # :bar invoke-virtual {v2}, getHeight
            "0a02"  # move-result v2
            "0f02"  # return v2
        )
    original = bytes.fromhex(
        "71000100 0000"  # invoke-static {}, isUseMaterial3FlagEnabled
        "0a00"  # move-result v0
        "1201"  # const/4 v1, 0
        "38001800"  # if-eqz v0, :legacy
        + bar(0) + bar(1))
    return dex_file(DOCUMENTS_METHODS, {0: (3, code or original)}, DOCUMENTS_FIELDS)


def run_dalvik(dex, method, args, calls, statics):
    """Runs a method of a dex file with the few instructions these edits use: `args` are its
    arguments, `calls` gives what each method it calls returns ({"Lclass;->name": function}),
    and `statics` each static field's value. Objects are dicts of their fields."""
    defined, refs = make_image.dex_methods(dex)
    _, _, fields = make_image.dex_ids(dex)
    (code_off, _), = defined[method]
    registers, ins = struct.unpack_from("<HH", dex, code_off)
    (count,) = struct.unpack_from("<I", dex, code_off + 12)
    code = struct.unpack_from(f"<{count}H", dex, code_off + 16)
    v = [None] * (registers - ins) + list(args)
    result, pc = None, 0

    def f32(x):
        return struct.unpack("<f", struct.pack("<f", x))[0]

    def s16(x):
        return x - 0x10000 if x & 0x8000 else x

    while True:
        unit = code[pc]
        op, a, b, aa = unit & 0xFF, unit >> 8 & 0xF, unit >> 12, unit >> 8
        if op == 0x00:
            pc += 1
        elif op in (0x0A, 0x0C):
            v[aa], pc = result, pc + 1
        elif op == 0x0F:
            return v[aa]
        elif op == 0x12:
            v[a], pc = (b ^ 8) - 8, pc + 1
        elif op == 0x15:
            v[aa], pc = struct.unpack("<f", struct.pack("<I", code[pc + 1] << 16))[0], pc + 2
        elif op == 0x28:
            pc += aa - 0x100 if aa & 0x80 else aa
        elif op == 0x35:
            pc += s16(code[pc + 1]) if v[a] >= v[b] else 2
        elif op in (0x38, 0x39):
            zero = v[aa] in (0, None, False)
            pc += s16(code[pc + 1]) if zero == (op == 0x38) else 2
        elif op == 0x52:
            v[a], pc = v[b][fields[code[pc + 1]].split("->")[1]], pc + 2
        elif op == 0x60:
            v[aa], pc = statics[fields[code[pc + 1]]], pc + 2
        elif op in (0x6E, 0x71):
            passed = [code[pc + 2] >> (4 * i) & 0xF for i in range(4)] + [a]
            result = calls[refs[code[pc + 1]]](*[v[r] for r in passed[:b]])
            pc += 3
        elif op == 0x87:
            v[a], pc = int(v[b]), pc + 1
        elif op == 0xC6:
            v[a], pc = f32(v[a] + v[b]), pc + 1
        elif op == 0xC8:
            v[a], pc = f32(v[a] * v[b]), pc + 1
        else:
            raise AssertionError(f"opcode {op:#x} is not one the edits use")


def save_layout_height(dex, material3, bars, density):
    """What getSaveLayoutHeight returns in a window whose bars ({id field: height}) are there,
    on a display of `density`."""
    ids = {f"{DOCUMENTS}/R$id;->bottom_container": 1, f"{DOCUMENTS}/R$id;->container_save": 2}
    names = {1: "bottom_container", 2: "container_save"}
    activity = {"bars": bars}
    calls = {
        f"{DOCUMENTS}/util/FlagUtils;->isUseMaterial3FlagEnabled": lambda: material3,
        f"{DOCUMENTS}/dirlist/DirectoryFragment;->getBaseActivity": lambda this: activity,
        f"{DOCUMENTS}/util/Material3Config;->getRes": lambda res: res,
        "Landroidx/appcompat/app/AppCompatActivity;->findViewById": (
            lambda act, res: {"height": act["bars"][names[res]]} if names[res] in act["bars"] else None),
        "Landroid/view/View;->getHeight": lambda view: view["height"],
        "Landroidx/fragment/app/Fragment;->getResources": lambda this: {"density": density},
        "Landroid/content/res/Resources;->getDisplayMetrics": lambda res: {"density": res["density"]},
    }
    return run_dalvik(dex, SAVE_LAYOUT_HEIGHT, [{"fragment": True}], calls, ids)


class ListBottomMarginTests(unittest.TestCase):
    """The lists that set their own padding, or that no overlay may reach, keep room below
    their last row in a window without a navigation bar."""

    @staticmethod
    def apk(files):
        return SettingsWithoutBluetoothTests.apk(files)

    def modified(self, files):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                for name, data in files.items():
                    archive.writestr(name, data)
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                return {name: archive.read(name) for name in archive.namelist()}

    def test_fr22_settings_home_list_on_android_16_keeps_the_room_its_layout_gives(self):
        # TopLevelSettings.onCreateRecyclerView set the list's padding to (0, 0, 0, 0) over the
        # layout's, which the Settings overlay gives 24 dp at the bottom.
        self.assertIn(("skip call", "Landroid/view/ViewGroup;->setPadding"), make_image.SETTINGS_CODE_LIST_MARGIN[
            f"{TOP_LEVEL_SETTINGS}->onCreateRecyclerView"])
        dex = top_level_settings_dex()
        settings = self.modified({make_image.SETTINGS_APP: self.apk({"classes2.dex": dex})})[make_image.SETTINGS_APP]
        with zipfile.ZipFile(io.BytesIO(settings)) as archive:
            edited = archive.read("classes2.dex")
        original = dex_code(dex, f"{TOP_LEVEL_SETTINGS}->onCreateRecyclerView")
        code = dex_code(edited, f"{TOP_LEVEL_SETTINGS}->onCreateRecyclerView")
        # Only the setPadding call (three code units, from unit 8) is gone; the scroll bar stays off.
        self.assertEqual(code[:16], original[:16])
        self.assertEqual(code[16:22], bytes(6))
        self.assertEqual(code[22:], original[22:])
        SettingsWithoutBluetoothTests.assert_dex_is_sealed(self, edited)
        self.assertEqual(make_image.apk_edits_left(settings, code=make_image.SETTINGS_CODE_LIST_MARGIN), [])
        self.assertNotEqual(make_image.apk_edits_left(self.apk({"classes2.dex": dex}),
                                                      code=make_image.SETTINGS_CODE_LIST_MARGIN), [])

    def test_fr22_settings_home_list_on_android_17_takes_its_freeform_padding_at_24_dp(self):
        # Android 17 pads the home list's bottom by dashboard_padding_bottom (16 dp) only when
        # the window shows a caption bar, which a DSA window's host draws outside the guest.
        dex = top_level_settings_dex()
        edited, changed = make_image.edit_dex(dex, make_image.SETTINGS_CODE_LIST_MARGIN)
        self.assertIn(f"{TOP_LEVEL_SETTINGS}->isRunningInFreeformWindow", changed)
        code = dex_code(edited, f"{TOP_LEVEL_SETTINGS}->isRunningInFreeformWindow")
        self.assertEqual(code[:4], bytes.fromhex("12100f00"))  # const/4 v0, 1; return v0
        self.assertEqual(make_image.edit_dex(edited, make_image.SETTINGS_CODE_LIST_MARGIN)[1], [])
        ns = "{http://schemas.android.com/apk/res/android}"
        for home_padding in (True, False):
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                make_image.settings_overlay_sources(root / "src", home_padding=home_padding)
                values = ET.fromstring((root / "src/res/values/dsa.xml").read_text())
                dimens = {d.get("name"): d.text for d in values.findall("dimen")}
                manifest = ET.fromstring((root / "src/AndroidManifest.xml").read_text())
                resources_map = manifest.find("overlay").get(ns + "resourcesMap").removeprefix("@xml/")
                mapping = ET.fromstring((root / "src/res/xml" / (resources_map + ".xml")).read_text())
                targets = [item.get("target") for item in mapping.findall("item")]
            if home_padding:
                self.assertEqual(dimens, {"dashboard_padding_bottom": "24dp"})
                self.assertIn("dimen/dashboard_padding_bottom", targets)
            else:
                # Android 16's Settings has no such dimen, and idmap2 would warn on every boot.
                self.assertEqual(dimens, {})
                self.assertNotIn("dimen/dashboard_padding_bottom", targets)

    def test_fr22_the_overlay_pads_the_home_list_only_for_a_settings_that_reads_the_dimen(self):
        methods = [(TOP_LEVEL_SETTINGS, "setPaddingHorizontal", "VI")]
        with_dimen = dex_file(methods, {0: (1, bytes.fromhex("0e00"))},
                              [("Lcom/android/settings/R$dimen;", "dashboard_padding_bottom", "I")])
        without = dex_file(methods, {0: (1, bytes.fromhex("0e00"))})
        self.assertTrue(make_image.settings_has_home_padding(self.apk({"classes2.dex": with_dimen})))
        self.assertFalse(make_image.settings_has_home_padding(self.apk({"classes2.dex": without})))

    def test_fr22_settings_all_apps_list_keeps_room_below_its_last_row(self):
        # manage_applications_apps' RecyclerView has no padding, and nothing sets it in code.
        layout = compiled_layout(ALL_APPS_LAYOUT)
        settings = self.modified({make_image.SETTINGS_APP: self.apk({
            "res/layout/manage_applications_apps.xml": layout})})[make_image.SETTINGS_APP]
        with zipfile.ZipFile(io.BytesIO(settings)) as archive:
            edited = archive.read("res/layout/manage_applications_apps.xml")
        found = layout_attributes(edited)
        self.assertEqual(found[1], (RECYCLER_VIEW, 1, [
            ("id", TYPE_REFERENCE, 0x7F0A00DA),
            ("paddingBottom", TYPE_DIMENSION, dp(24)),
            ("visibility", TYPE_INT_DEC, 1),
            ("clipToPadding", TYPE_BOOLEAN, 0),
        ]))
        # The other elements are as they were.
        self.assertEqual(found[2], layout_attributes(layout)[2])
        self.assertEqual(struct.unpack_from("<I", edited, 4)[0], len(edited))
        self.assertEqual(make_image.set_xml_dimensions(edited, make_image.SETTINGS_LIST_DIMENSIONS[
            "res/layout/manage_applications_apps.xml"])[1], 0)
        self.assertEqual(make_image.apk_edits_left(settings, dimensions=make_image.SETTINGS_LIST_DIMENSIONS), [])

    def test_fr22_an_attribute_the_layout_never_names_is_refused(self):
        layout = compiled_layout((RECYCLER_VIEW, [("id", ANDROID_ID, TYPE_REFERENCE, 1)], []))
        with self.assertRaises(ValueError):
            make_image.set_xml_dimensions(layout, [(RECYCLER_VIEW, ANDROID_PADDING_BOTTOM, 24)])

    def test_fr22_permission_controller_lists_keep_room_below_their_last_row(self):
        # PermissionController's preference_recyclerview has paddingBottom 0dp, and the app's
        # <overlayable> keeps any overlay from replacing it.
        apk = self.apk({"res/layout/preference_recyclerview.xml": compiled_layout(PREFERENCE_LIST_LAYOUT)})
        edited = make_image.edit_permission_controller_apk(apk, "PermissionController.apk")
        with zipfile.ZipFile(io.BytesIO(edited)) as archive:
            (tag, _, attrs), = layout_attributes(archive.read("res/layout/preference_recyclerview.xml"))
        self.assertEqual(tag, RECYCLER_VIEW)
        self.assertIn(("paddingBottom", TYPE_DIMENSION, dp(24)), attrs)
        self.assertIn(("paddingTop", TYPE_DIMENSION, dp(0)), attrs)
        self.assertIn(("clipToPadding", TYPE_BOOLEAN, 0), attrs)
        self.assertEqual(make_image.apk_edits_left(edited, dimensions=make_image.PERMISSION_CONTROLLER_DIMENSIONS), [])
        self.assertNotEqual(make_image.apk_edits_left(apk, dimensions=make_image.PERMISSION_CONTROLLER_DIMENSIONS), [])

    @staticmethod
    def fake_apex_tools(calls):
        """deapexer, apexer and apex_compression_tool for an APEX whose payload image is a zip
        of its files. Each command is added to `calls`."""
        def run(*cmd, **kw):
            calls.append([str(c) for c in cmd])
            tool = Path(str(cmd[0])).name
            if tool == "deapexer":
                apex, out = Path(cmd[-2]), Path(cmd[-1])
                with zipfile.ZipFile(apex) as outer, zipfile.ZipFile(io.BytesIO(outer.read("apex_payload.img"))) as payload:
                    payload.extractall(out)
            elif tool == "apexer":
                folder, image = Path(cmd[-2]), Path(cmd[-1])
                with zipfile.ZipFile(image, "w") as payload:
                    for f in sorted(folder.rglob("*")):
                        if f.is_file():
                            payload.write(f, f.relative_to(folder).as_posix())
            elif tool == "apex_compression_tool":
                source, out = Path(cmd[cmd.index("--input") + 1]), Path(cmd[cmd.index("--output") + 1])
                with zipfile.ZipFile(out, "w") as capex:
                    capex.writestr("original_apex", source.read_bytes())
            else:
                raise AssertionError(f"unexpected tool {tool}")
        return run

    @staticmethod
    def permission_capex(controller):
        payload = io.BytesIO()
        with zipfile.ZipFile(payload, "w") as image:
            image.writestr("apex_manifest.pb", b"manifest")
            image.writestr("priv-app/PermissionController@BP4A/PermissionController.apk", controller)
            image.writestr("priv-app/SafetyCenterResources@BP4A/SafetyCenterResources.apk", b"resources")
        apex = io.BytesIO()
        with zipfile.ZipFile(apex, "w") as outer:
            outer.writestr("apex_manifest.pb", b"manifest")
            outer.writestr("apex_build_info.pb", b"build info")
            outer.writestr("apex_payload.img", payload.getvalue())
        capex = io.BytesIO()
        with zipfile.ZipFile(capex, "w") as outer:
            outer.writestr("original_apex", apex.getvalue())
        return capex.getvalue()

    @staticmethod
    def capex_payload(capex):
        with zipfile.ZipFile(io.BytesIO(capex)) as outer, zipfile.ZipFile(io.BytesIO(outer.read("original_apex"))) as apex:
            with zipfile.ZipFile(io.BytesIO(apex.read("apex_payload.img"))) as payload:
                return {n: payload.read(n) for n in payload.namelist()}, apex.read("apex_build_info.pb")

    def test_fr22_the_permission_apex_is_packed_again_with_the_edited_controller(self):
        controller = self.apk({"res/layout/preference_recyclerview.xml": compiled_layout(PREFERENCE_LIST_LAYOUT)})
        capex = self.permission_capex(controller)
        name = "SYSTEM/apex/com.android.permission.capex"
        self.assertIn(name, make_image.PERMISSION_APEXES)
        calls = []
        with tempfile.TemporaryDirectory() as tmp, patch.object(make_image, "run", self.fake_apex_tools(calls)):
            edited = make_image.edit_permission_apex(capex, name, Path(tmp) / "otatools", Path(tmp))
            files, build_info = self.capex_payload(edited)
            apexer, = [c for c in calls if Path(c[0]).name == "apexer"]
            self.assertIn("--payload_only", apexer)
            # The APEX's own manifest and build information (file contexts, filesystem,
            # ownership), as signing packs it.
            self.assertEqual(Path(apexer[apexer.index("--build_info") + 1]).name, "apex_build_info.pb")
            self.assertEqual(Path(apexer[apexer.index("--manifest") + 1]).name, "apex_manifest.pb")
            self.assertTrue(apexer[apexer.index("--key") + 1].endswith("testkey_rsa4096.pem"))
            self.assertTrue(any(Path(c[0]).name == "apex_compression_tool" for c in calls))
            # Signing finds the problems that remain in what it packed.
            signed = Path(tmp) / "signed.zip"
            for apex, found in [(edited, False), (capex, True)]:
                with zipfile.ZipFile(signed, "w") as z:
                    z.writestr(name, apex)
                problems = make_image.permission_controller_problems(signed, Path(tmp) / "otatools", Path(tmp), {})
                self.assertEqual(bool(problems), found, problems)
        self.assertEqual(build_info, b"build info")
        # apexer writes the manifest into the payload itself.
        self.assertNotIn("apex_manifest.pb", files)
        self.assertEqual(files["priv-app/SafetyCenterResources@BP4A/SafetyCenterResources.apk"], b"resources")
        with zipfile.ZipFile(io.BytesIO(files["priv-app/PermissionController@BP4A/PermissionController.apk"])) as apk:
            (_, _, attrs), = layout_attributes(apk.read("res/layout/preference_recyclerview.xml"))
        self.assertIn(("paddingBottom", TYPE_DIMENSION, dp(24)), attrs)

    def test_fr22_without_otatools_the_permission_apex_is_left_as_it_is(self):
        capex = self.permission_capex(b"controller")
        with patch.object(make_image, "run") as run:
            self.assertEqual(make_image.edit_permission_apex(
                capex, "SYSTEM/apex/com.android.permission.capex", None, Path(".")), capex)
        run.assert_not_called()

    def test_fr22_files_on_android_16_keeps_room_below_its_list_without_a_bar_below_it(self):
        # DirectoryFragment pads its list's bottom by getSaveLayoutHeight: the bar below the
        # list, or 0 in a wide window that has none.
        dex = documentsui_dex()
        self.assertEqual(save_layout_height(dex, True, {}, 1.5), 0)
        docs = "SYSTEM/priv-app/DocumentsUI/DocumentsUI.apk"
        apk = self.modified({docs: self.apk({"classes.dex": dex})})[docs]
        with zipfile.ZipFile(io.BytesIO(apk)) as archive:
            edited = archive.read("classes.dex")
        SettingsWithoutBluetoothTests.assert_dex_is_sealed(self, edited)
        self.assertEqual(len(dex_code(edited, SAVE_LAYOUT_HEIGHT)), len(dex_code(dex, SAVE_LAYOUT_HEIGHT)))
        for material3 in (True, False):
            bar = "bottom_container" if material3 else "container_save"
            with self.subTest(material3=material3):
                # 24 dp, rounded as getDimensionPixelSize rounds.
                self.assertEqual(save_layout_height(edited, material3, {}, 1.0), 24)
                self.assertEqual(save_layout_height(edited, material3, {}, 1.5), 36)
                self.assertEqual(save_layout_height(edited, material3, {}, 2.625), 63)
                # A bar taller than the margin keeps the list above it.
                self.assertEqual(save_layout_height(edited, material3, {bar: 120}, 1.5), 120)
                self.assertEqual(save_layout_height(edited, material3, {bar: 0}, 1.5), 36)
        self.assertEqual(make_image.edit_dex(edited, make_image.DOCUMENTSUI_CODE)[1], [])
        self.assertEqual(make_image.apk_edits_left(apk, code=make_image.DOCUMENTSUI_CODE), [])

    def test_fr22_a_files_app_built_from_other_sources_is_refused(self):
        dex = documentsui_dex(code=bytes.fromhex("1200" "0f00" + "0000" * 49))
        with self.assertRaises(ValueError):
            make_image.edit_dex(dex, make_image.DOCUMENTSUI_CODE)
        self.assertNotEqual(make_image.apk_edits_left(self.apk({"classes.dex": dex}),
                                                      code=make_image.DOCUMENTSUI_CODE), [])

    def test_fr22_dalvik_assembly_matches_what_d8_writes(self):
        dex = documentsui_dex()
        code, outs = make_image.assemble_dex([
            "invoke-static {}, Lcom/android/documentsui/util/FlagUtils;->isUseMaterial3FlagEnabled",
            "move-result v0",
            "const/4 v1, 0",
            "if-eqz v0, :next",
            "sget v0, Lcom/android/documentsui/R$id;->container_save",
            "invoke-virtual {v2, v0}, Landroidx/appcompat/app/AppCompatActivity;->findViewById",
            ":next",
            "const/high16 v1, 0x41c00000",
            "if-ge v2, v0, :next",
            "goto :next",
        ], dex)
        self.assertEqual(code, bytes.fromhex(
            "71000100 0000" "0a00" "1201" "38000700" "60000100" "6e200400 0200"
            "1501c041" "3502feff" "28fc"))
        self.assertEqual(outs, 2)
        with self.assertRaises(ValueError):
            make_image.assemble_dex(["sget v0, Lcom/android/documentsui/R$id;->missing"], dex)

    def test_fr22_check_image_rejects_lists_without_their_room(self):
        check_script = Path(__file__).with_name("check-image.py")
        settings = self.apk({"classes2.dex": top_level_settings_dex(),
                             "res/layout/manage_applications_apps.xml": compiled_layout(ALL_APPS_LAYOUT)})
        docs = self.apk({"classes.dex": documentsui_dex()})
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            outputs = []
            for settings_apk, docs_apk in [(settings, docs), (make_image.edit_settings_apk(settings, "Settings.apk"),
                                                              make_image.edit_documentsui_apk(docs, "DocumentsUI.apk"))]:
                with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                    archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                    archive.writestr("META/apkcerts.txt", "")
                    archive.writestr(make_image.SETTINGS_APP, settings_apk)
                    archive.writestr("SYSTEM/priv-app/DocumentsUI/DocumentsUI.apk", docs_apk)
                outputs.append(subprocess.run([sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                                              capture_output=True, text=True).stdout)
        self.assertIn("TopLevelSettings;->onCreateRecyclerView", outputs[0])
        self.assertIn("manage_applications_apps.xml", outputs[0])
        self.assertIn("getSaveLayoutHeight", outputs[0])
        self.assertNotIn("sits on the window's bottom edge", outputs[1])


PUBLISHED_KEYCHAIN = "SYSTEM/app/KeyChain/KeyChain.apk"


class PublishedOdexTests(unittest.TestCase):
    """The apps the published build shipped compiled keep a valid odex in the image."""

    def published(self, apk, filter="speed", checksums="i;2/cb8fc4a6:i;2/f8c5165e", art=False, vdex_for=None):
        files = {
            apk: fake_apk(b"dex\n035\0keychain"),
            make_image.FRAMEWORK_ODEX: fake_odex(**{"bootclasspath": BOOTCLASSPATH,
                                                     "bootclasspath-checksums": "i;2/cb8fc4a6:i;2/f8c5165e"}),
        }
        odex, vdex = make_image.oat_files(apk)
        files[odex] = fake_odex(**{"bootclasspath": BOOTCLASSPATH, "bootclasspath-checksums": checksums,
                                    "classpath": "PCL[]", "compiler-filter": filter})
        files[vdex] = fake_vdex(make_image.apk_dex_checksums(vdex_for or files[apk]))
        if art:
            files[odex[:-len(".odex")] + ".art"] = b"art"
        return files

    @staticmethod
    def archive(files):
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as z:
            for name, data in files.items():
                z.writestr(name, data)
        return zipfile.ZipFile(io.BytesIO(buffer.getvalue()))

    def test_fr5_the_published_builds_own_odex_files_are_recorded(self):
        files = self.published(PUBLISHED_KEYCHAIN, art=True)
        # system_other is not on the guest's disk, and a removed app is not in the image.
        files["SYSTEM_OTHER/system/app/Stk/oat/x86_64/Stk.odex"] = fake_odex(**{"compiler-filter": "verify"})
        files["SYSTEM/app/Stk/Stk.apk"] = fake_apk(b"dex\n035\0stk")
        removed = "VENDOR/priv-app/CuttlefishService/CuttlefishService.apk"
        files[removed] = fake_apk(b"dex\n035\0cf")
        files["VENDOR/priv-app/CuttlefishService/oat/x86_64/CuttlefishService.odex"] = fake_odex(
            **{"compiler-filter": "verify"})
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                for name, data in files.items():
                    archive.writestr(name, data)
            make_image.modify(source, root / "modified.zip")
            with zipfile.ZipFile(root / "modified.zip") as archive:
                record = json.loads(archive.read(make_image.PREBUILT_OAT_RECORD))
                self.assertEqual(make_image.prebuilt_oat_problems(archive), [])
        self.assertEqual(record, {PUBLISHED_KEYCHAIN: {
            "files": ["SYSTEM/app/KeyChain/oat/x86_64/KeyChain.odex", "SYSTEM/app/KeyChain/oat/x86_64/KeyChain.vdex",
                      "SYSTEM/app/KeyChain/oat/x86_64/KeyChain.art"],
            "filter": "speed", "classpath": "PCL[]"}})

    def record(self, files, art=False):
        record = make_image.prebuilt_oat(files, lambda n: files[n])
        files[make_image.PREBUILT_OAT_RECORD] = json.dumps(record).encode()
        return files

    def test_fr5_an_app_whose_dex_changed_is_compiled_again_as_the_published_build_compiled_it(self):
        unchanged = self.record(self.published(PUBLISHED_KEYCHAIN))
        self.assertEqual(make_image.compile_jobs(unchanged, lambda n: unchanged[n]), [])
        edited = self.record(self.published(PUBLISHED_KEYCHAIN, art=True))
        edited[PUBLISHED_KEYCHAIN] = fake_apk(b"dex\n035\0edited")
        self.assertEqual(make_image.compile_jobs(edited, lambda n: edited[n]),
                         [(PUBLISHED_KEYCHAIN, "speed", "PCL[]", True)])
        cmd = make_image.dex2oat_command(
            "dex2oatd64", "/o/KeyChain.apk", "/system/app/KeyChain/KeyChain.apk", "/o/KeyChain.odex", Path("/r"),
            BOOTCLASSPATH, ["/system/framework/boot.art"], ("PCL[]", "PCL[]"), "speed", app_image="/o/KeyChain.art")
        self.assertIn("--app-image-file=/o/KeyChain.art", cmd)
        self.assertIn("--compiler-filter=speed", cmd)
        # The apps users open come first, with their own filters.
        settings = {make_image.SETTINGS_APP: fake_apk(b"dex\n035\0settings")}
        self.assertEqual(make_image.compile_jobs(settings, lambda n: settings[n]),
                         [(make_image.SETTINGS_APP, None, make_image.DEFAULT_CLASS_LOADER_CONTEXT, False)])

    def test_fr5_the_apps_whose_dex_is_edited_are_compiled_after_the_edits(self):
        # Their published odex, in system_other, was made for the unedited dex; dex2oat
        # verifies the edited methods as it compiles them.
        for apk in make_image.EDITED_APPS:
            self.assertIn(apk, make_image.COMPILED_APPS)
        self.assertIn("SYSTEM/priv-app/DocumentsUI/DocumentsUI.apk", make_image.EDITED_APPS)
        self.assertIn(make_image.SETTINGS_APP, make_image.EDITED_APPS)

    def test_fr5_check_rejects_a_published_odex_that_is_gone_or_no_longer_matches(self):
        good = self.record(self.published(PUBLISHED_KEYCHAIN))
        self.assertEqual(make_image.prebuilt_oat_problems(self.archive(good)), [])
        odex, vdex = make_image.oat_files(PUBLISHED_KEYCHAIN)
        for change, words in [
            (lambda f: f.pop(odex), "lost"),
            (lambda f: f.pop(PUBLISHED_KEYCHAIN), "is gone"),
            (lambda f: f.update({vdex: fake_vdex([1])}), "dex checksums"),
            (lambda f: f.update({odex: fake_odex(**{"compiler-filter": "verify", "bootclasspath-checksums":
                                                    "i;2/cb8fc4a6:i;2/f8c5165e"})}), "published build compiled it"),
            (lambda f: f.update({odex: fake_odex(**{"compiler-filter": "speed", "bootclasspath-checksums":
                                                    "i;2/00000000:i;2/f8c5165e"})}), "boot image"),
            (lambda f: f.pop(make_image.PREBUILT_OAT_RECORD), "is missing"),
        ]:
            files = dict(good)
            change(files)
            problems = make_image.prebuilt_oat_problems(self.archive(files))
            self.assertTrue(any(words in p for p in problems), (words, problems))


def fake_kernel(arch):
    """The first page of a kernel file: a bzImage's setup header or an arm64 Image's magic."""
    data = bytearray(0x1000)
    if arch == "x86_64":
        data[0x202:0x206] = b"HdrS"
    else:
        data[0x38:0x3c] = b"ARM\x64"
    return bytes(data)


def fake_elf(machine):
    """The header of a little-endian 64-bit ELF file for `machine`."""
    return b"\x7fELF" + bytes([2, 1, 1]) + bytes(9) + (3).to_bytes(2, "little") + machine.to_bytes(2, "little") + bytes(44)


class Arm64GuestTests(unittest.TestCase):
    """An arm64 guest image for ARM64 Windows hosts: 64-bit only, with its own kernel format."""

    def tearDown(self):
        make_image.use_arch(make_image.DEFAULT_ARCH)

    def inputs(self):
        return json.loads((Path(make_image.__file__).parent / "inputs.json").read_text())["images"]

    def test_fr27_inputs_pin_an_arm64_only_build_for_every_android_release(self):
        images = self.inputs()
        majors = {i["android_major"] for i in images}
        for major in majors:
            spec = make_image.image_spec(images, major, "arm64")
            self.assertEqual(spec["arch"], "arm64")
            # 64-bit only: the target carries no 32-bit ARM code.
            self.assertEqual(spec["target"], "aosp_cf_arm64_only_phone-userdebug")
            self.assertTrue(spec["files"]["target_files"]["name"].startswith("aosp_cf_arm64_only_phone-target_files-"))
            for f in spec["files"].values():
                self.assertRegex(f["sha256"], r"^[0-9a-f]{64}$")
                self.assertGreater(f["size"], 0)
            x86 = make_image.image_spec(images, major, "x86_64")
            # The two images are published side by side, so their versions differ.
            self.assertNotEqual(spec["version"], x86["version"])
            self.assertTrue(spec["version"].endswith("-arm64"))

    def test_fr27_every_input_names_its_architecture_once_per_release(self):
        seen = [(i["android_major"], i["arch"]) for i in self.inputs()]
        self.assertEqual(len(seen), len(set(seen)))
        self.assertEqual({a for _, a in seen}, {"x86_64", "arm64"})

    def test_fr27_an_input_without_an_architecture_is_x86_64(self):
        images = [{"android_major": 17, "version": "old"}]
        self.assertEqual(make_image.image_spec(images, 17)["version"], "old")
        with self.assertRaises(ValueError):
            make_image.image_spec(images, 17, "arm64")

    def test_fr27_the_arm64_kernel_is_an_image_and_the_x86_64_kernel_a_bzimage(self):
        self.assertEqual(make_image.kernel_name("arm64"), "Image")
        self.assertEqual(make_image.kernel_name("x86_64"), "bzImage")
        self.assertEqual(make_image.kernel_format(fake_kernel("arm64")), "arm64")
        self.assertEqual(make_image.kernel_format(fake_kernel("x86_64")), "x86_64")
        self.assertIsNone(make_image.kernel_format(bytes(0x1000)))

    def test_fr27_apps_are_compiled_for_the_arm64_boot_image(self):
        make_image.use_arch("arm64")
        self.assertEqual(make_image.FRAMEWORK_ODEX, "SYSTEM/framework/oat/arm64/services.odex")
        odex, vdex = make_image.oat_files("SYSTEM/priv-app/Settings/Settings.apk")
        self.assertIn("/oat/arm64/", odex)
        self.assertIn("/oat/arm64/", vdex)
        cmd = make_image.dex2oat_command("dex2oat", "a.apk", "/system/app/A/A.apk", "A.odex", "/r",
                                         "/a.jar", ["/b.art"], ("PCL[]", "PCL[]"), "speed")
        self.assertIn("--instruction-set=arm64", cmd)
        self.assertIn("--instruction-set-variant=generic", cmd)
        self.assertNotIn("--instruction-set=x86_64", cmd)

    def test_fr17_an_arm64_image_names_its_device_dsa_arm64(self):
        make_image.use_arch("arm64")
        props = make_image.edit_identity(
            "ro.product.vendor.device=vsoc_arm64_only\n"
            "ro.product.name=aosp_cf_arm64_only_phone\n"
            "ro.build.fingerprint=generic/aosp_cf/vsoc:16/BP4A/15581820:userdebug/test-keys\n")
        self.assertIn("ro.product.vendor.device=dsa_arm64", props)
        self.assertIn("ro.product.name=dsa_arm64", props)
        self.assertIn("ro.build.fingerprint=DSA/dsa_arm64/dsa_arm64:16/BP4A/15581820:user/test-keys", props)
        make_image.use_arch("x86_64")
        self.assertEqual(make_image.IDENTITY["device"], "dsa_x86_64")

    def test_fr27_x86_64_stays_the_default(self):
        self.assertEqual(make_image.DEFAULT_ARCH, "x86_64")
        self.assertEqual(make_image.FRAMEWORK_ODEX, "SYSTEM/framework/oat/x86_64/services.odex")
        with self.assertRaises(ValueError):
            make_image.use_arch("arm")

    def test_fr27_elf_machine_tells_aarch64_from_x86_64(self):
        self.assertEqual(make_image.elf_machine(fake_elf(183)), make_image.ARCHES["arm64"]["elf_machine"])
        self.assertEqual(make_image.elf_machine(fake_elf(62)), make_image.ARCHES["x86_64"]["elf_machine"])
        self.assertIsNone(make_image.elf_machine(b"not an elf file at all"))

    def test_fr27_fetch_takes_the_architecture_and_keeps_each_apart(self):
        fetch = (Path(make_image.__file__).parent / "fetch.sh").read_text()
        self.assertIn('arch="${2:-x86_64}"', fetch)
        self.assertIn("$build-$arch", fetch)
        self.assertIn('(.arch // "x86_64") == $a', fetch)

    def check(self, arch, target, kernel, binaries):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (keys_dir / "test.x509.pem").write_text("-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n")
            files = {}
            if kernel is not None:
                name, data = kernel
                (img_dir / name).write_bytes(data)
                files[name] = {"size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
            manifest = {"arch": arch, "source": {"target": target}, "files": files,
                        "kernel_cmdline": "console=ttyS0 init=/init"}
            (img_dir / "image.json").write_text(json.dumps(manifest))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                archive.writestr("META/apkcerts.txt", "")
                for name, data in binaries.items():
                    archive.writestr(name, data)
            proc = subprocess.run([sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                                  capture_output=True, text=True)
            self.assertNotEqual(proc.returncode, 0)
            return proc.stdout

    def test_fr27_check_image_rejects_an_x86_64_kernel_and_binaries_in_an_arm64_image(self):
        out = self.check("arm64", "aosp_cf_arm64_only_phone-userdebug", ("Image", fake_kernel("x86_64")),
                         {"VENDOR/bin/dsa-channeld": fake_elf(62), "VENDOR/bin/hw/dsa-hwcomposer": fake_elf(183)})
        self.assertIn("Image is not a kernel for arm64", out)
        self.assertIn("VENDOR/bin/dsa-channeld is built for ELF machine 62, not arm64", out)
        self.assertNotIn("dsa-hwcomposer is built for ELF machine", out)

    def test_fr27_check_image_accepts_an_arm64_kernel_and_rejects_a_bzimage_name(self):
        out = self.check("arm64", "aosp_cf_arm64_only_phone-userdebug", ("Image", fake_kernel("arm64")), {})
        self.assertNotIn("is not a kernel for", out)
        out = self.check("arm64", "aosp_cf_arm64_only_phone-userdebug", ("bzImage", fake_kernel("x86_64")), {})
        self.assertIn("carries bzImage, a kernel name for another architecture", out)

    def test_fr27_check_image_rejects_an_arm64_image_with_32_bit_arm(self):
        out = self.check("arm64", "aosp_cf_arm64_phone-userdebug", None, {})
        self.assertIn("not a 64-bit only one", out)

    def test_fr27_check_image_takes_an_image_without_an_architecture_as_x86_64(self):
        out = self.check(None, "aosp_cf_x86_64_only_phone-userdebug", ("bzImage", fake_kernel("x86_64")),
                         {"VENDOR/bin/dsa-channeld": fake_elf(183)})
        self.assertNotIn("is not a kernel for", out)
        self.assertIn("VENDOR/bin/dsa-channeld is built for ELF machine 183, not x86_64", out)

    def test_fr27_the_arm64_kernel_command_line_uses_the_pl011_console(self):
        arm = make_image.kernel_cmdline("blkdevparts=sda:1@0(misc)", arch="arm64").split()
        self.assertIn("console=ttyAMA0", arm)
        self.assertNotIn("console=ttyS0", arm)
        self.assertNotIn("8250.nr_uarts=1", arm)
        x86 = make_image.kernel_cmdline("blkdevparts=sda:1@0(misc)").split()
        self.assertIn("console=ttyS0", x86)
        # Everything but the console is the same on both.
        self.assertEqual([a for a in arm if "console=" not in a],
                         [a for a in x86 if "console=" not in a and a != "8250.nr_uarts=1"])

    def test_fr27_every_dsa_guest_binary_builds_for_arm64(self):
        guest = Path(make_image.__file__).resolve().parents[1]
        for script in ["hwcomposer/build.sh", "codec2/build.sh", "power/build.sh"]:
            text = (guest / script).read_text()
            self.assertIn('guest_arch="${DSA_GUEST_ARCH:-x86_64}"', text, script)
            self.assertIn("arm64) android_abi=arm64-v8a", text, script)
            self.assertIn('-DANDROID_ABI="$android_abi"', text, script)
            self.assertNotIn("-DANDROID_ABI=x86_64", text, script)
        channeld = (guest / "channeld/build-android.sh").read_text()
        self.assertIn("arm64) target=aarch64-linux-android; machine='AArch64'", channeld)
        self.assertIn('grep -Fq "$machine"', channeld)
        self.assertNotIn("CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER", channeld)

    def test_fr27_ci_makes_and_checks_arm64_images_beside_x86_64(self):
        workflows = Path(make_image.__file__).resolve().parents[3] / ".github/workflows"
        for name in ["test-image.yml", "release-image.yml"]:
            text = (workflows / name).read_text()
            self.assertIn("arch: [x86_64, arm64]", text, name)
            self.assertIn("DSA_GUEST_ARCH: ${{ matrix.arch }}", text, name)
            self.assertIn("--arch ${{ matrix.arch }}", text, name)
            self.assertIn("fetch.sh ${{ matrix.android }} ${{ matrix.arch }}", text, name)
        release = (workflows / "release-image.yml").read_text()
        self.assertIn("name: kernel-arm64", release)
        self.assertIn("images-arm64.json", release)


class ArmTranslationImageTests(unittest.TestCase):
    """The image's half of ARM app translation: nothing is on unless the host says a package is."""

    def _modify(self, root, arm_translation=None):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
            archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
            archive.writestr("META/filesystem_config.txt", "system/bin/sh 0 2000 755 capabilities=0x0\n")
            archive.writestr("PRODUCT/etc/build.prop", "ro.product.product.name=dsa\nro.dalvik.vm.native.bridge=0\n")
        modified = root / "modified.zip"
        make_image.modify(source, modified, arm_translation=arm_translation)
        return zipfile.ZipFile(modified)

    @staticmethod
    def _sections(rc):
        """Each action of an init script: its trigger line and its command lines."""
        sections = []
        for line in rc.splitlines():
            if line.startswith("on "):
                sections.append((line, []))
            elif line.strip() and not line.lstrip().startswith("#") and sections:
                sections[-1][1].append(line.strip())
        return sections

    def test_fr25_the_product_props_import_the_bridge_only_when_the_host_turns_it_on(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            product = archive.read("PRODUCT/etc/build.prop").decode().splitlines()
            self.assertEqual(
                product[-1],
                "import /system/etc/dsa/arm-translation-${ro.boot.dsa.arm_translation:-off}.prop",
            )
            self.assertEqual(product.count(product[-1]), 1)
            on = archive.read("SYSTEM/etc/dsa/arm-translation-on.prop").decode().splitlines()
            for prop in [
                "ro.dalvik.vm.native.bridge=libdsa_native_bridge.so",
                "ro.dalvik.vm.isa.arm64=x86_64",
                "ro.enable.native.bridge.exec=1",
                "ro.product.cpu.abilist=x86_64,arm64-v8a",
                "ro.product.cpu.abilist64=x86_64,arm64-v8a",
            ]:
                self.assertIn(prop, on)
            self.assertFalse(any("armeabi" in line for line in on), "the guest has no 32-bit zygote")
            off = archive.read("SYSTEM/etc/dsa/arm-translation-off.prop").decode().splitlines()
            self.assertEqual([line for line in off if line and not line.startswith("#")], [])
            config = archive.read("META/filesystem_config.txt").decode()
            for rule in make_image.ARM_TRANSLATION_FS_CONFIG:
                self.assertIn(rule, config)

    def test_fr25_x86_64_stays_first_in_every_abi_list_when_translation_is_on(self):
        # The package manager and the stores take the first ABI an app has code for, so an app
        # with x86_64 code runs without the translator only while x86_64 comes first.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            on = archive.read("SYSTEM/etc/dsa/arm-translation-on.prop").decode().splitlines()
            lists = {}
            for line in on:
                name, _, value = line.partition("=")
                if not line.startswith("#") and ".cpu.abilist" in name:
                    lists[name] = value.split(",")
            for name in [
                "ro.product.cpu.abilist",
                "ro.product.cpu.abilist64",
                "ro.product.product.cpu.abilist",
                "ro.product.product.cpu.abilist64",
            ]:
                self.assertIn(name, lists)
            for name, abis in lists.items():
                self.assertEqual(abis, ["x86_64", "arm64-v8a"], name)

    def test_fr25_the_package_is_mounted_over_system_only_when_the_host_turns_it_on(self):
        rc = (Path(make_image.__file__).with_name("dsa-arm-translation.rc")).read_text()
        sections = dict(self._sections(rc))
        images = {
            "on": "/data/vendor/dsa/arm-translation/system.img",
            "default": "/system/etc/dsa/arm-translation/default.img",
        }
        for value, image in images.items():
            section = sections[f"on post-fs-data && property:ro.boot.dsa.arm_translation={value}"]
            self.assertIn(f"mount erofs loop@{image} /mnt/dsa-arm-translation ro nodev nosuid "
                          "context=u:object_r:system_lib_file:s0", section)
            for directory in ["lib64", "bin", "etc"]:
                self.assertIn(
                    f"mount overlay overlay /system/{directory} ro nodev nosuid "
                    f"lowerdir=/mnt/dsa-arm-translation/system/{directory}:/system/{directory}",
                    section,
                )
            # Nothing else in Android mounts binfmt_misc, so the handlers are registered only
            # once it is mounted here.
            mount = section.index("mount binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc nodev noexec nosuid")
            for rule in ["arm64_exe", "arm64_dyn"]:
                register = section.index(f"copy /mnt/dsa-arm-translation/system/etc/binfmt_misc/{rule} "
                                         "/proc/sys/fs/binfmt_misc/register")
                self.assertGreater(register, mount)
        # Every mount waits for the host's word.
        for trigger, commands in self._sections(rc):
            if trigger not in {f"on post-fs-data && property:ro.boot.dsa.arm_translation={value}"
                               for value in images}:
                self.assertFalse([c for c in commands if c.startswith("mount ")], trigger)
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            self.assertEqual(archive.read("SYSTEM/etc/init/dsa-arm-translation.rc").decode(), rc)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn("(allow init system_lib_file (dir (mounton)))", policy)
            self.assertIn("(allow init proc (dir (mounton)))", policy)
            self.assertIn("(allow init binfmt_miscfs (filesystem (mount)))", policy)
            self.assertIn("(allow init binfmt_miscfs (file (write open getattr)))", policy)

    def test_fr25_an_image_made_with_a_default_package_carries_it_in_its_system_partition(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            package = DefaultArmTranslationTests._package(root)
            with self._modify(root, arm_translation=package) as archive:
                self.assertEqual(archive.read("SYSTEM/etc/dsa/arm-translation/default.img"),
                                 (package / "system.img").read_bytes())
                # The default package is laid over the system as a user's is.
                self.assertEqual(archive.read("SYSTEM/etc/dsa/arm-translation-default.prop"),
                                 archive.read("SYSTEM/etc/dsa/arm-translation-on.prop"))
                config = archive.read("META/filesystem_config.txt").decode()
                for rule in make_image.ARM_TRANSLATION_DEFAULT_FS_CONFIG:
                    self.assertIn(rule, config)
            # Without one, a boot told to use the default has no properties to import, so it
            # stays x86_64 only.
            (root / "modified.zip").unlink()
            with self._modify(root) as archive:
                names = archive.namelist()
                self.assertNotIn("SYSTEM/etc/dsa/arm-translation/default.img", names)
                self.assertNotIn("SYSTEM/etc/dsa/arm-translation-default.prop", names)
                config = archive.read("META/filesystem_config.txt").decode()
                for rule in make_image.ARM_TRANSLATION_DEFAULT_FS_CONFIG:
                    self.assertNotIn(rule, config)

    def test_fr25_check_image_wants_the_default_package_in_the_system_partition_of_an_x86_64_image_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            package = DefaultArmTranslationTests._package(root)
            out = root / "image"
            out.mkdir()
            entry = make_image.default_arm_translation(package, "x86_64", out)
            self.assertEqual(entry["system_image"], "/system/etc/dsa/arm-translation/default.img")
            on = (Path(make_image.__file__).with_name("arm-translation-on.prop")).read_bytes()
            good = {
                "SYSTEM/etc/dsa/arm-translation/default.img": (package / "system.img").read_bytes(),
                "SYSTEM/etc/dsa/arm-translation-default.prop": on,
            }

            def check(manifest, files):
                return make_image.check_builtin_arm_translation(manifest, list(files), files.__getitem__)

            x86 = {"arch": "x86_64", "arm_translation": entry}
            self.assertEqual(check(x86, good), [])
            self.assertTrue(check(x86, {}), "described but not carried")
            changed = dict(good, **{"SYSTEM/etc/dsa/arm-translation/default.img": b"other"})
            self.assertTrue(check(x86, changed), "not the described package")
            other_props = dict(good, **{"SYSTEM/etc/dsa/arm-translation-default.prop": b"ro.x=1\n"})
            self.assertTrue(check(x86, other_props))
            self.assertEqual(check({"arch": "x86_64"}, {}), [])
            self.assertTrue(check({"arch": "x86_64"}, good), "carried but not described")
            # An arm64 image runs ARM apps itself: no default package, described or carried.
            self.assertEqual(check({"arch": "arm64"}, {}), [])
            self.assertTrue(check({"arch": "arm64"}, good))
            for name, data in good.items():
                self.assertTrue(check({"arch": "arm64"}, {name: data}), name)

    def test_fr25_check_image_rejects_an_image_that_turns_a_bridge_on_by_itself(self):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            on_trigger = "on post-fs-data && property:ro.boot.dsa.arm_translation=on\n"
            default_trigger = "on post-fs-data && property:ro.boot.dsa.arm_translation=default\n"
            outputs = []
            # Three images: one without the translation files, one whose rc only mounts an
            # installed package and never the default translator, and a complete one.
            for rc in [None, on_trigger, on_trigger + default_trigger]:
                complete = rc is not None
                with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                    archive.writestr("META/apkcerts.txt", "")
                    bridge = "0" if complete else "libhoudini.so"
                    archive.writestr("SYSTEM/build.prop", f"ro.debuggable=0\nro.dalvik.vm.native.bridge={bridge}\n")
                    if complete:
                        archive.writestr("SYSTEM/etc/init/dsa-arm-translation.rc", rc)
                        archive.writestr("SYSTEM/etc/dsa/arm-translation-on.prop", "")
                        archive.writestr("SYSTEM/etc/dsa/arm-translation-off.prop", "")
                        archive.writestr("PRODUCT/etc/build.prop", make_image.add_arm_translation_import(""))
                    else:
                        archive.writestr("PRODUCT/etc/build.prop", "ro.product.product.name=dsa\n")
                outputs.append(subprocess.run(
                    [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                    capture_output=True, text=True,
                ).stdout)
        missing, without_default, whole = outputs
        for problem in ["dsa-arm-translation.rc is missing", "does not end with the ARM translation import",
                        "sets ro.dalvik.vm.native.bridge=libhoudini.so"]:
            self.assertIn(problem, missing)
            self.assertNotIn(problem, without_default)
            self.assertNotIn(problem, whole)
        # Checked only once the rc is there: a missing rc is reported as missing above.
        laid_over = "never lays the image's default ARM translation package"
        self.assertIn(laid_over, without_default)
        self.assertNotIn(laid_over, whole)

    def test_fr32_the_product_props_import_the_google_play_properties_before_the_arm_ones(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            product = archive.read("PRODUCT/etc/build.prop").decode().splitlines()
            self.assertEqual(product[-2:], [
                "import /system/etc/dsa/gapps-${ro.boot.dsa.gapps:-off}.prop",
                "import /system/etc/dsa/arm-translation-${ro.boot.dsa.arm_translation:-off}.prop",
            ])
            self.assertEqual(product.count(product[-2]), 1)
            on = archive.read("SYSTEM/etc/dsa/gapps-on.prop").decode().splitlines()
            props = [line for line in on if line and not line.startswith("#")]
            self.assertEqual([line.split("=", 1)[0] for line in props], ["ro.oem.build.fingerprint"])
            off = archive.read("SYSTEM/etc/dsa/gapps-off.prop").decode().splitlines()
            self.assertEqual([line for line in off if line and not line.startswith("#")], [])
            config = archive.read("META/filesystem_config.txt").decode()
            for rule in make_image.GAPPS_FS_CONFIG:
                self.assertIn(rule, config)

    def test_fr32_the_google_apps_are_laid_over_only_when_the_host_says_they_are_installed(self):
        rc = (Path(make_image.__file__).with_name("dsa-gapps.rc")).read_text()
        trigger = "on post-fs-data && property:ro.boot.dsa.gapps=on\n"
        before, after = rc.split(trigger, 1)
        section, rest = after.split("\n\n", 1)
        self.assertIn("mount erofs loop@/data/vendor/dsa/gapps/system.img /mnt/dsa-gapps ro nodev nosuid "
                      "context=u:object_r:system_file:s0", section)
        for partition in ["product", "system_ext"]:
            self.assertIn(
                f"mount overlay overlay /{partition} ro nodev nosuid "
                f"lowerdir=/mnt/dsa-gapps/{partition}:/{partition}",
                section,
            )
        # Every mount waits for the host's word.
        self.assertNotIn("mount ", before)
        self.assertNotIn("mount ", rest)
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            self.assertEqual(archive.read("SYSTEM/etc/init/dsa-gapps.rc").decode(), rc)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn("(allow init system_file (filesystem (relabelto associate)))", policy)
            self.assertIn("(allow system_file labeledfs (filesystem (associate)))", policy)

    def test_fr32_x86_64_stays_the_first_abi_when_arm_translation_is_on(self):
        on = (Path(make_image.__file__).with_name("arm-translation-on.prop")).read_text().splitlines()
        abilists = [line.split("=", 1)[1] for line in on if ".cpu.abilist" in line.split("=", 1)[0]]
        self.assertTrue(abilists)
        for abilist in abilists:
            self.assertEqual(abilist.split(",")[0], "x86_64", abilist)

    def test_fr32_check_image_rejects_an_image_that_lays_the_google_apps_over_by_itself(self):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            gapps_rc = (Path(make_image.__file__).with_name("dsa-gapps.rc")).read_text()
            outputs = []
            for case in ["missing", "always", "arm-first", "good"]:
                with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                    archive.writestr("META/apkcerts.txt", "")
                    archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                    abilist = "arm64-v8a,x86_64" if case == "arm-first" else "x86_64,arm64-v8a"
                    archive.writestr("SYSTEM/etc/dsa/arm-translation-on.prop",
                                     f"ro.product.cpu.abilist={abilist}\n")
                    if case == "missing":
                        archive.writestr("PRODUCT/etc/build.prop", "ro.product.product.name=dsa\n")
                    else:
                        rc = gapps_rc if case != "always" else "on post-fs-data\n    mount overlay overlay /product ro\n"
                        archive.writestr("SYSTEM/etc/init/dsa-gapps.rc", rc)
                        archive.writestr("SYSTEM/etc/dsa/gapps-on.prop", "ro.oem.build.fingerprint=x\n")
                        archive.writestr("SYSTEM/etc/dsa/gapps-off.prop", "# nothing\n")
                        archive.writestr("PRODUCT/etc/build.prop",
                                         make_image.add_arm_translation_import(make_image.add_gapps_import("")))
                outputs.append(subprocess.run(
                    [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                    capture_output=True, text=True,
                ).stdout)
        missing, always, arm_first, good = outputs
        for problem in ["dsa-gapps.rc is missing", "does not import the Google Play properties"]:
            self.assertIn(problem, missing)
            self.assertNotIn(problem, good)
        self.assertIn("without waiting for the host", always)
        self.assertNotIn("without waiting for the host", good)
        self.assertIn("x86_64 is not the ABI apps are installed for first", arm_first)
        self.assertNotIn("x86_64 is not the ABI apps are installed for first", good)


class DefaultArmTranslationTests(unittest.TestCase):
    """The default translator an x86_64 image's release carries (the Translator build's
    Digitalis package), and that an arm64 image never carries one."""

    @staticmethod
    def _package(root, **changes):
        package = Path(root) / "package"
        package.mkdir(exist_ok=True)
        description = {"name": "Digitalis", "version": "17-abefbe2b1600", "library": "libberberis_arm64.so",
                       "abis": ["arm64-v8a"], "license": "Apache-2.0", **changes}
        (package / "arm-translation.json").write_text(json.dumps(description))
        image = bytearray(4096)
        image[1024:1028] = (0xE0F5E1E2).to_bytes(4, "little")
        (package / "system.img").write_bytes(bytes(image))
        return package

    def test_fr25_the_x86_64_release_carries_the_default_package_that_image_json_describes(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "image"
            out.mkdir()
            entry = make_image.default_arm_translation(self._package(tmp), "x86_64", out)
            self.assertEqual(entry["file"], "arm-translation.zip")
            self.assertEqual((entry["name"], entry["version"], entry["abis"], entry["license"]),
                             ("Digitalis", "17-abefbe2b1600", ["arm64-v8a"], "Apache-2.0"))
            with zipfile.ZipFile(out / "arm-translation.zip") as z:
                self.assertEqual(sorted(z.namelist()), ["arm-translation.json", "system.img"])
            self.assertEqual(entry["system_image"], "/system/etc/dsa/arm-translation/default.img")
            self.assertEqual(entry["system_image_sha256"],
                             hashlib.sha256((Path(tmp) / "package/system.img").read_bytes()).hexdigest())
            manifest = {"arch": "x86_64", "files": {}, "arm_translation": entry}
            self.assertEqual(make_image.check_default_arm_translation(manifest, out), [])
            # The host uses only a default package image.json says is in the system partition.
            elsewhere = dict(entry, system_image="/data/default.img")
            self.assertTrue(make_image.check_default_arm_translation(
                {"arch": "x86_64", "files": {}, "arm_translation": elsewhere}, out))
            # The zip is what install takes, so a changed one is caught before it is published.
            with zipfile.ZipFile(out / "arm-translation.zip", "a") as z:
                z.writestr("extra", "x")
            self.assertTrue(make_image.check_default_arm_translation(manifest, out))

    def test_fr25_an_arm64_image_never_takes_or_carries_a_translator(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "image"
            out.mkdir()
            with self.assertRaises(SystemExit):
                make_image.default_arm_translation(self._package(tmp), "arm64", out)
            self.assertFalse((out / "arm-translation.zip").exists())
            self.assertEqual(make_image.check_default_arm_translation({"arch": "arm64", "files": {}}, out), [])
            entry = {"file": "arm-translation.zip", "name": "Digitalis"}
            self.assertTrue(make_image.check_default_arm_translation(
                {"arch": "arm64", "files": {}, "arm_translation": entry}, out))
            (out / "arm-translation.zip").write_bytes(b"PK")
            self.assertTrue(make_image.check_default_arm_translation({"arch": "arm64", "files": {}}, out))

    def test_fr25_make_image_refuses_a_translator_for_an_arm64_image_before_making_it(self):
        script = Path(__file__).with_name("make-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            result = subprocess.run(
                [sys.executable, str(script), "--major", "17", "--arch", "arm64", "--keys", tmp,
                 "--inputs", tmp, "--out", tmp, "--arm-translation", str(self._package(tmp))],
                capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("takes no ARM translation package", result.stderr)
            self.assertFalse(any(Path(tmp).glob("17.*")), "nothing is made for the arm64 image")

    def test_fr25_a_default_package_the_host_would_refuse_is_not_released(self):
        for changes in [{"abis": ["arm64-v8a", "armeabi-v7a"]}, {"library": "../libx.so"}, {"name": ""},
                        {"version": " 17"}]:
            with tempfile.TemporaryDirectory() as tmp:
                out = Path(tmp) / "image"
                out.mkdir()
                with self.assertRaises(SystemExit, msg=str(changes)):
                    make_image.default_arm_translation(self._package(tmp, **changes), "x86_64", out)
        with tempfile.TemporaryDirectory() as tmp:
            package = self._package(tmp)
            (package / "system.img").write_bytes(bytes(4096))
            out = Path(tmp) / "image"
            out.mkdir()
            with self.assertRaises(SystemExit):
                make_image.default_arm_translation(package, "x86_64", out)


class FrameworkImageTests(unittest.TestCase):
    """The patched SurfaceFlinger, libhwui and libEGL from the Framework build, which recover
    from a GPU device loss, in place of the published build's own."""

    @staticmethod
    def _elf(machine, body):
        header = bytearray(64)
        header[:4] = b"\x7fELF"
        header[4] = 2
        header[5] = 1
        header[18:20] = machine.to_bytes(2, "little")
        return bytes(header) + body

    def _framework(self, root, build_id="16373615", arch="x86_64", machine=62):
        folder = root / "framework"
        files = {}
        for name in make_image.FRAMEWORK_FILES:
            data = self._elf(machine, b"patched " + name.encode())
            (folder / name).parent.mkdir(parents=True, exist_ok=True)
            (folder / name).write_bytes(data)
            files[name] = {"size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        (folder / "framework.json").write_text(json.dumps({
            "android": "android17", "android_major": 17, "build_id": build_id, "release": "cp2a",
            "arch": arch, "version": "17-0123456789ab", "files": files}))
        return folder

    def _source(self, root, names=None):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type surfaceflinger)\n")
            archive.writestr("SYSTEM/build.prop", "ro.debuggable=1\n")
            for name in make_image.FRAMEWORK_FILES if names is None else names:
                archive.writestr(name, self._elf(62, b"published " + name.encode()))
        return source

    def test_nfr5_the_framework_replaces_surfaceflinger_libhwui_and_libegl(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            payloads, description = make_image.checked_framework(self._framework(root), "16373615", 17, "x86_64")
            self.assertEqual(sorted(payloads), sorted(["SYSTEM/bin/surfaceflinger", "SYSTEM/lib64/libhwui.so",
                                                       "SYSTEM/lib64/libEGL.so"]))
            modified = root / "modified.zip"
            make_image.modify(self._source(root), modified, framework=payloads)
            with zipfile.ZipFile(modified) as archive:
                for name, data in payloads.items():
                    self.assertEqual(archive.read(name), data)
                policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertIn(make_image.GPU_RESET_RULE, policy)
            entry = make_image.framework_entry(description)
            self.assertEqual(entry["build_id"], "16373615")
            self.assertEqual(entry["files"], {n: hashlib.sha256(d).hexdigest() for n, d in payloads.items()})

    def test_nfr5_without_the_framework_the_image_keeps_the_builds_files_and_policy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            modified = root / "modified.zip"
            make_image.modify(self._source(root), modified)
            with zipfile.ZipFile(modified) as archive:
                self.assertIn(b"published", archive.read("SYSTEM/lib64/libhwui.so"))
                policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
                self.assertNotIn(make_image.GPU_RESET_RULE, policy)

    def test_nfr5_the_framework_must_replace_files_the_build_has(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            payloads, _ = make_image.checked_framework(self._framework(root), "16373615", 17, "x86_64")
            source = self._source(root, names=["SYSTEM/bin/surfaceflinger"])
            with self.assertRaisesRegex(ValueError, "libEGL.so, SYSTEM/lib64/libhwui.so, which the patched"):
                make_image.modify(source, root / "modified.zip", framework=payloads)

    def test_nfr5_a_framework_built_for_another_build_or_architecture_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaisesRegex(SystemExit, "image is made from build 16373615"):
                make_image.checked_framework(self._framework(root, build_id="1"), "16373615", 17, "x86_64")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaisesRegex(SystemExit, "built for arm64, not x86_64"):
                make_image.checked_framework(self._framework(root, arch="arm64", machine=183),
                                             "16373615", 17, "x86_64")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            folder = self._framework(root)
            (folder / "SYSTEM/lib64/libhwui.so").write_bytes(self._elf(62, b"something else"))
            with self.assertRaisesRegex(SystemExit, "libhwui.so is not the file framework.json describes"):
                make_image.checked_framework(folder, "16373615", 17, "x86_64")

    def test_nfr5_check_image_finds_the_framework_build_files_in_the_image(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            payloads, description = make_image.checked_framework(self._framework(root), "16373615", 17, "x86_64")
            modified = root / "modified.zip"
            make_image.modify(self._source(root), modified, framework=payloads)
            manifest = {"arch": "x86_64", "source": {"build_id": "16373615"},
                        "framework": make_image.framework_entry(description)}
            with zipfile.ZipFile(modified) as archive:
                self.assertEqual(make_image.check_framework(manifest, archive.namelist(), archive.read), [])
            # The build's own files, or a policy without the rule, are found.
            with zipfile.ZipFile(self._source(root)) as archive:
                problems = make_image.check_framework(manifest, archive.namelist(), archive.read)
            self.assertEqual(len([p for p in problems if "not the framework build's file" in p]), 3)
            self.assertTrue(any("never hear of a GPU reset" in p for p in problems), problems)
            # An image without a framework has nothing to check.
            self.assertEqual(make_image.check_framework({"arch": "x86_64"}, [], None), [])


if __name__ == "__main__":
    unittest.main()
