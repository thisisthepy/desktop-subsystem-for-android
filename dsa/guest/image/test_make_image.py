"""Regression tests for the guest image layout helpers."""

import base64
import gzip
import hashlib
import importlib.util
import io
import json
import re
import stat
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
    def test_nfr2_the_cached_process_limit_applies_where_android_17_reads_it(self):
        # Android 17 takes the limit from the device configuration, which the agent sets, and
        # else from config_customizedMaxCachedProcesses (1024), never from the global setting.
        self.assertEqual(make_image.FRAMEWORK_OVERLAY_INTEGERS["config_customizedMaxCachedProcesses"], 8)
        self.assertIn("integer/config_customizedMaxCachedProcesses", make_image.framework_overlaid())
        with tempfile.TemporaryDirectory() as tmp:
            make_image.framework_overlay_sources(Path(tmp))
            values = (Path(tmp) / "res/values/dsa.xml").read_text()
        self.assertIn('<integer name="config_customizedMaxCachedProcesses">8</integer>', values)
        agent = Path(__file__).parent.parent / "agent"
        source = (agent / "src/main/kotlin/io/github/thisisthepy/dsa/agent/platform/AndroidAppProcesses.kt").read_text()
        self.assertIn('NAMESPACE = "activity_manager"', source)
        self.assertIn("DeviceConfig.setProperty(NAMESPACE, MAX_CACHED", source)
        closed = (agent / "src/main/kotlin/io/github/thisisthepy/dsa/agent/memory/ClosedApps.kt").read_text()
        self.assertIn("const val MAX_CACHED_PROCESSES = 8", closed)

    def test_nfr2_channeld_may_page_a_woken_app_out_to_zram(self):
        # process_madvise on another user's process asks CAP_SYS_NICE and CAP_SYS_PTRACE: init
        # gives the daemon both, and the policy lets it use them, or every pageout is refused.
        here = Path(__file__).parent
        cil = (here / "sepolicy" / "channeld.cil").read_text()
        self.assertRegex(cil, r"\(allow dsa_channeld self \(capability \([^)]*\bsys_nice\b[^)]*\bsys_ptrace\b")
        rc = (here / "dsa-channeld.rc").read_text()
        service = rc[rc.index("service dsa_channeld"):rc.index("on property:sys.boot_completed=1")]
        self.assertIn("capabilities SYS_NICE SYS_PTRACE", service)

    def test_fr21_channeld_may_clear_the_pstore_records_the_host_collected(self):
        # init leaves /sys/fs/pstore read only to system; the daemon makes it writable to
        # itself and unlinks each collected record, which erases it from the pstore disk.
        cil = (Path(__file__).parent / "sepolicy" / "channeld.cil").read_text()
        self.assertRegex(cil, r"\(allow dsa_channeld pstorefs \(dir \([^)]*\bsetattr\b[^)]*\bremove_name\b")
        self.assertRegex(cil, r"\(allow dsa_channeld pstorefs \(file \([^)]*\bunlink\b")

    # The SELinux type and class of every kernel file dsa-channeld opens, as the guest's kernel
    # labels it, and the permission an open asks of it when that is not read (dir: search). A
    # path the daemon opens that is not listed here fails the test below, so a new read cannot
    # ship without its rule (the refused /proc/vmstat read went unnoticed).
    CHANNELD_KERNEL_PATHS = {
        "/proc/bootconfig": [("proc_bootconfig", "file")],
        "/sys/fs/pstore": [("pstorefs", "dir"), ("pstorefs", "file")],
        "/proc/meminfo": [("proc_meminfo", "file")],
        "/proc/pressure/memory": [("proc_pressure_mem", "file")],
        "/proc/vmstat": [("proc_vmstat", "file")],
        # The process listing, the owner of each process's entry (an app's or not), and each
        # app process's /proc/<pid>/cmdline.
        "/proc": [("proc", "dir"), ("appdomain", "dir"), ("appdomain", "dir", "getattr"), ("appdomain", "file")],
        "/proc/{}/cmdline": [("appdomain", "file")],
        # The pageout after an app wakes: its importance, its memory and its address space.
        "/proc/{pid}/status": [("appdomain", "file")],
        "/proc/{}/oom_score_adj": [("appdomain", "file")],
        "/proc/{}/maps": [("appdomain", "file")],
        # The kernel's su guard on securityfs, which only the elevation helper writes.
        "/sys/kernel/security/dsa_su/control": [
            ("securityfs", "dir"),
            ("dsa_su_control_file", "dir"),
            ("dsa_su_control_file", "file", "write"),
        ],
    }
    # Kernel files a source names for a domain that never opens them. su_relay.rs is the
    # daemon's, and its KernelGuard writes the su guard only where a test points it at a file
    # of its own: on the guest the daemon hands each config to the elevation helper
    # (HelperGuard), because the guard refuses the daemon, which runs as system.
    CHANNELD_KERNEL_NAMED_NOT_OPENED = {("su_relay.rs", "/sys/kernel/security/dsa_su/control")}
    # The modes init runs the same binary in, each in a domain of its own.
    CHANNELD_SOURCE_DOMAINS = {
        "share_mount.rs": ("dsa_sharemount", "files.cil"),
        "android_files.rs": ("dsa_files", "files.cil"),
        "webdav.rs": ("dsa_files", "files.cil"),
        # The elevation helper (dsa-channeld --su-relay) and the commands it starts.
        "su_helper.rs": ("dsa_su", "su.cil"),
        "su_process.rs": ("dsa_su", "su.cil"),
        # KeyMint's root key from the host (dsa-channeld --keymint-key).
        "keymint_key.rs": ("dsa_keymint_key", "channeld.cil"),
        # The packet filter rules that send DNS queries to the stub (dsa-channeld --dns-redirect).
        "dns_redirect.rs": ("dsa_dns_redirect", "channeld.cil"),
    }
    # The policy files the image installs together with the daemon, so a rule for one of its
    # domains may live in any of them (the daemon's root elevation rules are in su.cil).
    CHANNELD_POLICY_FILES = ("channeld.cil", "files.cil", "su.cil")

    @classmethod
    def _daemon_policy(cls, here):
        """Every rule the image installs with the daemon, as one text."""
        return "\n".join((here / "sepolicy" / name).read_text() for name in cls.CHANNELD_POLICY_FILES)

    def test_nfr2_every_kernel_file_channeld_opens_has_an_allow_rule(self):
        here = Path(__file__).parent
        src = here.parent / "channeld" / "src"
        literal = re.compile(r'"(/(?:proc|sys)(?:/[^"]*)?)"')
        found = 0
        for source in sorted(src.glob("*.rs")):
            domain, policy_file = self.CHANNELD_SOURCE_DOMAINS.get(
                source.name, ("dsa_channeld", "channeld.cil")
            )
            policy = self._daemon_policy(here)
            for number, line in enumerate(source.read_text().splitlines(), 1):
                if line.strip().startswith("//"):
                    continue
                for path in literal.findall(line):
                    if path.startswith("/proc/self/"):
                        # A process's own /proc entries are every domain's to read.
                        continue
                    if (source.name, path) in self.CHANNELD_KERNEL_NAMED_NOT_OPENED:
                        continue
                    where = f"{source.name}:{number} opens {path}"
                    self.assertIn(
                        path,
                        self.CHANNELD_KERNEL_PATHS,
                        f"{where}, which this test does not know: add its SELinux type here "
                        f"and an allow rule for {domain} to sepolicy/{policy_file}",
                    )
                    found += 1
                    for target, cls, *asked in self.CHANNELD_KERNEL_PATHS[path]:
                        perm = asked[0] if asked else "search" if cls == "dir" else "read"
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
        # The package manager's list, where root elevation finds each named app's uid.
        "/data/system/packages.list": ["system_data_root_file", "system_data_file"],
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
            policy = self._daemon_policy(here)
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

    def test_nfr1_example_hals_for_hardware_a_pc_lacks_are_not_in_the_image(self):
        # Each ran as a process of its own from boot and answered nothing; the framework runs
        # without each of them. The HALs the guest uses stay.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            removed = [
                "com.android.hardware.authsecret", "com.android.hardware.rebootescrow",
                "com.android.hardware.cas", "com.android.hardware.contexthub",
                "com.android.hardware.input.processor", "com.google.cf.ir",
                "com.android.hardware.memtrack", "com.android.hardware.net.nlinterceptor",
                "com.android.hardware.tetheroffload", "com.android.hardware.neuralnetworks",
                "com.android.hardware.secure_element", "com.android.hardware.usb",
                "com.android.hardware.vibrator",
            ]
            kept = ["com.android.hardware.boot", "com.android.hardware.wifi", "com.android.hardware.thermal",
                    "com.android.hardware.security.secretkeeper", "com.android.hardware.drm.clearkey"]
            with zipfile.ZipFile(source, "w") as archive:
                for apex in removed + kept:
                    archive.writestr(f"VENDOR/apex/{apex}.apex", b"apex")
                archive.writestr("META/apexkeys.txt", "".join(
                    f'name="{apex}.apex" public_key="k.avbpubkey" private_key="k.pem" container_certificate="c.x509.pem" container_private_key="c.pk8" partition="vendor"\n'
                    for apex in removed + kept
                ))
                for name in make_image.REMOVED_VENDOR_FILES:
                    archive.writestr(name, b"file")
                archive.writestr("VENDOR/etc/init/motioncontext-service-default.rc", b"service")
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                names = archive.namelist()
                xml = archive.read("VENDOR/etc/permissions/dsa-unavailable-features.xml").decode()
            for apex in removed:
                self.assertNotIn(f"VENDOR/apex/{apex}.apex", names)
            for apex in kept:
                self.assertIn(f"VENDOR/apex/{apex}.apex", names)
            for name in make_image.REMOVED_VENDOR_FILES:
                self.assertNotIn(name, names)
            self.assertIn("VENDOR/etc/init/motioncontext-service-default.rc", names)
            # Without these, system_server does not start the services that would wait for the
            # removed HALs, and the consumer IR service does not refuse to start.
            for feature in ["android.hardware.biometrics.face", "android.hardware.fingerprint",
                            "android.hardware.consumerir", "android.hardware.context_hub"]:
                self.assertIn(f'<unavailable-feature name="{feature}" />', xml)

    def test_nfr1_the_biometric_hals_go_with_their_init_scripts_manifests_and_features(self):
        for name in ["VENDOR/bin/hw/android.hardware.biometrics.fingerprint-service.default",
                     "VENDOR/etc/init/face-default.rc",
                     "VENDOR/etc/vintf/manifest/fingerprint-default.xml",
                     "VENDOR/etc/permissions/android.hardware.fingerprint.xml"]:
            self.assertTrue(make_image.removed(name), name)
        self.assertFalse(make_image.removed("VENDOR/etc/permissions/android.hardware.wifi.xml"))

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

    def test_fr32_pr3_android_reports_opengles_3_2_to_app_stores(self):
        # v0.2.1 on a real PC: ro.opengles.version was empty and `pm list features` reported
        # reqGlEsVersion=0x0, so Google Play called YouTube incompatible and left YouTube and
        # Chrome out of its search. Cuttlefish's launcher passes the version at boot, and DSA
        # does not run it, so the vendor build.prop claims it and the boot passes the same one.
        self.assertEqual(make_image.OPENGLES_VERSION, 196610)
        for vendor in [False, True]:
            props = make_image.edit_props("ro.debuggable=1\nro.opengles.version=196608\n", vendor=vendor)
            self.assertIn("ro.opengles.version=196610\n", props)
            self.assertNotIn("ro.opengles.version=196608", props)
        self.assertIn("ro.opengles.version=196610\n",
                      make_image.edit_props("ro.debuggable=1\n", vendor=True, network=True))
        for arch in sorted(make_image.ARCHES):
            for hwcomposer in [False, True]:
                cmdline = make_image.kernel_cmdline("", hwcomposer=hwcomposer, arch=arch)
                self.assertIn("androidboot.opengles.version=196610", cmdline.split())

    def test_fr4_android_starts_at_the_main_monitors_density(self):
        # Image test run 42562909966: without ro.sf.lcd_density SurfaceFlinger fell back to
        # 213 dpi, the host then asked each task for 160, and that density change relaunched
        # every app right after it opened; hello-vulkan crashed in the relaunch. The density
        # is now the boot's: the image names 160 for both the hwcomposer and SurfaceFlinger,
        # and the host replaces both with its main monitor's.
        cmdline = make_image.kernel_cmdline(f"blkdevparts={make_image.SYSTEM_DISK_GUID}:1@0(misc)", hwcomposer=True)
        self.assertIn(f"androidboot.dsa.display_dpi={make_image.DISPLAY_DPI}", cmdline)
        self.assertIn(f"androidboot.lcd_density={make_image.DISPLAY_DPI}", cmdline)
        self.assertIn("androidboot.lcd_density=130", make_image.kernel_cmdline("", hwcomposer=False))
        # No build.prop sets it, the input's included: init reads them before any script, and
        # the first value would hold every boot at the image's density.
        for vendor in (False, True):
            props = make_image.edit_props("ro.debuggable=1\nro.sf.lcd_density=320\n", vendor=vendor)
            self.assertNotIn("ro.sf.lcd_density", props)

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

    def test_fr36_the_agents_backup_transport_is_listed_for_the_backup_manager(self):
        # Android's backup manager uses a transport only when a sysconfig file lists it.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = self._target_files_with_system_ext(root)
            agent = root / "agent.apk"
            agent.write_bytes(b"PK agent")
            modified = root / "modified.zip"
            make_image.modify(source, modified, agent=agent)
            with zipfile.ZipFile(modified) as archive:
                listed = archive.read(make_image.AGENT_SYSCONFIG).decode()
                fs_config = archive.read("META/system_ext_filesystem_config.txt").decode().splitlines()
        self.assertIn('<backup-transport-whitelisted-service', listed)
        self.assertIn('service="io.github.thisisthepy.dsa.agent/'
                      'io.github.thisisthepy.dsa.agent.platform.HostBackupTransportService"', listed)
        self.assertIn("system_ext/etc/sysconfig/sysconfig-dsa-agent.xml 0 0 644 capabilities=0x0", fs_config)
        manifest = (make_image.REPO / "dsa/guest/agent/src/main/AndroidManifest.xml").read_text()
        self.assertIn('android:name=".platform.HostBackupTransportService"', manifest)

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

    def test_fr26_the_sensors_hal_removes_a_plain_file_multihal(self):
        # Android 17's Cuttlefish installs its sensors multi-HAL as plain vendor files. Left
        # beside DSA's HAL, init starts it and it aborts every five seconds because its
        # sub-HAL finds no Cuttlefish sensors control socket.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            sensors = root / "dsa-sensors"
            sensors.write_bytes(b"\x7fELF sensors")
            multihal = [
                "VENDOR/bin/hw/android.hardware.sensors-service.multihal",
                "VENDOR/etc/init/android.hardware.sensors-service-multihal.rc",
                "VENDOR/etc/sensors/hals.conf",
                "VENDOR/lib64/hw/android.hardware.sensors@2.1-impl.cuttlefish.so",
            ]
            kept = [
                "VENDOR/lib64/android.hardware.sensors-V3-ndk.so",
                "VENDOR/lib64/android.hardware.sensors@2.1.so",
                "VENDOR/bin/hw/android.hardware.gnss-service.example",
            ]
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
                for name in multihal + kept:
                    archive.writestr(name, b"x")

            modified = root / "modified.zip"
            make_image.modify(source, modified, sensors=sensors)

            with zipfile.ZipFile(modified) as archive:
                names = set(archive.namelist())
                for name in multihal:
                    self.assertNotIn(name, names)
                for name in kept:
                    self.assertIn(name, names)
                self.assertIn("VENDOR/bin/hw/dsa-sensors", names)
                self.assertIn("VENDOR/etc/init/dsa-sensors.rc", names)

    def test_fr26_check_image_rejects_a_second_sensors_service(self):
        check_script = Path(__file__).with_name("check-image.py")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (keys_dir / "test.x509.pem").write_text("-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n")
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": "console=ttyS0"}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                archive.writestr("META/apkcerts.txt", "")
                archive.writestr("VENDOR/bin/hw/dsa-sensors", b"x")
                archive.writestr("VENDOR/etc/init/dsa-sensors.rc", "service vendor.dsa-sensors\n")
                archive.writestr("VENDOR/bin/hw/android.hardware.sensors-service.multihal", b"x")
                archive.writestr("VENDOR/etc/sensors/hals.conf", "x\n")
            proc = subprocess.run(
                [sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                capture_output=True,
                text=True,
            )
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("VENDOR/bin/hw/android.hardware.sensors-service.multihal is a second sensors HAL", proc.stdout)
        self.assertIn("VENDOR/etc/sensors/hals.conf lists sub-HALs", proc.stdout)
        self.assertNotIn("VENDOR/bin/hw/dsa-sensors is a second", proc.stdout)

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
        self.assertIn("androidboot.dsa.display_dpi=130", cmdline)
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

    def _modify(self, root, channeld=False, extra=None, policy="(type init)\n"):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", policy)
            archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
            for name, data in (extra or {}).items():
                archive.writestr(name, data)
        daemon = None
        client = None
        if channeld:
            daemon = root / "dsa-channeld"
            daemon.write_bytes(b"daemon")
            client = root / "su"
            client.write_bytes(b"client")
        modified = root / "modified.zip"
        make_image.modify(source, modified, channeld=daemon, su_client=client)
        return zipfile.ZipFile(modified)

    # The rules on the memfd types the published Android 16 and 17 policies already carry.
    PLATFORM_MEMFD_RULES = (
        "(type init)\n"
        "(typeattributeset mlstrustedobject (ashmem_device system_server_tmpfs traced_tmpfs))\n"
        "(allow appdomain audioserver_tmpfs (file (read write getattr map)))\n"
        "(allow appdomain system_server_tmpfs (file (read write getattr map)))\n"
        "(allow hal_audio_server system_server_tmpfs (file (read getattr map)))\n"
        "(allow mediaextractor mediaserver_tmpfs (file (read write getattr map)))\n"
    )

    def test_pr4_the_platform_policy_alone_refuses_apps_the_audio_control_block(self):
        problems = make_image.memfd_sharing_problems(self.PLATFORM_MEMFD_RULES)
        self.assertTrue(any(p.startswith("audioserver_tmpfs is not mlstrustedobject") for p in problems), problems)
        self.assertIn("the SELinux policy does not let mediaserver getattr map read write the memfds labelled "
                      "mediaextractor_tmpfs, so the binder calls that carry them fail", problems)

    def test_pr4_the_image_lets_apps_and_the_media_services_share_their_memfds(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), policy=self.PLATFORM_MEMFD_RULES) as archive:
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        self.assertEqual(make_image.memfd_sharing_problems(policy), [])

    def test_fr26_the_sensors_hal_may_use_the_event_queue_system_server_hands_it(self):
        sensors = make_image.MEMFD_SHARING_SENSORS
        refused = make_image.memfd_sharing_problems(self.PLATFORM_MEMFD_RULES, sharing=sensors, to_apps=[])
        self.assertIn("the SELinux policy does not let hal_sensors_server getattr map read write the memfds "
                      "labelled system_server_tmpfs, so the binder calls that carry them fail", refused)
        policy = self.PLATFORM_MEMFD_RULES + make_image.SENSORS_POLICY.read_text()
        self.assertEqual(make_image.memfd_sharing_problems(policy, sharing=sensors, to_apps=[]), [])

    def test_fr4_the_boot_density_reaches_surfaceflinger_through_a_vendor_script(self):
        rc = (Path(make_image.__file__).with_name("dsa-display.rc")).read_text()
        self.assertIn("on early-init\n    setprop ro.sf.lcd_density ${ro.boot.lcd_density:-130}\n", rc)
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            self.assertEqual(archive.read("VENDOR/etc/init/dsa-display.rc").decode(), rc)
            self.assertIn(
                "vendor/etc/init/dsa-display.rc 0 0 644 capabilities=0x0",
                archive.read("META/vendor_filesystem_config.txt").decode(),
            )

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

    def test_nfr1_the_idle_reclaim_compresses_what_stays_in_zram_again_with_zstd(self):
        # LZ4 for every swap, zstd for what stays cold: zstd must be named before swapon_all
        # sizes the device, and the reclaim compresses again only after it swapped, in a
        # bounded step, and compacts zram before the caches go.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            memory_rc = archive.read("VENDOR/etc/init/dsa-memory.rc").decode()
            name = 'write /sys/devices/virtual/block/zram0/recomp_algorithm "algo=zstd priority=1"'
            # The order of the commands, not of the words in the comments above them.
            commands = [line.strip() for line in memory_rc.splitlines() if not line.lstrip().startswith("#")]
            self.assertIn(name, commands)
            swapon = next(i for i, command in enumerate(commands) if command.startswith("swapon_all "))
            self.assertLess(commands.index(name), swapon)
            rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode()
            start = rc.index("on property:vendor.dsa.memory.reclaim=1 && property:sys.boot_completed=1")
            trigger = rc[start:rc.index("on property:vendor.dsa.memory.trim=1")]
            idle = trigger.index("write /sys/devices/virtual/block/zram0/idle all")
            again = trigger.index('write /sys/devices/virtual/block/zram0/recompress "type=idle max_pages=16384"')
            compact = trigger.index("write /sys/devices/virtual/block/zram0/compact 1")
            self.assertLess(trigger.rindex("cgroup/memory.reclaim"), idle)
            self.assertLess(idle, again)
            self.assertLess(again, compact)
            self.assertLess(compact, trigger.index("drop_caches"))
            # The working set trim, which runs while apps are in use, leaves zram alone.
            trim = rc[rc.index("on property:vendor.dsa.memory.trim=1"):].split("\n\n")[0]
            self.assertNotIn("zram0", trim)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn("(allow vendor_init sysfs_zram (file (write open getattr)))", policy)

    def test_nfr1_zram_has_a_backing_file_on_the_data_disk(self):
        # fs_mgr makes /data/per_boot/zram_swap before it sizes zram, but only in a folder that
        # exists; a system script makes it (vendor_init may not write the top of /data), with a
        # label the loop driver may read and write, and removes the last boot's file first.
        fstab = (Path(make_image.__file__).with_name("fstab.dsa")).read_text()
        entries = [line.split() for line in fstab.splitlines() if line.startswith("/dev/block/zram0")]
        self.assertEqual(len(entries), 1)
        self.assertIn("zram_backingdev_size=512M", entries[0][4].split(","))
        contexts = {"SYSTEM/etc/selinux/plat_file_contexts": "/system/bin/sh u:object_r:shell_exec:s0\n"}
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), extra=contexts) as archive:
            for directory in make_image.FSTAB_DIRS:
                self.assertIn(b"zram_backingdev_size=512M", archive.read(directory + "fstab.dsa"))
            rc = archive.read(make_image.ZRAM_BACKING_RC).decode()
            trigger = rc[rc.index("on post-fs-data\n"):]
            # As Android's init.rc makes it when the boot completes: the last boot's folder
            # goes, and the new one has init.rc's owner and per-boot key. A folder left with
            # another key makes init.rc's mkdir ask for a wipe of the user's data.
            remove = trigger.index("exec - system system -- /bin/rm -rf /data/per_boot")
            mkdir = trigger.index(
                "mkdir /data/per_boot 0700 system system encryption=Require key=per_boot_ref"
            )
            self.assertLess(remove, mkdir)
            self.assertIn(
                "system/etc/init/dsa-zram-backing.rc 0 0 644 capabilities=0x0",
                archive.read("META/filesystem_config.txt").decode(),
            )
            contexts = archive.read("SYSTEM/etc/selinux/plat_file_contexts").decode().splitlines()
            self.assertEqual(contexts.count(make_image.ZRAM_BACKING_CONTEXT), 1)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn("(type dsa_zram_backing_file)", policy)
            self.assertIn("(allow kernel dsa_zram_backing_file (file (read write getattr)))", policy)
            self.assertIn(
                "(allow init dsa_zram_backing_file (file (create open read write getattr setattr unlink relabelto)))",
                policy,
            )
            # No other domain may open what zram wrote: it is apps' memory. toolbox's rm may
            # only unlink it and remove the folder.
            file_rules = re.findall(r"\(allow (\S+) dsa_zram_backing_file \(file \(([^)]*)\)\)\)", policy)
            readers = {domain for domain, perms in file_rules if {"open", "read"} & set(perms.split())}
            self.assertEqual(readers, {"init", "kernel"})
            self.assertIn("(allow toolbox dsa_zram_backing_file (file (getattr unlink)))", policy)
            self.assertIn(
                "(allow toolbox dsa_zram_backing_file (dir (getattr open read search write remove_name rmdir)))",
                policy,
            )
        # A label the build already has for the folder goes, as two would conflict.
        text = make_image.with_zram_backing_context("/data/per_boot(/.*)? u:object_r:system_data_file:s0\n")
        self.assertEqual(text.splitlines(), [make_image.ZRAM_BACKING_CONTEXT])

    def test_nfr1_zram_writes_nothing_back_until_the_daemon_gives_it_a_budget(self):
        # The limit goes on as the swap starts, with no budget, so nothing writes to the disk
        # until dsa-channeld asks; the times the daemon and init use come from the boot or the
        # defaults.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-memory.rc").decode()
            enable = rc.index("write /sys/devices/virtual/block/zram0/writeback_limit_enable 1")
            self.assertLess(rc.index("swapon_all /vendor/etc/fstab.dsa"), enable)
            self.assertIn(
                "setprop vendor.dsa.memory.writeback_after ${ro.boot.dsa.zram_writeback_after:-600}", rc)
            self.assertIn(
                "setprop vendor.dsa.memory.writeback_age ${ro.boot.dsa.zram_writeback_age:-1800}", rc)

    def test_nfr1_the_zram_writeback_writes_only_old_pages_within_its_budget(self):
        # The run's budget is set first and taken away last; only pages unused for the age are
        # marked idle, incompressible ones go first, and the freed memory is reported again.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode()
            start = rc.index("on property:vendor.dsa.memory.writeback=1 && property:sys.boot_completed=1")
            trigger = rc[start:].split("\n\n")[0]
            zram = "write /sys/devices/virtual/block/zram0/"
            budget = trigger.index(zram + "writeback_limit ${vendor.dsa.memory.writeback_pages}")
            age = trigger.index(zram + "idle ${vendor.dsa.memory.writeback_age}")
            huge = trigger.index(zram + "writeback huge_idle")
            idle = trigger.index(zram + "writeback idle")
            closed = trigger.index(zram + "writeback_limit 0")
            compact = trigger.index(zram + "compact 1")
            again = trigger.index("write /sys/module/page_reporting/parameters/rereport 1")
            done = trigger.index("setprop vendor.dsa.memory.writeback 0")
            self.assertEqual(sorted([budget, age, huge, idle, closed, compact, again, done]),
                             [budget, age, huge, idle, closed, compact, again, done])
            self.assertNotIn("idle all", trigger)
            contexts = archive.read("VENDOR/etc/selinux/vendor_property_contexts").decode().splitlines()
            for context in make_image.WRITEBACK_PROPERTY_CONTEXTS.values():
                self.assertIn(context, contexts)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            # The daemon reads what was written; only init writes zram's files.
            self.assertIn("(allow dsa_channeld sysfs_zram (file (read open getattr)))", policy)
            self.assertNotRegex(policy, r"\(allow dsa_channeld sysfs_zram \(file \([^)]*write")

    def test_nfr2_the_idle_reclaim_reports_every_free_page_to_the_host_again(self):
        # After the reclaim has freed and compacted memory, the kernel reports every free page
        # again, those a host that faults in clusters took back in included.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode()
            start = rc.index("on property:vendor.dsa.memory.reclaim=1 && property:sys.boot_completed=1")
            trigger = rc[start:].split("\n\n")[0]
            again = trigger.index("write /sys/module/page_reporting/parameters/rereport 1")
            self.assertLess(trigger.index("write /proc/sys/vm/compact_memory 1"), again)
            self.assertLess(again, trigger.index("setprop vendor.dsa.memory.reclaim 0"))
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn(
                '(genfscon sysfs "/module/page_reporting/parameters/rereport" '
                '(u object_r dsa_sysfs_page_reporting ((s0) (s0))))',
                policy,
            )
            self.assertIn("(allow vendor_init dsa_sysfs_page_reporting (file (write open getattr)))", policy)

    def test_nfr1_free_memory_is_compacted_sooner_so_page_reporting_hands_it_back(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-memory.rc").decode()
            self.assertIn("write /proc/sys/vm/compaction_proactiveness 40", rc)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn(
                '(genfscon proc "/sys/vm/compaction_proactiveness" (u object_r dsa_proc_vm_tunable',
                policy,
            )

    def test_nfr2_free_pages_do_not_pile_up_on_the_cpus_lists_where_the_host_never_gets_them(self):
        # Free pages on the per-CPU lists are not reported to the host; by default the lists may
        # grow from the low watermark (raised by extra_free_kbytes) up to an eighth of a zone.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-memory.rc").decode()
            boot = rc[rc.index("on boot"):]
            self.assertIn("write /proc/sys/vm/percpu_pagelist_high_fraction 1048576", boot)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn(
                '(genfscon proc "/sys/vm/percpu_pagelist_high_fraction" '
                '(u object_r dsa_proc_vm_tunable',
                policy,
            )

    def test_nfr1_dentries_and_inodes_are_reclaimed_twice_as_readily(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-memory.rc").decode()
            self.assertIn("write /proc/sys/vm/vfs_cache_pressure 200", rc)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn(
                '(genfscon proc "/sys/vm/vfs_cache_pressure" (u object_r dsa_proc_vm_tunable',
                policy,
            )

    def test_nfr1_system_server_starts_no_service_for_a_radio_virt_wifi_does_not_have(self):
        # virt_wifi is a station on the host's network: no Wi-Fi Direct, Aware or round-trip
        # timing, and the Thread HAL is removed. Declared, system_server starts a service and
        # its threads for each.
        self.assertIn("com.android.hardware.threadnetwork", make_image.REMOVED_APEXES)
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            xml = archive.read("VENDOR/etc/permissions/dsa-unavailable-features.xml").decode()
        for feature in ["android.hardware.wifi.direct", "android.hardware.wifi.aware",
                        "android.hardware.wifi.rtt", "android.hardware.thread_network"]:
            self.assertIn(f'<unavailable-feature name="{feature}" />', xml)
        # Wi-Fi itself stays: apps see the host's network as a Wi-Fi network.
        self.assertNotIn('<unavailable-feature name="android.hardware.wifi" />', xml)

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

    def test_nfr1_the_direct_mapped_table_maps_the_logical_partitions_without_verity(self):
        # The host's direct-mapped mode attaches the system disk as persistent memory; erofs maps
        # the files with dax=always, which dm-verity cannot pass through, so those entries lose
        # their verity flags and nothing else changes.
        fstab = (Path(make_image.__file__).with_name("fstab.dsa")).read_text()
        mapped = make_image.direct_mapped_fstab(fstab)
        normal = [line.split() for line in fstab.splitlines() if line and not line.startswith("#")]
        changed = [line.split() for line in mapped.splitlines() if line and not line.startswith("#")]
        self.assertEqual(len(normal), len(changed))
        logical = 0
        for before, after in zip(normal, changed):
            self.assertEqual(before[:3], after[:3])
            if before[0] in make_image.DIRECT_MAPPED_VERIFIED:
                # odm keeps dm-verity, so first stage init still checks the verified boot data.
                self.assertEqual(before, after)
            elif "logical" in before[4].split(","):
                logical += 1
                self.assertEqual(after[3].split(","), before[3].split(",") + ["dax=always"], before[1])
                self.assertFalse([f for f in after[4].split(",") if f.split("=")[0] in ("avb", "avb_keys")],
                                 before[1])
                self.assertEqual([f for f in before[4].split(",") if f.split("=")[0] not in ("avb", "avb_keys")],
                                 after[4].split(","), before[1])
            else:
                # /data, /metadata, misc, zram and the verified boot data of the boot partitions.
                self.assertEqual(before, after)
        self.assertGreaterEqual(logical, 6)
        # Every image carries it beside fstab.dsa, in the partition and the first stage ramdisk.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            names = set(archive.namelist())
            for directory in make_image.FSTAB_DIRS:
                self.assertEqual(archive.read(directory + "fstab.dsa_dax").decode(), mapped)
            self.assertIn("vendor/etc/fstab.dsa_dax 0 0 644 capabilities=0x0",
                          archive.read("META/vendor_filesystem_config.txt").decode())
            self.assertEqual(make_image.direct_mapped_problems(names, archive.read,
                                                               make_image.direct_mapped_entry(False)), [])
        self.assertEqual(make_image.direct_mapped_entry(True),
                         {"fstab_suffix": "dsa_dax", "uncompressed_erofs": True,
                          "verified_partitions": ["odm"]})

    def test_nfr1_the_direct_mapped_table_still_verifies_odm_so_vbmeta_is_checked(self):
        # First stage init opens verified boot only for an entry it mounts with dm-verity; odm
        # keeps its entry as it is, so the vbmeta images and the boot images' hashes are
        # checked, and ro.boot.avb_version is set, as in a normal boot.
        fstab = (Path(make_image.__file__).with_name("fstab.dsa")).read_text()
        mapped = make_image.direct_mapped_fstab(fstab)
        odm = [line.split() for line in mapped.splitlines() if line.startswith("odm ")]
        self.assertEqual(len(odm), 2)
        for fields in odm:
            self.assertIn("avb", fields[4].split(","))
            self.assertIn("first_stage_mount", fields[4].split(","))
            self.assertNotIn("dax=always", fields[3].split(","))
        broken = mapped.replace("odm /odm erofs ro wait,logical,first_stage_mount,slotselect,avb,nofail",
                                "odm /odm erofs ro,dax=always wait,logical,first_stage_mount,slotselect,nofail")
        files = {"VENDOR/etc/fstab.dsa": fstab.encode(), "VENDOR/etc/fstab.dsa_dax": broken.encode()}
        problems = make_image.direct_mapped_problems(set(files), files.__getitem__,
                                                     make_image.direct_mapped_entry(False))
        self.assertTrue(any("does not verify /odm" in p for p in problems), problems)

    def test_nfr1_image_json_may_describe_the_hosts_copy_of_the_system_disk(self):
        fstab = (Path(make_image.__file__).with_name("fstab.dsa")).read_text()
        files = {"VENDOR/etc/fstab.dsa": fstab.encode(),
                 "VENDOR/etc/fstab.dsa_dax": make_image.direct_mapped_fstab(fstab).encode()}
        entry = make_image.direct_mapped_entry(False)
        entry["system_disk"] = {"format": 1, "bytes": 2304 << 20, "sha256": "ab" * 32, "super_bytes": 2048 << 20}
        self.assertEqual(make_image.direct_mapped_problems(set(files), files.__getitem__, entry), [])
        entry["system_disk"]["bytes"] += 4096
        self.assertEqual(len(make_image.direct_mapped_problems(set(files), files.__getitem__, entry)), 1)

    def test_nfr1_check_image_refuses_a_direct_mapped_table_that_still_verifies(self):
        fstab = (Path(make_image.__file__).with_name("fstab.dsa")).read_text().encode()
        files = {"VENDOR/etc/fstab.dsa": fstab, "VENDOR/etc/fstab.dsa_dax": fstab}
        problems = make_image.direct_mapped_problems(set(files), files.__getitem__,
                                                     make_image.direct_mapped_entry(False))
        self.assertTrue(any("is not the direct-mapped table" in p for p in problems), problems)
        self.assertTrue(any("without dax=always" in p for p in problems), problems)
        self.assertTrue(any("sets up dm-verity" in p for p in problems), problems)
        del files["VENDOR/etc/fstab.dsa_dax"]
        self.assertEqual(make_image.direct_mapped_problems(set(files), files.__getitem__,
                                                           make_image.direct_mapped_entry(False)),
                         ["VENDOR/etc/fstab.dsa_dax is missing, so a direct-mapped boot finds no mount table"])
        self.assertEqual(len(make_image.direct_mapped_problems(set(files), files.__getitem__, None)), 1)

    def test_nfr1_uncompressed_erofs_builds_every_read_only_partition_whole(self):
        # erofs maps only files stored whole (FLAT_PLAIN or chunk-based); the published build
        # compresses with lz4hc and keeps APEXes whole through its hints.
        published = ("erofs_default_compress_hints=device/google/cuttlefish/shared/erofs_compress_hints.txt\n"
                     "erofs_default_compressor=lz4hc,9\n"
                     "erofs_sparse_flag=-s\n"
                     "system_erofs_compressor=lz4\n"
                     "vendor_erofs_pcluster_size=262144\n"
                     "system_fs_type=erofs\n")
        info = make_image.uncompressed_erofs_misc_info(published)
        settings = dict(line.split("=", 1) for line in info.splitlines())
        self.assertEqual(settings["erofs_default_compressor"], "none")
        self.assertEqual(settings["erofs_share_dup_blocks"], "true")
        self.assertEqual(settings["erofs_sparse_flag"], "-s")
        self.assertEqual(settings["system_fs_type"], "erofs")
        for key in ("erofs_default_compress_hints", "system_erofs_compressor", "vendor_erofs_pcluster_size"):
            self.assertNotIn(key, settings)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("META/misc_info.txt", published)
            for uncompressed in (False, True):
                modified = root / f"modified-{uncompressed}.zip"
                make_image.modify(source, modified, uncompressed_erofs=uncompressed)
                with zipfile.ZipFile(modified) as archive:
                    text = archive.read("META/misc_info.txt").decode()
                    self.assertEqual(text, info if uncompressed else published)
                    problems = make_image.direct_mapped_problems(
                        set(archive.namelist()), archive.read, make_image.direct_mapped_entry(True))
                    self.assertEqual(problems == [], uncompressed, problems)

    def test_nfr1_the_system_disk_is_a_whole_number_of_persistent_memory_sections(self):
        self.assertEqual(make_image.SYSTEM_DISK_ALIGNMENT, 2 << 20)
        source = Path(make_image.__file__).read_text()
        self.assertIn("system_disk_size = ((needed + SYSTEM_DISK_ALIGNMENT - 1) // SYSTEM_DISK_ALIGNMENT)",
                      source)

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

    def test_nfr1_the_shell_can_ask_root_for_the_kernel_memory_report_only_in_developer_mode(self):
        # The shell of a user build cannot read /proc/slabinfo or /proc/vmallocinfo; the report
        # runs as root in a domain that may only read, and only while adb is on.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=True) as archive:
            rc = archive.read("VENDOR/etc/init/dsa-channeld.rc").decode().splitlines()
            service = rc.index("service dsa_memory_report /vendor/bin/dsa-channeld --memory-report")
            block = rc[service:rc.index("", service)]
            for line in ["    user root", "    group root readproc", "    seclabel u:r:dsa_memory_report:s0",
                         "    oneshot", "    disabled", "    stdio_to_kmsg"]:
                self.assertIn(line, block)
            trigger = rc.index("on property:debug.dsa.memory_report=1 && property:vendor.dsa.adb=1")
            self.assertEqual(rc[trigger + 1], "    start dsa_memory_report")
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn("(typeattributeset domain (dsa_memory_report))", policy)
            for kind in ["proc_slabinfo", "proc_vmallocinfo", "proc_meminfo"]:
                self.assertIn(f"(allow dsa_memory_report {kind} (file (read open getattr)))", policy)
            # It writes nothing but its log.
            writes = re.findall(r"\(allow dsa_memory_report (\S+) \((\S+) \([^)]*\b(?:write|append)\b", policy)
            self.assertEqual(sorted(writes), [("kmsg_debug_device", "chr_file"), ("kmsg_device", "chr_file")])

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
        self.assertIn("dalvik.vm.heapmaxfree=4m", props)
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
            + "".join(context + "\n" for context in make_image.WRITEBACK_PROPERTY_CONTEXTS.values())
            + make_image.BOOT_PROPERTY_CONTEXT + "\n"
            + make_image.ADB_PROPERTY_CONTEXT + "\n"
            + make_image.SHARES_PROPERTY_CONTEXT + "\n"
            + make_image.FILES_PROPERTY_CONTEXT + "\n"
            + make_image.ROOT_ELEVATION_PROPERTY_CONTEXT + "\n"
            + make_image.DNS_PROPERTY_CONTEXT + "\n"
            + "".join(context + "\n" for context in make_image.GAPPS_COMPILE_PROPERTY_CONTEXTS.values())
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
        # The phone process (TeleService) is a privileged system app.
        removed_paths = {"TeleService": "SYSTEM/priv-app/TeleService/TeleService.apk"}
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
        extra.update({name: b"apk" for name in removed_paths.values()})
        extra.update({name: b"apk" for name in kept})
        # Settings.apk is opened to edit its sound screen, and DocumentsUI.apk to edit its
        # list's room, so each has to be an APK.
        extra["SYSTEM/priv-app/Settings/Settings.apk"] = SettingsWithoutBluetoothTests.apk({})
        extra["SYSTEM/priv-app/DocumentsUI/DocumentsUI.apk"] = SettingsWithoutBluetoothTests.apk({})
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
            for app, name in removed_paths.items():
                self.assertNotIn(name, names)
                self.assertNotIn(f'name="{app}.apk"', certs)
            for name in kept:
                self.assertIn(name, names)
                self.assertIn(f'name="{Path(name).name}"', certs)
        for needed in ["webview", "ContactsProvider", "PackageInstaller", "PermissionController",
                       "DocumentsUI", "PrintSpooler", "Settings", "Provision", "Telecom",
                       "TelephonyProvider", "CarrierConfig", "DeskClock", "CalendarProvider",
                       "DownloadProvider", "MusicFX"]:
            self.assertNotIn(needed, make_image.REMOVED_APPS)

    def test_nfr1_the_phone_process_is_not_in_the_image(self):
        # com.android.phone is persistent and does nothing without the telephony feature; the
        # image withdraws that feature and leaves TeleService out. Telecom and the telephony
        # provider stay.
        self.assertIn("TeleService", make_image.REMOVED_APPS)
        self.assertTrue(make_image.removed("SYSTEM/priv-app/TeleService/TeleService.apk"))
        self.assertTrue(make_image.removed("SYSTEM/priv-app/TeleService/oat/x86_64/TeleService.odex"))
        self.assertFalse(make_image.removed("SYSTEM/priv-app/Telecom/Telecom.apk"))
        self.assertFalse(make_image.removed("SYSTEM/priv-app/TelephonyProvider/TelephonyProvider.apk"))
        features = (Path(__file__).parent / "unavailable-features.xml").read_text()
        self.assertIn('<unavailable-feature name="android.hardware.telephony" />', features)

    def test_nfr1_no_persistent_app_is_left_in_the_phone_process_without_its_permissions(self):
        # The opportunistic network service shares the phone's user ID and process and is
        # persistent, but the network permissions it uses come from TeleService's manifest.
        # Without TeleService it crash-looped at boot until the rescue party rebooted Android,
        # so it leaves the image with TeleService.
        self.assertIn("ONS", make_image.REMOVED_APPS)
        self.assertTrue(make_image.removed("SYSTEM/priv-app/ONS/ONS.apk"))
        self.assertTrue(make_image.removed("SYSTEM/priv-app/ONS/oat/x86_64/ONS.odex"))
        self.assertFalse(make_image.removed("SYSTEM/priv-app/CarrierConfig/CarrierConfig.apk"))
        self.assertFalse(make_image.removed("SYSTEM/priv-app/TelephonyProvider/TelephonyProvider.apk"))

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
        self.assertEqual(
            make_image.FRAMEWORK_OVERLAY_INTEGERS,
            {"config_tapTimeoutMillis": 0, "config_customizedMaxCachedProcesses": 8},
        )
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
            self.assertIn('<bool name="config_appCompatUserAppAspectRatioSettingsIsEnabled">true</bool>', values)

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

    def test_fr22_the_system_menus_aspect_ratio_takes_effect(self):
        # Settings stores the user's choice either way; the window manager applies it only
        # while the framework's flag is on, and Android's default leaves it off.
        self.assertIs(
            make_image.FRAMEWORK_OVERLAY_BOOLEANS["config_appCompatUserAppAspectRatioSettingsIsEnabled"], True)

    def test_nfr4_the_boot_does_not_wait_for_a_wallpaper_the_image_cannot_draw(self):
        # The default wallpaper is SystemUI's, and SystemUI is removed: a window manager
        # that waits for a wallpaper at boot waits its whole 30 s boot timeout.
        self.assertIn("SystemUI", make_image.SHELL_APPS)
        self.assertIs(make_image.FRAMEWORK_OVERLAY_BOOLEANS["config_checkWallpaperAtBoot"], False)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_image.framework_overlay_sources(root / "src")
            values = (root / "src/res/values/dsa.xml").read_text()
        self.assertIn('<bool name="config_checkWallpaperAtBoot">false</bool>', values)
        self.assertIn("bool/config_checkWallpaperAtBoot", make_image.framework_overlaid())

    def test_nfr2_google_play_services_is_the_device_configurator(self):
        # WRITE_DEVICE_CONFIG is a "configurator" permission. With no configurator named, Play
        # services' platform configurator could never write the flags it syncs, and retried
        # about once a second after boot (its RETRY_UPDATE alarm).
        self.assertEqual(make_image.FRAMEWORK_OVERLAY_STRINGS["config_deviceConfiguratorPackageName"],
                         "com.google.android.gms")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_image.framework_overlay_sources(root / "src")
            values = ET.fromstring((root / "src/res/values/dsa.xml").read_text())
        strings = {item.get("name"): item.text for item in values.findall("string")}
        self.assertEqual(strings["config_deviceConfiguratorPackageName"], "com.google.android.gms")
        # The overlay is checked against the image's framework-res, so a renamed resource fails
        # the image check instead of leaving Play services without the permission again.
        self.assertIn("string/config_deviceConfiguratorPackageName", make_image.framework_overlaid())

    def test_fr36_the_agent_is_the_network_location_provider_and_without_play_the_geocode_provider(self):
        # With the overlay flags on, the location manager picks the provider of the highest
        # service version, which is Google Play's once it is installed.
        self.assertIs(make_image.FRAMEWORK_OVERLAY_BOOLEANS["config_enableNetworkLocationOverlay"], False)
        self.assertIs(make_image.FRAMEWORK_OVERLAY_BOOLEANS["config_enableGeocoderOverlay"], False)
        agent = "io.github.thisisthepy.dsa.agent"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_image.framework_overlay_sources(root / "src")
            values = (root / "src/res/values/dsa.xml").read_text()
        self.assertIn(
            f'<string name="config_networkLocationProviderPackageName" translatable="false">{agent}</string>', values)
        self.assertIn(
            f'<string name="config_geocoderProviderPackageName" translatable="false">{agent}</string>', values)
        self.assertIn('<bool name="config_enableGeocoderOverlay">false</bool>', values)
        self.assertIn("string/config_geocoderProviderPackageName", make_image.framework_overlaid())

    def test_fr36_network_location_and_geocoder_come_from_the_host_and_google_plays_geocoder_wins_with_play(self):
        # The Google apps overlay applies only on a boot with the Google apps image: the property
        # is the one the host's bootconfig sets and the image's own Google apps import reads.
        overlay = make_image.system_overlay(make_image.GAPPS_FRAMEWORK_OVERLAY_APK)
        self.assertEqual(overlay["target"], "android")
        self.assertEqual(overlay["required_property"], ("ro.boot.dsa.gapps", "on"))
        self.assertIn("${ro.boot.dsa.gapps:-off}", make_image.GAPPS_IMPORT)
        self.assertIn(make_image.GAPPS_FRAMEWORK_OVERLAY_APK, make_image.DESIGN_OVERLAYS)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_image.design_overlay_sources(root / "src", make_image.GAPPS_FRAMEWORK_OVERLAY_APK)
            manifest = ET.fromstring((root / "src/AndroidManifest.xml").read_text())
            values = (root / "src/res/values/dsa.xml").read_text()
        element = manifest.find("overlay")
        self.assertEqual(element.get(NS + "requiredSystemPropertyName"), "ro.boot.dsa.gapps")
        self.assertEqual(element.get(NS + "requiredSystemPropertyValue"), "on")
        self.assertEqual(element.get(NS + "isStatic"), "true")
        # Above the framework overlay, whose geocoder values it replaces.
        base = ET.fromstring(make_image.overlay_manifest(make_image.FRAMEWORK_OVERLAY_PACKAGE, "android"))
        self.assertGreater(int(element.get(NS + "priority")), int(base.find("overlay").get(NS + "priority")))
        self.assertIsNone(base.find("overlay").get(NS + "requiredSystemPropertyName"))
        # With Google Play the location manager binds Google's geocoder and no other, so DSA is
        # not the geocode provider.
        self.assertIn('<bool name="config_enableGeocoderOverlay">false</bool>', values)
        self.assertIn('<string name="config_geocoderProviderPackageName" translatable="false">'
                      'com.google.android.gms</string>', values)
        # The network location provider is not touched: the host's stays, with or without Play.
        self.assertNotIn("NetworkLocation", values)
        self.assertEqual(set(make_image.overlaid_resources(make_image.GAPPS_FRAMEWORK_OVERLAY_APK)),
                         {"bool/config_enableGeocoderOverlay", "string/config_geocoderProviderPackageName"})

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
            # SettingsLib's icon is in drawable-v36; an overlay's value is used only where its
            # configuration matches at least as well as the one it replaces.
            for folder, theme in [("drawable-v36", "light"), ("drawable-night-v36", "dark")]:
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

    def test_fr14_a_remade_image_gets_a_revision_no_release_has(self):
        # The v0.1.9 image release rebuilt the images under 17.0.0-16373615-9 and replaced that
        # release's files: a host that had -9 saw no update, and its hashes changed under it.
        spec = {"version": "17.0.0-16373615-9", "build_id": "16373615", "revision": 9}
        arm = dict(spec, version="17.0.0-16373615-9-arm64")
        tags = ["android-17.0.0-16373615-9", "android-17.0.0-16373615-8",
                "android-17.0.0-16373615-12-arm64", "android-16.0.0-15581820-30", "v0.1.9"]
        self.assertEqual(make_image.next_revision(spec, tags), 10)
        self.assertEqual(make_image.next_revision(arm, tags), 13)
        # Nothing published yet: inputs.json's revision.
        self.assertEqual(make_image.next_revision(spec, []), 9)
        self.assertEqual(make_image.revised(spec, 10)["version"], "17.0.0-16373615-10")
        self.assertEqual(make_image.revised(spec, 10)["revision"], 10)
        self.assertEqual(make_image.revised(arm, 13)["version"], "17.0.0-16373615-13-arm64")
        with self.assertRaises(ValueError):
            make_image.revised(spec, 8)
        # The build number is the AOSP build times 100 plus the revision, so 100 is refused.
        with self.assertRaises(ValueError):
            make_image.revised(spec, 100)
        with self.assertRaises(ValueError):
            make_image.next_revision(spec, ["android-17.0.0-16373615-99"])

    def test_fr14_a_development_build_names_its_number_after_the_revision(self):
        spec = {"version": "17.0.0-16373615-9", "build_id": "16373615", "revision": 9}
        dev = make_image.as_development_build(make_image.revised(spec, 15), 42)
        self.assertEqual(dev["version"], "17.0.0-16373615-15-dev.42")
        # The build number the image gets is the revision's, unchanged.
        self.assertEqual(dev["revision"], 15)
        for bad in (0, -1, "7"):
            with self.assertRaises(ValueError):
                make_image.as_development_build(spec, bad)

    def test_fr14_the_release_publishes_each_image_under_a_new_revision(self):
        workflow = (Path(make_image.__file__).resolve().parents[3] / ".github/workflows/release-image.yml").read_text()
        self.assertIn("make-image.py --major ${{ matrix.android }} --arch ${{ matrix.arch }} --next-revision", workflow)
        self.assertIn("--revision ${{ steps.revision.outputs.revision }}", workflow)
        self.assertNotIn('gh release upload "$tag"', workflow)

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
TYPE_ATTRIBUTE = 0x02


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
        # The list room check's own problem; the missing Settings overlay is reported in
        # words of its own, which also name the window's bottom edge.
        self.assertIn("so the last row of its list sits on the window's bottom edge", outputs[0])
        self.assertNotIn("so the last row of its list sits on the window's bottom edge", outputs[1])


def string_pool(strings):
    """A string pool of UTF-8 strings shorter than 128 bytes, as aapt2 writes one."""
    data, offsets = b"", []
    for text in strings:
        offsets.append(len(data))
        encoded = text.encode("utf-8")
        data += bytes([len(text), len(encoded)]) + encoded + b"\0"
    data += b"\0" * (-len(data) % 4)
    start = 28 + 4 * len(strings)
    header = struct.pack("<HHIIIIII", 0x0001, 28, start + len(data), len(strings), 0, 0x100, start, 0)
    return header + b"".join(struct.pack("<I", offset) for offset in offsets) + data


def resource_table(resources, layout="full", public=()):
    """A compiled resource table of package 0x7f with `resources` ({type: [name]}), one spec
    chunk and one type chunk per type in the default configuration, with the resources
    `public` names ("type/name") public. `layout` is how aapt2 lays the entries out: "full"
    (32-bit offsets, with one empty entry), "compact" (16-bit offsets and compact entries,
    with one empty entry) or "sparse" (index and offset pairs)."""
    types = list(resources)
    keys = [name for names in resources.values() for name in names]
    type_pool, key_pool = string_pool(types), string_pool(keys)
    chunks = b""
    for type_id, type_name in enumerate(types, 1):
        spec = b"".join(struct.pack("<I", make_image.ARSC_SPEC_PUBLIC if f"{type_name}/{name}" in public else 0)
                        for name in resources[type_name])
        spec_body = struct.pack("<BBHI", type_id, 0, 0, len(resources[type_name])) + spec
        chunks += struct.pack("<HHI", make_image.ARSC_TYPE_SPEC, 16, 8 + len(spec_body)) + spec_body
        entries, offsets = b"", []
        for name in resources[type_name]:
            offsets.append(len(entries))
            if layout == "full":
                entries += struct.pack("<HHI", 8, 0, keys.index(name)) + struct.pack("<HBBI", 8, 0, 0x10, 0)
            else:
                entries += struct.pack("<HHI", keys.index(name), make_image.ARSC_ENTRY_COMPACT, 0)
        if layout == "sparse":
            flags = make_image.ARSC_TYPE_SPARSE
            array = b"".join(struct.pack("<HH", i, offset // 4) for i, offset in enumerate(offsets))
            count = len(offsets)
        elif layout == "compact":
            flags = make_image.ARSC_TYPE_OFFSET16
            array = b"".join(struct.pack("<H", offset // 4) for offset in offsets) + struct.pack("<H", 0xFFFF)
            count = len(offsets) + 1
        else:
            flags = 0
            array = b"".join(struct.pack("<I", offset) for offset in offsets) + struct.pack("<I", 0xFFFFFFFF)
            count = len(offsets) + 1
        array += b"\0" * (-len(array) % 4)
        config = struct.pack("<I", 64) + bytes(60)
        header_size = 8 + 12 + len(config)
        body = struct.pack("<BBHII", type_id, flags, 0, count, header_size + len(array)) + config + array + entries
        chunks += struct.pack("<HHI", 0x0201, header_size, 8 + len(body)) + body
    name = "com.example".encode("utf-16-le").ljust(256, b"\0")
    package_body = (struct.pack("<I", 0x7F) + name
                    + struct.pack("<IIIII", 288, len(types), 288 + len(type_pool), len(keys), 0)
                    + type_pool + key_pool + chunks)
    package = struct.pack("<HHI", 0x0200, 288, 8 + len(package_body)) + package_body
    globals_pool = string_pool(["res/x.xml"])
    return struct.pack("<HHII", 0x0002, 12, 12 + len(globals_pool) + len(package), 1) + globals_pool + package


def table_of(names, public=()):
    """A resource table that defines exactly `names` ("type/name"), with those `public`
    names public."""
    resources = {}
    for name in names:
        kind, entry = name.split("/", 1)
        resources.setdefault(kind, []).append(entry)
    return resource_table(resources, public=public)


# Every framework resource the system screen overlays name, as checked in the framework-res
# of the Android 16 (15581820) and Android 17 (16373615) images: public in both, or (named
# @*android:) there in both. A name the overlays start to use is checked the same way before
# it goes here; aapt2 refuses to link an overlay that names a private one as public, as
# colorSurface was.
FRAMEWORK_PUBLIC_CHECKED = {
    "attr/accessibilityHeading", "attr/animateLayoutChanges", "attr/autoMirrored", "attr/background",
    "attr/bottomLeftRadius", "attr/bottomRightRadius", "attr/buttonBarNegativeButtonStyle",
    "attr/buttonBarNeutralButtonStyle", "attr/buttonBarPositiveButtonStyle", "attr/buttonBarStyle",
    "attr/clickable", "attr/clipToOutline", "attr/clipToPadding", "attr/color", "attr/colorBackground",
    "attr/colorControlHighlight", "attr/elevation", "attr/ellipsize", "attr/end", "attr/fillColor",
    "attr/fillViewport", "attr/focusable", "attr/focusedByDefault", "attr/fontFamily", "attr/foreground",
    "attr/gravity", "attr/hasCode", "attr/height", "attr/id", "attr/importantForAccessibility",
    "attr/isStatic", "attr/keyboardNavigationCluster", "attr/layoutDirection", "attr/layout_height",
    "attr/layout_marginEnd", "attr/layout_marginStart", "attr/layout_marginTop", "attr/layout_weight",
    "attr/layout_width", "attr/listPreferredItemPaddingEnd", "attr/listPreferredItemPaddingStart",
    "attr/maxLines", "attr/maxWidth", "attr/minHeight", "attr/minSdkVersion", "attr/orientation",
    "attr/paddingBottom", "attr/paddingEnd", "attr/paddingStart", "attr/paddingTop", "attr/pathData",
    "attr/priority", "attr/radius", "attr/requiredSystemPropertyName", "attr/requiredSystemPropertyValue",
    "attr/resourcesMap", "attr/scrollIndicators",
    "attr/scrollbarAlwaysDrawVerticalTrack", "attr/selectableItemBackground", "attr/shape", "attr/src",
    "attr/start", "attr/strokeColor", "attr/strokeLineCap", "attr/strokeWidth", "attr/targetPackage",
    "attr/targetSdkVersion", "attr/text", "attr/textAppearance", "attr/textColor", "attr/textColorPrimary",
    "attr/textColorSecondary", "attr/textSize", "attr/textStyle", "attr/tint", "attr/topLeftRadius",
    "attr/topRightRadius", "attr/touchscreenBlocksFocus", "attr/viewportHeight", "attr/viewportWidth",
    "attr/visibility", "attr/width", "color/white", "id/button1", "id/button2", "id/button3", "id/message",
    "string/search_go", "style/TextAppearance.DeviceDefault.Headline",
}
FRAMEWORK_PRIVATE_CHECKED = {
    "color/system_on_surface_dark", "color/system_on_surface_light", "color/system_on_surface_variant_dark",
    "color/system_on_surface_variant_light", "color/system_surface_bright_dark",
    "color/system_surface_bright_light", "color/system_surface_container_highest_dark",
    "color/system_surface_container_highest_light", "dimen/toast_elevation", "dimen/toast_width",
    "drawable/toast_frame", "id/buttonPanel", "id/spacer", "string/global_action_settings",
    "style/TextAppearance.Material.DialogWindowTitle", "style/TextAppearance.Material.Widget.ActionBar.Title",
    "style/TextAppearance.Material.WindowTitle", "style/TextAppearance.Toast",
}


def overlay_framework_references(apk, home_padding=False):
    """What the sources of the system screen overlay file `apk` name of the framework (see
    make_image.framework_references)."""
    with tempfile.TemporaryDirectory() as tmp:
        make_image.system_overlay_sources(Path(tmp), apk, home_padding)
        return make_image.framework_references(tmp)


def linkable_framework_table(home_padding=False, overlaid=()):
    """A framework-res table every system screen overlay links against: everything they name
    there, public, and `overlaid` ("type/name")."""
    named = {name for apk in make_image.SYSTEM_OVERLAY_FILES
             for name, _ in overlay_framework_references(apk, home_padding).values()}
    return table_of(sorted(named | set(overlaid)), public=named)


def complex_dp(dp):
    """`dp` as a compiled dimension in dp, with 15 fraction bits, as aapt2 writes 35.4198dp."""
    return (round(dp * (1 << 15)) << 8) | (2 << 4) | make_image.ARSC_UNIT_DIP


def status_bar_table(heights, names=None):
    """A compiled resource table of dimens: each of `names` (the status bar heights) in every
    density of `heights` ({density: dp}, 0 for the values without a density), and in the
    values without a density the highest of them unless `heights` gives one."""
    names = names or make_image.STATUS_BAR_DIMENS
    heights = dict(heights)
    heights.setdefault(0, max(heights.values()))
    type_pool, key_pool = string_pool(["dimen"]), string_pool(names)
    spec_body = struct.pack("<BBHI", 1, 0, 0, len(names)) + bytes(4 * len(names))
    chunks = struct.pack("<HHI", make_image.ARSC_TYPE_SPEC, 16, 8 + len(spec_body)) + spec_body
    for density, dp in sorted(heights.items()):
        entries = b"".join(struct.pack("<HHI", 8, 0, i) + struct.pack("<HBBI", 8, 0, 0x05, complex_dp(dp))
                           for i in range(len(names)))
        array = b"".join(struct.pack("<I", 16 * i) for i in range(len(names)))
        config = bytearray(struct.pack("<I", 64) + bytes(60))
        struct.pack_into("<H", config, make_image.ARSC_CONFIG_DENSITY, density)
        header_size = 8 + 12 + len(config)
        body = struct.pack("<BBHII", 1, 0, 0, len(names), header_size + len(array)) + bytes(config) + array + entries
        chunks += struct.pack("<HHI", make_image.ARSC_TYPE, header_size, 8 + len(body)) + body
    name = "io.github.thisisthepy.dsa.overlay.framework".encode("utf-16-le").ljust(256, b"\0")
    package_body = (struct.pack("<I", 0x7F) + name
                    + struct.pack("<IIIII", 288, 1, 288 + len(type_pool), len(names), 0)
                    + type_pool + key_pool + chunks)
    package = struct.pack("<HHI", 0x0200, 288, 8 + len(package_body)) + package_body
    globals_pool = string_pool(["x"])
    return struct.pack("<HHII", 0x0002, 12, 12 + len(globals_pool) + len(package), 1) + globals_pool + package


def apk_with_table(table, files=None):
    """An APK holding the resource table `table` and `files`."""
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w") as archive:
        archive.writestr("resources.arsc", table)
        for name, data in (files or {}).items():
            archive.writestr(name, data)
    return out.getvalue()


APP_EXPANDED = 0x7F040249
APP_BAR = "com.google.android.material.appbar.AppBarLayout"
SETTINGS_TITLE_LAYOUT = "res/layout-v36/settingslib_expressive_collapsing_toolbar_content_layout.xml"
NS = "{http://schemas.android.com/apk/res/android}"
APP_NS = "{http://schemas.android.com/apk/res-auto}"


class SystemScreenDesignTests(unittest.TestCase):
    """The design of the screens Android draws itself: overlays built against each image's
    own framework-res and app, a large title that folds, rounded cards, bold titles, a blue
    accent in both themes, and rounded dialogs with their buttons side by side."""

    @staticmethod
    def sources(write):
        """The files an overlay's sources writer puts in a folder, by path."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write(root)
            return {path.relative_to(root).as_posix(): path.read_text()
                    for path in root.rglob("*") if path.is_file()}

    def every_overlay(self, home_padding=False):
        return {
            "DsaFrameworkOverlay.apk": self.sources(make_image.framework_overlay_sources),
            "DsaSettingsOverlay.apk": self.sources(
                lambda root: make_image.settings_overlay_sources(root, home_padding=home_padding)),
            **{apk: self.sources(lambda root, apk=apk: make_image.design_overlay_sources(root, apk))
               for apk in make_image.DESIGN_OVERLAYS},
        }

    def test_fr34_a_resource_table_is_read_in_every_entry_layout_aapt2_writes(self):
        resources = {"dimen": ["settingslib_toolbar_layout_height", "settingslib_preference_corner_radius"],
                     "style": ["SettingsLibCollapsingToolbarTitle.Expanded"], "id": ["app_bar"]}
        wanted = {"dimen/settingslib_toolbar_layout_height", "dimen/settingslib_preference_corner_radius",
                  "style/SettingsLibCollapsingToolbarTitle.Expanded", "id/app_bar"}
        for layout in ("full", "compact", "sparse"):
            self.assertEqual(make_image.arsc_resource_names(resource_table(resources, layout)), wanted, layout)
        with self.assertRaises(ValueError):
            make_image.arsc_resource_names(b"\x03\x00\x08\x00\x08\x00\x00\x00")

    def test_fr34_a_resource_table_says_which_resources_are_public(self):
        # framework-res keeps its private attributes in a type of their own, so the public
        # ones keep their ids; a reference names them attr/ all the same.
        resources = {"attr": ["colorBackground"], "^attr-private": ["colorSurface"],
                     "color": ["white", "system_surface_bright_light"]}
        public = {"attr/colorBackground", "color/white"}
        wanted = {"attr/colorBackground": True, "attr/colorSurface": False, "color/white": True,
                  "color/system_surface_bright_light": False}
        for layout in ("full", "compact", "sparse"):
            self.assertEqual(make_image.arsc_resources(resource_table(resources, layout, public)), wanted, layout)

    def test_fr34_an_overlay_may_name_only_what_framework_res_has_and_shows_it(self):
        framework = table_of(
            ["attr/colorBackground", "attr/colorSurface", "attr/color", "attr/hiddenAttribute",
             "color/hidden", "color/white"],
            public={"attr/colorBackground", "attr/color", "color/white"})
        with tempfile.TemporaryDirectory() as tmp:
            res = Path(tmp) / "res/drawable"
            res.mkdir(parents=True)
            (res / "a.xml").write_text(
                f'<shape {make_image.ANDROID_XMLNS} android:hiddenAttribute="1">\n'
                '    <solid android:color="?android:attr/colorSurface" />\n'
                '    <stroke android:color="@android:color/hidden" />\n'
                '    <solid android:color="@*android:dimen/gone" />\n'
                '    <fine android:color="?android:attr/colorBackground" />\n'
                '    <fine android:color="@*android:color/hidden" />\n'
                '    <fine android:color="@android:color/white" />\n'
                '</shape>\n')
            (Path(tmp) / "res/values").mkdir()
            (Path(tmp) / "res/values/styles.xml").write_text(
                '<resources><style name="A"><item name="android:hiddenStyleItem">1</item></style></resources>\n')
            problems = make_image.framework_reference_problems(tmp, framework)
        self.assertEqual(len(problems), 5, problems)
        for text in ("android:hiddenAttribute", "?android:attr/colorSurface", "@android:color/hidden",
                     "@*android:dimen/gone", "android:hiddenStyleItem"):
            self.assertTrue(any(problem.startswith(text + " ") for problem in problems), (text, problems))

    def test_fr34_the_overlays_name_only_framework_resources_checked_in_android_16_and_17(self):
        # The image's build links each overlay against its framework-res and stops at the first
        # resource that is missing or private; this catches one before an image is made.
        for home_padding in (False, True):
            for apk in make_image.SYSTEM_OVERLAY_FILES:
                for text, (name, needs_public) in overlay_framework_references(apk, home_padding).items():
                    checked = FRAMEWORK_PUBLIC_CHECKED | (set() if needs_public else FRAMEWORK_PRIVATE_CHECKED)
                    self.assertIn(name, checked,
                                  f"{apk} names {text}, which no one checked is "
                                  f"{'public' if needs_public else 'there'} in the framework-res of "
                                  "Android 16 and 17")

    def test_fr34_every_overlay_s_framework_references_are_checked_before_any_is_linked(self):
        tables = {apk: table_of(make_image.overlaid_resources(apk)) for apk in make_image.SYSTEM_OVERLAY_FILES}
        named = {name for apk in make_image.SYSTEM_OVERLAY_FILES
                 for name, _ in overlay_framework_references(apk).values()}
        overlaid = make_image.overlaid_resources("DsaFrameworkOverlay.apk")
        # The cards' highlight is gone and the primary text colour is private.
        tables["DsaFrameworkOverlay.apk"] = table_of(
            sorted((named | set(overlaid)) - {"attr/colorControlHighlight"}),
            public=named - {"attr/textColorPrimary"})
        with tempfile.TemporaryDirectory() as tmp, patch.object(make_image, "run") as run:
            with self.assertRaises(ValueError) as raised:
                make_image.build_system_overlays(Path("/sdk/aapt2"), Path(tmp) / "framework-res.apk",
                                                 Path(tmp), tables)
            run.assert_not_called()
        message = str(raised.exception)
        self.assertIn("DsaSettingsOverlay.apk: ?android:attr/colorControlHighlight names "
                      "attr/colorControlHighlight, which this image's framework-res does not have", message)
        self.assertIn("DsaPackageInstallerOverlay.apk: ?android:attr/textColorPrimary", message)
        self.assertIn("DsaSettingsOverlay.apk: ?android:attr/textColorPrimary", message)
        self.assertIn("keeps private", message)

    def test_fr34_every_target_package_has_an_overlay(self):
        targets = {overlay["target"] for overlay in make_image.SYSTEM_OVERLAYS}
        self.assertEqual(targets, {"android", "com.android.settings", "com.android.documentsui",
                                   "com.android.permissioncontroller", "com.android.packageinstaller",
                                   "com.android.intentresolver"})
        for apk, files in self.every_overlay().items():
            overlay = make_image.system_overlay(apk)
            manifest = ET.fromstring(files["AndroidManifest.xml"])
            self.assertEqual(manifest.get("package"), overlay["package"])
            element = manifest.find("overlay")
            self.assertEqual(element.get(NS + "targetPackage"), overlay["target"])
            # Static: enabled at every boot, and no app or user can turn it off.
            self.assertEqual(element.get(NS + "isStatic"), "true")
            self.assertEqual(manifest.find("application").get(NS + "hasCode"), "false")
            if apk != "DsaFrameworkOverlay.apk":
                # Only a resource map makes the framework rewrite the overlay's references to
                # the app's ids and attributes into the app's own.
                resources_map = element.get(NS + "resourcesMap").removeprefix("@xml/")
                mapping = ET.fromstring(files[f"res/xml/{resources_map}.xml"])
                targets = {item.get("target") for item in mapping.findall("item")}
                self.assertEqual(targets, set(make_image.overlaid_resources(apk)), apk)

    def test_fr34_the_overlays_are_built_against_the_images_framework_res_and_each_app(self):
        home_padding = True
        tables = {apk: table_of(make_image.overlaid_resources(apk, home_padding))
                  for apk in make_image.SYSTEM_OVERLAY_FILES}
        tables["DsaFrameworkOverlay.apk"] = linkable_framework_table(
            home_padding, make_image.overlaid_resources("DsaFrameworkOverlay.apk"))
        with tempfile.TemporaryDirectory() as tmp, patch.object(make_image, "run") as run:
            root = Path(tmp)
            framework, settings, others = make_image.build_system_overlays(
                Path("/sdk/aapt2"), root / "framework-res.apk", root, tables, home_padding)
            self.assertEqual(set(others), set(make_image.DESIGN_OVERLAYS))
            links = [call.args for call in run.call_args_list if call.args[1] == "link"]
            self.assertEqual(len(links), len(make_image.SYSTEM_OVERLAYS))
            for link in links:
                self.assertIn(root / "framework-res.apk", link)
                # The record goes in as an asset.
                self.assertIn("-A", link)
            for apk_path in [framework, settings, *others.values()]:
                apk = Path(apk_path).name
                record = json.loads((Path(apk_path).parent / make_image.SYSTEM_OVERLAY_RECORD).read_text())
                self.assertEqual(record["target"], make_image.system_overlay(apk)["target"])
                self.assertEqual(record["framework_res"],
                                 hashlib.sha256(tables["DsaFrameworkOverlay.apk"]).hexdigest())
                self.assertEqual(record["target_resources"], hashlib.sha256(tables[apk]).hexdigest())

    def test_fr34_an_overlay_written_for_another_build_of_its_app_stops_the_image(self):
        tables = {apk: table_of(make_image.overlaid_resources(apk)) for apk in make_image.SYSTEM_OVERLAY_FILES}
        tables["DsaFrameworkOverlay.apk"] = linkable_framework_table(
            overlaid=make_image.overlaid_resources("DsaFrameworkOverlay.apk"))
        tables["DsaSettingsOverlay.apk"] = table_of(
            [name for name in make_image.overlaid_resources("DsaSettingsOverlay.apk")
             if name != "style/SettingsLibTheme.CollapsingToolbar.Expressive"])
        with tempfile.TemporaryDirectory() as tmp, patch.object(make_image, "run"):
            with self.assertRaisesRegex(ValueError, "SettingsLibTheme.CollapsingToolbar.Expressive"):
                make_image.build_system_overlays(Path("/sdk/aapt2"), Path(tmp) / "framework-res.apk",
                                                 Path(tmp), tables)

    def image(self, framework_table=b"", break_settings=False, without=None):
        """The names and files of a target_files with every overlay, built against
        `framework_table` (the image's own when empty), its framework-res, and its apps."""
        own = table_of(["dimen/config_dialogCornerRadius"])
        files = {make_image.FRAMEWORK_RES: apk_with_table(own)}
        for overlay in make_image.SYSTEM_OVERLAYS:
            app = table_of([f"id/{overlay['apk']}"])
            if overlay["target_apk"] and overlay["target"] != "android":
                files["SYSTEM" + overlay["target_apk"]] = apk_with_table(app)
            if break_settings and overlay["target"] == "com.android.settings":
                app = table_of(["id/another_build"])
            record = make_image.overlay_record(overlay["target"], framework_table or own,
                                               own if overlay["target"] == "android" else app)
            if overlay["apk"] != without:
                table = status_bar_table(make_image.status_bar_heights()) if overlay["target"] == "android" \
                    else b"overlay"
                files[f"VENDOR/overlay/{overlay['apk']}"] = apk_with_table(
                    table, {make_image.SYSTEM_OVERLAY_RECORD: json.dumps(record)})
        return list(files), files.__getitem__

    def test_fr34_check_image_accepts_overlays_built_against_its_framework_res_and_apps(self):
        self.assertEqual(make_image.system_overlay_problems(*self.image()), [])

    def test_fr34_check_image_rejects_an_image_missing_an_overlay(self):
        for apk in make_image.SYSTEM_OVERLAY_FILES:
            problems = make_image.system_overlay_problems(*self.image(without=apk))
            self.assertEqual(len(problems), 1, apk)
            self.assertIn(f"VENDOR/overlay/{apk} is missing", problems[0])

    def test_fr34_check_image_rejects_an_overlay_built_against_another_framework_res(self):
        problems = make_image.system_overlay_problems(*self.image(framework_table=table_of(["id/other"])))
        self.assertEqual(len(problems), len(make_image.SYSTEM_OVERLAYS))
        self.assertTrue(all("built against another framework-res" in problem for problem in problems))

    def test_fr34_check_image_rejects_an_overlay_built_for_another_build_of_its_app(self):
        problems = make_image.system_overlay_problems(*self.image(break_settings=True))
        self.assertEqual(len(problems), 1)
        self.assertIn("DsaSettingsOverlay.apk was built for another build of com.android.settings", problems[0])

    def test_fr22_check_image_rejects_a_framework_overlay_whose_status_bar_is_not_the_title_bar(self):
        names, read = self.image()
        files = {name: read(name) for name in names}
        path = make_image.FRAMEWORK_OVERLAY_APK
        with zipfile.ZipFile(io.BytesIO(files[path])) as apk:
            record = apk.read(make_image.SYSTEM_OVERLAY_RECORD)
        phone = {density: 24 for density in make_image.status_bar_heights()}
        files[path] = apk_with_table(status_bar_table(phone), {make_image.SYSTEM_OVERLAY_RECORD: record})
        problems = make_image.system_overlay_problems(list(files), files.__getitem__)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("does not line up with the window's title bar", problems[0])
        self.assertIn("status_bar_height is 24 (unit 1) at 262 dpi, not the title bar's 58 px there", problems[0])
        files[path] = apk_with_table(b"overlay", {make_image.SYSTEM_OVERLAY_RECORD: record})
        problems = make_image.system_overlay_problems(list(files), files.__getitem__)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("cannot be read", problems[0])

    def test_fr34_check_image_rejects_an_overlay_that_does_not_say_what_it_was_built_against(self):
        names, read = self.image()
        files = {name: read(name) for name in names}
        files["VENDOR/overlay/DsaIntentResolverOverlay.apk"] = apk_with_table(b"overlay")
        problems = make_image.system_overlay_problems(list(files), files.__getitem__)
        self.assertEqual(len(problems), 1)
        self.assertIn("DsaIntentResolverOverlay.apk does not say which framework-res", problems[0])

    def test_fr34_check_image_runs_the_overlay_check(self):
        check_script = Path(__file__).with_name("check-image.py")
        outputs = []
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            img_dir = root / "image"
            img_dir.mkdir()
            keys_dir = root / "keys"
            keys_dir.mkdir()
            (img_dir / "image.json").write_text(json.dumps({"files": {}, "kernel_cmdline": ""}))
            (img_dir / "initrd.img").write_bytes(gzip.compress(b""))
            for framework_table in (b"", table_of(["id/other"])):
                names, read = self.image(framework_table=framework_table)
                with zipfile.ZipFile(img_dir / "signed-target_files.zip", "w") as archive:
                    archive.writestr("META/apkcerts.txt", "")
                    archive.writestr("SYSTEM/build.prop", "ro.debuggable=0\n")
                    for name in names:
                        archive.writestr(name, read(name))
                outputs.append(subprocess.run([sys.executable, str(check_script), str(img_dir), str(keys_dir)],
                                              capture_output=True, text=True).stdout)
        self.assertNotIn("built against another framework-res", outputs[0])
        for apk in make_image.SYSTEM_OVERLAY_FILES:
            self.assertNotIn(f"{apk} is missing", outputs[0])
            self.assertIn(f"VENDOR/overlay/{apk} was built against another framework-res", outputs[1])

    def test_fr34_the_image_carries_every_overlay_with_the_platform_key(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            overlays = {}
            for apk in make_image.DESIGN_OVERLAYS:
                (root / apk).write_bytes(apk.encode())
                overlays[apk] = root / apk
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("META/apkcerts.txt", 'name="DsaPackageInstallerOverlay.apk" certificate="x"\n')
            make_image.modify(source, root / "modified.zip", overlays=overlays)
            with zipfile.ZipFile(root / "modified.zip") as archive:
                certs = archive.read("META/apkcerts.txt").decode()
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode()
                for apk in make_image.DESIGN_OVERLAYS:
                    self.assertEqual(archive.read(f"VENDOR/overlay/{apk}"), apk.encode())
                    self.assertEqual(certs.count(f'name="{apk}"'), 1)
                    self.assertIn(f'name="{apk}" certificate="build/make/target/product/security/platform.x509.pem"',
                                  certs)
                    self.assertIn(f"vendor/overlay/{apk} 0 0 644 capabilities=0x0", fs_config)
        self.assertEqual(fs_config.count("vendor/overlay 0 0 755"), 1)

    def test_fr34_settings_home_opens_with_a_large_title_in_a_tall_window_and_folded_below_600_dp(self):
        files = self.every_overlay()["DsaSettingsOverlay.apk"]
        tall = ET.fromstring(files["res/layout-h600dp/settings_homepage_container.xml"])
        short = ET.fromstring(files["res/layout/settings_homepage_container.xml"])
        self.assertEqual(make_image.DESIGN_TALL_WINDOW_DP, 600)

        def app_bar(layout):
            return layout.find("com.google.android.material.appbar.AppBarLayout")

        title = app_bar(tall)[0]
        self.assertEqual(title.tag, "TextView")
        self.assertEqual(title.get(NS + "text"), "@*android:string/global_action_settings")
        self.assertEqual(title.get(NS + "textStyle"), "bold")
        # It scrolls away with the list and leaves the search bar as the toolbar.
        self.assertEqual(title.get(APP_NS + "layout_scrollFlags"), "scroll|snap")
        self.assertGreaterEqual(int(title.get(NS + "layout_height").removesuffix("dp")), 150)
        self.assertEqual(app_bar(tall)[1].get(NS + "id"), "@+id/app_bar_container")
        # Below 600 dp the app bar holds the search bar alone: folded from the start.
        self.assertEqual([child.get(NS + "id") for child in app_bar(short)], ["@+id/app_bar_container"])
        # Every view the home page's code finds keeps Settings' id, in both layouts.
        for layout in (tall, short):
            ids = {element.get(NS + "id") for element in layout.iter()}
            for name in make_image.SETTINGS_HOME_IDS:
                self.assertIn(f"@+id/{name}", ids)
            self.assertEqual(layout.find("androidx.core.widget.NestedScrollView").get(APP_NS + "layout_behavior"),
                             "com.android.settings.widget.HomepageAppBarScrollingViewBehavior")

    def test_fr34_settings_pages_open_with_a_large_title_in_a_tall_window_and_folded_below_600_dp(self):
        files = self.every_overlay()["DsaSettingsOverlay.apk"]

        def theme(folder):
            style = ET.fromstring(files[f"res/{folder}/dsa_title.xml"]).find("style")
            items = {item.get("name"): item.text for item in style.findall("item")}
            return style.get("name"), style.get("parent"), items

        # The app bar's theme says whether it opens expanded: folded below 600 dp, expanded
        # from 600 dp up, over Material's theme as SettingsLib's own style is.
        self.assertEqual(theme("values-v36"),
                         ("DsaAppBarTheme", "@style/Theme.MaterialComponents.DayNight", {"expanded": "false"}))
        self.assertEqual(theme("values-h600dp-v36"),
                         ("DsaAppBarTheme", "@style/Theme.MaterialComponents.DayNight", {"expanded": "true"}))
        # Material's theme is named through a stand-in that never matches (watches only), so
        # the parent resolves to Settings' own.
        stand_in = ET.fromstring(files["res/values-watch/dsa_stand_ins.xml"]).find("style")
        self.assertEqual((stand_in.get("name"), stand_in.get("parent")), ("Theme.MaterialComponents.DayNight", ""))
        mapping = make_image.settings_overlay_map()
        self.assertEqual(mapping["style/SettingsLibTheme.CollapsingToolbar.Expressive"], "style/DsaAppBarTheme")
        self.assertEqual(mapping["style/Theme.MaterialComponents.DayNight"], "style/Theme.MaterialComponents.DayNight")
        self.assertEqual(mapping["attr/expanded"], "attr/expanded")
        # The app bar reads it from the theme, where SettingsLib's layout says false.
        layout = compiled_layout((APP_BAR, [("id", ANDROID_ID, TYPE_REFERENCE, 0x7F0A00CB),
                                            ("expanded", APP_EXPANDED, TYPE_BOOLEAN, 0)], []))
        apk = ListBottomMarginTests.apk({SETTINGS_TITLE_LAYOUT: layout})
        edited = make_image.edit_settings_apk(apk, "Settings.apk")
        with zipfile.ZipFile(io.BytesIO(edited)) as archive:
            found = layout_attributes(archive.read(SETTINGS_TITLE_LAYOUT))
        self.assertEqual(found[0][2], [("id", TYPE_REFERENCE, 0x7F0A00CB), ("expanded", TYPE_ATTRIBUTE, APP_EXPANDED)])
        self.assertEqual(make_image.apk_edits_left(edited, theme_attributes=make_image.SETTINGS_TITLE_EXPANDED), [])
        self.assertNotEqual(make_image.apk_edits_left(apk, theme_attributes=make_image.SETTINGS_TITLE_EXPANDED), [])
        # Both of SettingsLib's toolbar layouts, the one for AppCompat activities too.
        self.assertEqual(set(make_image.SETTINGS_TITLE_EXPANDED), {
            SETTINGS_TITLE_LAYOUT, "res/layout-v36/settingslib_expressive_support_toolbar_layout.xml"})

    def test_fr34_settings_lists_are_grouped_in_rounded_cards(self):
        files = self.every_overlay()["DsaSettingsOverlay.apk"]
        corners = {"settingslib_round_background": (26, 26), "settingslib_round_background_top": (26, 0),
                   "settingslib_round_background_center": (0, 0), "settingslib_round_background_bottom": (0, 26)}
        for folder, theme in [("drawable-v36", "light"), ("drawable-night-v36", "dark")]:
            for name, (top, bottom) in corners.items():
                ripple = ET.fromstring(files[f"res/{folder}/{name}.xml"])
                # SettingsLib's padding, which its adapter gives the row.
                self.assertEqual(ripple.get(NS + "paddingStart"), "32dp")
                item = ripple.find("item")
                self.assertEqual(item.get(NS + "start"), "?android:attr/listPreferredItemPaddingStart")
                shape = item.find("shape")
                self.assertIn(f"system_surface_bright_{theme}", shape.find("solid").get(NS + "color"))
                radii = shape.find("corners")
                self.assertEqual([radii.get(NS + f"{side}Radius") for side in
                                  ("topLeft", "topRight", "bottomLeft", "bottomRight")],
                                 [f"{top}dp", f"{top}dp", f"{bottom}dp", f"{bottom}dp"], name)
        values = ET.fromstring(files["res/values-v36/dsa_design.xml"])
        self.assertEqual({d.get("name"): d.text for d in values.findall("dimen")},
                         {"settingslib_preference_corner_radius": "26dp"})
        # The page and the cards differ in both themes, so the cards show.
        colors = make_image.DESIGN_COLORS
        for theme in ("light", "dark"):
            self.assertNotEqual(colors[f"system_surface_container_{theme}"], colors[f"system_surface_bright_{theme}"])

    def test_fr34_permission_controller_lists_get_the_same_cards(self):
        def corners(attrs):
            return compiled_layout(("ripple", [], [("item", [], [("shape", [], [("corners", attrs, [])])])]))

        top = corners([("topLeftRadius", 0x010101A9, TYPE_REFERENCE, 0x7F0705F5),
                       ("topRightRadius", 0x010101AA, TYPE_REFERENCE, 0x7F0705F5),
                       ("bottomLeftRadius", 0x010101AB, TYPE_DIMENSION, dp(4)),
                       ("bottomRightRadius", 0x010101AC, TYPE_DIMENSION, dp(4))])
        center = corners([("radius", 0x010101A8, TYPE_DIMENSION, dp(4))])
        apk = ListBottomMarginTests.apk({"res/drawable-v36/settingslib_round_background_top.xml": top,
                                         "res/drawable-v36/settingslib_round_background_center.xml": center})
        edited = make_image.edit_permission_controller_apk(apk, "PermissionController.apk")
        with zipfile.ZipFile(io.BytesIO(edited)) as archive:
            found_top = layout_attributes(archive.read("res/drawable-v36/settingslib_round_background_top.xml"))
            found_center = layout_attributes(archive.read("res/drawable-v36/settingslib_round_background_center.xml"))
        self.assertEqual(found_top[3][2], [("topLeftRadius", TYPE_DIMENSION, dp(26)),
                                           ("topRightRadius", TYPE_DIMENSION, dp(26)),
                                           ("bottomLeftRadius", TYPE_DIMENSION, dp(0)),
                                           ("bottomRightRadius", TYPE_DIMENSION, dp(0))])
        self.assertEqual(found_center[3][2], [("radius", TYPE_DIMENSION, dp(0))])
        self.assertEqual(make_image.apk_edits_left(edited, dimensions=make_image.permission_controller_edits()), [])

    def test_fr34_the_accent_is_blue_in_the_light_and_the_dark_theme(self):
        colors = make_image.DESIGN_COLORS

        def rgb(name):
            value = colors[name].lstrip("#")
            return [int(value[i:i + 2], 16) for i in (0, 2, 4)]

        for theme in ("light", "dark"):
            for role in ("primary", "primary_container", "inverse_primary", "surface_tint"):
                red, green, blue = rgb(f"system_{role}_{theme}")
                self.assertGreater(blue, red, role)
                self.assertGreater(blue, green, role)
            for tone in range(200, 900, 100):
                red, green, blue = rgb(f"system_accent1_{tone}_{theme}")
                self.assertGreater(blue, max(red, green), tone)
        # Every colour role the design sets for one theme it sets for the other.
        for name in colors:
            if name.endswith("_light"):
                self.assertIn(name.removesuffix("_light") + "_dark", colors)
        files = self.every_overlay()["DsaFrameworkOverlay.apk"]
        values = ET.fromstring(files["res/values/dsa_design.xml"])
        self.assertEqual({c.get("name"): c.text for c in values.findall("color")}, colors)

    def test_fr34_titles_are_bold(self):
        overlays = self.every_overlay()
        framework = ET.fromstring(overlays["DsaFrameworkOverlay.apk"]["res/values/dsa_design.xml"])
        styles = {s.get("name"): s for s in framework.findall("style")}
        self.assertEqual(set(styles), set(make_image.DESIGN_FRAMEWORK_TITLES))
        settings = ET.fromstring(overlays["DsaSettingsOverlay.apk"]["res/values-v36/dsa_design.xml"])
        installer = ET.fromstring(overlays["DsaPackageInstallerOverlay.apk"]["res/values/dsa.xml"])
        permission = [ET.fromstring(overlays["DsaPermissionControllerOverlay.apk"][f"res/{folder}/dsa.xml"])
                      for folder in ("values-v36", "values-night-v36")]
        every = [*styles.values(), *settings.findall("style"), *installer.findall("style"),
                 *(style for values in permission for style in values.findall("style"))]
        self.assertGreaterEqual(len(every), 9)
        for style in every:
            items = {item.get("name"): item.text for item in style.findall("item")}
            self.assertEqual(items["android:textStyle"], "bold", style.get("name"))
            self.assertEqual(items["android:fontFamily"], "sans-serif", style.get("name"))
        # Each framework title keeps its own parent's other items.
        for name, parent in make_image.DESIGN_FRAMEWORK_TITLES.items():
            self.assertEqual(styles[name].get("parent"), f"@*android:style/{parent}")

    def test_fr34_dialogs_have_rounded_corners_and_their_buttons_side_by_side(self):
        files = self.every_overlay()["DsaFrameworkOverlay.apk"]
        values = ET.fromstring(files["res/values/dsa_design.xml"])
        dimens = {d.get("name"): d.text for d in values.findall("dimen")}
        self.assertEqual(dimens["config_dialogCornerRadius"], "26dp")
        self.assertEqual(dimens["config_bottomDialogCornerRadius"], "26dp")
        panel = ET.fromstring(files["res/layout/alert_dialog_button_bar_material.xml"])
        self.assertEqual(panel.get(NS + "id"), "@*android:id/buttonPanel")
        bar = panel.find("com.android.internal.widget.ButtonBarLayout")
        self.assertEqual(bar.get(NS + "orientation"), "horizontal")
        buttons = bar.findall("Button")
        # The framework's own ids, which AlertController finds the buttons by, each sharing the row.
        self.assertEqual([b.get(NS + "id") for b in buttons],
                         ["@android:id/button3", "@android:id/button2", "@android:id/button1"])
        self.assertTrue(all(b.get(NS + "layout_weight") == "1" for b in buttons))
        self.assertEqual(bar.find("Space").get(NS + "id"), "@*android:id/spacer")
        toast = ET.fromstring(files["res/layout/transient_notification.xml"])
        self.assertEqual(toast.get(NS + "background"), "@*android:drawable/toast_frame")
        frame = ET.fromstring(files["res/drawable/toast_frame.xml"])
        self.assertEqual(frame.find("corners").get(NS + "radius"), "26dp")

    def test_fr34_the_overlays_use_no_samsung_name_or_artwork(self):
        for apk, files in self.every_overlay(home_padding=True).items():
            for path, text in files.items():
                for word in ("one ui", "oneui", "samsung", "sec-roboto", "samsungone"):
                    self.assertNotIn(word, text.lower(), f"{apk}:{path}")
        # The font is the image's own sans-serif (Roboto), and the only drawings are DSA's.
        self.assertEqual(make_image.DESIGN_FONT, "sans-serif")


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
                # A removed app first, as the published build has them among the rest: the
                # members after it lie elsewhere in the new archive than in the published one,
                # so a member read from the published archive after it is copied is read at
                # its new place, which is no member's start there.
                archive.writestr("SYSTEM/app/Traceur/Traceur.apk", b"removed" * 64)
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

    @staticmethod
    def _allowed(policy):
        """The (source, target, class, permission) quadruples the policy's allow rules grant."""
        granted = set()
        for source, target, kind, perms in re.findall(
                r"\(allow\s+(\S+)\s+(\S+)\s+\((\S+)\s+\(([^()]*)\)\)\)", policy):
            granted.update((source, target, kind, perm) for perm in perms.split())
        return granted

    @staticmethod
    def _attributes(policy):
        """The types each typeattributeset of the policy puts in an attribute."""
        members = {}
        for attribute, types in re.findall(r"\(typeattributeset\s+(\S+)\s+\(([^()]*)\)\)", policy):
            members.setdefault(attribute, set()).update(types.split())
        return members

    @staticmethod
    def _erofs_mounts(rc):
        """Each erofs mount of an init script: its mount point, the type its fscontext= gives the
        superblock (None without one), and the type its context= gives the files."""
        mounts = []
        for line in rc.splitlines():
            line = line.strip()
            if not line.startswith("mount erofs "):
                continue
            point = line.split()[3]
            fs = re.search(r"\bfscontext=u:object_r:([a-z0-9_]+):s0", line)
            files = re.search(r"(?<![a-z])context=u:object_r:([a-z0-9_]+):s0", line)
            mounts.append((point, fs.group(1) if fs else None, files.group(1) if files else None))
        return mounts

    # The two files of 0.1.9 and PR #487 (context= alone, so the superblock carried the files'
    # type), which mounted the translator on develop d69b9ff2 and then rebooted Android at
    # 6.4 s: "avc: denied { getattr } for comm="netbpfload" name="/" dev="loop76"
    # scontext=u:r:bpfloader:s0 tcontext=u:object_r:system_lib_file:s0 tclass=filesystem".
    OLD_TRANSLATION_RC = ("on post-fs-data && property:ro.boot.dsa.arm_translation=default\n"
                          "    mount erofs loop@/system/etc/dsa/arm-translation/default.img /mnt/dsa-arm-translation "
                          "ro nodev nosuid context=u:object_r:system_lib_file:s0\n")
    OLD_TRANSLATION_POLICY = ("(allow init labeledfs (filesystem (relabelfrom)))\n"
                              "(allow init system_lib_file (filesystem (mount relabelfrom relabelto)))\n"
                              "(allow system_lib_file system_lib_file (filesystem (associate)))\n"
                              "(allow init system_lib_file (filesystem (getattr)))\n")

    def _context_mount_problems(self, rc_name, policy, rc=None):
        """What stops each erofs mount of the init script `rc_name` from mounting, or the
        overlays laid over it from being read. The mount gives the superblock a filesystem type
        of its own (fscontext=) and the files a file type (context=), and the kernel checks: for
        the superblock, relabelfrom erofs's own type (labeledfs) and relabelto the new one; for
        the files, the root first, relabelfrom the superblock's type and the files' type's
        associate on it; then the mount itself against the superblock's type. A rule missing
        from that chain fails the mount with "Permission denied" at boot. And statfs on an
        overlay is checked against the superblock of its top lower layer, so the superblock's
        type must be an fs_type, which every domain may getattr, and a contextmount_type, the
        only fs_types the platform lets anything relabel to."""
        if rc is None:
            rc = (Path(make_image.__file__).with_name(rc_name)).read_text()
        mounts = self._erofs_mounts(rc)
        self.assertTrue(mounts, rc_name)
        granted = self._allowed(policy)
        attributes = self._attributes(policy)
        problems = []
        for point, fs, files in mounts:
            if files is None:
                problems.append(f"{point}: no context=, so the files keep no system type")
                continue
            if fs is None:
                problems.append(f"{point}: no fscontext=, so the superblock carries {files}, a file type, and "
                                "every domain but init is refused statfs through the overlays")
                continue
            for attribute in ("fs_type", "contextmount_type"):
                if fs not in attributes.get(attribute, set()):
                    problems.append(f"{point}: {fs} is not a {attribute}")
            for rule in [("init", "labeledfs", "filesystem", "relabelfrom"),
                         ("init", fs, "filesystem", "relabelto"),
                         ("init", fs, "filesystem", "relabelfrom"),
                         (files, fs, "filesystem", "associate"),
                         ("init", fs, "filesystem", "mount")]:
                if rule not in granted:
                    problems.append(f"{point}: no (allow {rule[0]} {rule[1]} ({rule[2]} ({rule[3]})))")
        return problems

    def test_fr25_init_may_mount_the_translation_images_labelled_as_system_libraries(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        self.assertEqual(self._context_mount_problems("dsa-arm-translation.rc", policy), [])
        for point, fs, files in self._erofs_mounts(
                (Path(make_image.__file__).with_name("dsa-arm-translation.rc")).read_text()):
            self.assertEqual((fs, files), ("dsa_arm_translation_fs", "system_lib_file"), point)
        # What 0.1.9 had, which the kernel refused at every boot ("avc: denied { relabelfrom }
        # ... tcontext=u:object_r:system_lib_file:s0 tclass=filesystem").
        old = ("(allow init labeledfs (filesystem (mount relabelfrom)))\n"
               "(allow init system_lib_file (filesystem (relabelto associate)))\n"
               "(allow system_lib_file labeledfs (filesystem (associate)))\n")
        self.assertNotEqual(self._context_mount_problems("dsa-arm-translation.rc", old), [])

    def test_fr25_the_translation_image_does_not_reboot_android_once_laid_over(self):
        # develop d69b9ff2 mounted the image and laid it over, and every domain that statfs'd
        # /system/bin was then refused (bpfloader first, whose failure reboots Android).
        problems = self._context_mount_problems("dsa-arm-translation.rc", self.OLD_TRANSLATION_POLICY,
                                                rc=self.OLD_TRANSLATION_RC)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("no fscontext=", problems[0])

    def test_fr32_init_may_mount_the_google_apps_image_labelled_as_system_files(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        self.assertEqual(self._context_mount_problems("dsa-gapps.rc", policy), [])
        for point, fs, files in self._erofs_mounts((Path(make_image.__file__).with_name("dsa-gapps.rc")).read_text()):
            self.assertEqual((fs, files), ("dsa_gapps_fs", "system_file"), point)
        old = ("(allow init labeledfs (filesystem (mount relabelfrom)))\n"
               "(allow init system_file (filesystem (relabelto associate)))\n"
               "(allow system_file labeledfs (filesystem (associate)))\n")
        self.assertNotEqual(self._context_mount_problems("dsa-gapps.rc", old), [])

    # The flags init's mount command knows (system/core/init/builtins.cpp, mount_flags[], and
    # "wait"). Init passes the kernel only the last argument that is none of these, as the
    # mount's data, and drops every other one without a word.
    INIT_MOUNT_FLAGS = {"noatime", "noexec", "nosuid", "nodev", "nodiratime", "ro", "rw", "remount",
                        "bind", "rec", "unbindable", "private", "slave", "shared", "nosymfollow",
                        "defaults", "wait"}

    @classmethod
    def _dropped_mount_options(cls, rc):
        """The options of each mount command in an init script that init drops: every argument
        after the mount point that is not a flag init knows, except the last one."""
        dropped = []
        for line in rc.splitlines():
            words = line.split()
            if not words or words[0] != "mount":
                continue
            options = [word for word in words[4:-1] if word not in cls.INIT_MOUNT_FLAGS]
            if options:
                dropped.append((words[3], options))
        return dropped

    def test_fr25_fr32_init_hands_every_mount_option_to_the_kernel(self):
        for name in ["dsa-arm-translation.rc", "dsa-gapps.rc", "dsa-addons.rc"]:
            rc = (Path(make_image.__file__).with_name(name)).read_text()
            self.assertEqual(self._dropped_mount_options(rc), [], name)
        # develop 406c30d7 named fscontext= and context= as two arguments. Init dropped
        # fscontext=, so the kernel relabelled the superblock to the files' type and refused it
        # at every boot: "avc: denied { relabelto } for comm="init" scontext=u:r:init:s0
        # tcontext=u:object_r:system_lib_file:s0 tclass=filesystem", then "mount() failed:
        # Permission denied".
        old = ("    mount erofs loop@/system/etc/dsa/arm-translation/default.img /mnt/dsa-arm-translation "
               "ro nodev nosuid fscontext=u:object_r:dsa_arm_translation_fs:s0 "
               "context=u:object_r:system_lib_file:s0\n")
        self.assertEqual(self._dropped_mount_options(old),
                         [("/mnt/dsa-arm-translation", ["fscontext=u:object_r:dsa_arm_translation_fs:s0"])])

    def test_fr25_fr32_the_mount_rules_stay_clear_of_the_platforms_neverallows(self):
        # The platform's policy (Android 16 and 17) forbids relabelto on any fs_type that is not
        # a contextmount_type, filesystem associate from an fs_type to a file type, associate
        # with proc, and any domain creating, writing or relabelling a file or folder of a
        # contextmount_type. A rule that broke one would stop the policy compiling at boot.
        fs_types = {"labeledfs", "tmpfs", "rootfs", "proc", "binfmt_miscfs"}
        for name in ("arm-translation.cil", "gapps.cil", "addons.cil"):
            text = (Path(make_image.__file__).parent / "sepolicy" / name).read_text()
            attributes = self._attributes(text)
            declared = attributes.get("fs_type", set())
            contextmount = attributes.get("contextmount_type", set())
            for source, target, kind, perm in self._allowed(text):
                if kind != "filesystem":
                    self.assertNotIn(target, contextmount, f"{name}: {source} {perm} {kind} on {target}")
                    continue
                if perm == "relabelto" and target in fs_types | declared:
                    self.assertIn(target, contextmount, f"{name}: relabelto {target}")
                if perm == "associate":
                    self.assertNotIn(source, fs_types | declared, f"{name}: {source} associate {target}")
                    self.assertNotEqual(target, "proc", name)

    def test_fr25_fr32_every_domain_may_statfs_the_folders_the_images_are_laid_over(self):
        # statfs on an overlay is checked against the superblock of its top lower layer, so
        # every domain that statfs's /system/bin, /product or /system_ext needs filesystem
        # getattr on the image's superblock type. The platform gives every domain that on
        # every fs_type ("allow domain fs_type:filesystem getattr" in Android 16 and 17), so
        # the type is an fs_type and no rule here grants filesystem getattr on a file type,
        # which every such domain would otherwise need.
        for name, rc_name in (("arm-translation.cil", "dsa-arm-translation.rc"), ("gapps.cil", "dsa-gapps.rc")):
            text = (Path(make_image.__file__).parent / "sepolicy" / name).read_text()
            declared = self._attributes(text).get("fs_type", set())
            for point, fs, files in self._erofs_mounts((Path(make_image.__file__).with_name(rc_name)).read_text()):
                self.assertIn(fs, declared, f"{rc_name}: {point}")
            for source, target, kind, perm in self._allowed(text):
                if kind == "filesystem" and perm == "getattr":
                    self.assertIn(target, declared | {"labeledfs"}, f"{name}: {source} getattr {target}")

    def test_fr25_fr32_the_overlays_let_init_start_services_from_the_folders_they_cover(self):
        # On a nosuid mount the kernel refuses a domain change on exec (develop d69b9ff2:
        # "avc: denied { nosuid_transition } for comm="init" ... tcontext=u:r:vdc:s0", and vold
        # failed). /product and /system_ext are not nosuid, so neither are the overlays laid
        # over their folders. The translation script lays no overlay at all.
        rc = (Path(make_image.__file__).with_name("dsa-gapps.rc")).read_text()
        overlays = [line.strip() for line in rc.splitlines() if line.strip().startswith("mount overlay ")]
        self.assertTrue(overlays)
        for line in overlays:
            self.assertNotIn("nosuid", line.split(), line)
        rc = (Path(make_image.__file__).with_name("dsa-arm-translation.rc")).read_text()
        self.assertEqual([line for line in rc.splitlines() if line.strip().startswith("mount overlay ")], [])

    # What develop 3b9b304c laid over, which rebooted Android 5.6 s into the boot: an overlay
    # over /system/bin runs bpfloader through overlayfs, which looks it up with init's
    # credentials ("avc: denied { getattr } for comm="uprobestatsbpfl" name="bpfloader"
    # scontext=u:r:init:s0 tcontext=u:object_r:bpfloader_platform_exec:s0", "overlayfs: failed
    # to get metacopy (-13)", "execve('/system/bin/bpfloader'): 13[Permission denied]").
    OLD_LAID_OVER_RC = (
        "on post-fs-data && property:ro.boot.dsa.arm_translation=default\n"
        "    mount overlay overlay /system/lib64 ro nodev lowerdir=/mnt/dsa-arm-translation/system/lib64:/system/lib64\n"
        "    mount overlay overlay /system/bin ro nodev lowerdir=/mnt/dsa-arm-translation/system/bin:/system/bin\n"
        "    mount overlay overlay /system/etc ro nodev lowerdir=/mnt/dsa-arm-translation/system/etc:/system/etc\n")
    OLD_GAPPS_RC = (
        "on post-fs-data && property:ro.boot.dsa.gapps=on\n"
        "    mount overlay overlay /product ro nodev lowerdir=/mnt/dsa-gapps/product:/product\n"
        "    mount overlay overlay /system_ext ro nodev lowerdir=/mnt/dsa-gapps/system_ext:/system_ext\n")

    def test_fr25_fr32_nothing_is_laid_over_the_folders_of_the_systems_programs(self):
        for rc_name in ("dsa-arm-translation.rc", "dsa-gapps.rc"):
            rc = (Path(make_image.__file__).with_name(rc_name)).read_text()
            self.assertEqual(make_image.program_folder_problems(rc_name, rc), [], rc_name)
            for kind, point in make_image.mounts_onto(rc):
                self.assertNotIn(point, make_image.PROGRAM_FOLDERS, f"{rc_name}: {kind} onto {point}")
        old = make_image.program_folder_problems("dsa-arm-translation.rc", self.OLD_LAID_OVER_RC)
        self.assertEqual(len(old), 3, old)
        self.assertTrue(any("/system/bin" in problem for problem in old))
        old = make_image.program_folder_problems("dsa-gapps.rc", self.OLD_GAPPS_RC)
        self.assertEqual(len(old), 2, old)
        # An overlay of the translator over a folder of its own is still not how it is reached.
        sideways = "    mount overlay overlay /system/lib64/arm64 ro lowerdir=/a:/system/lib64/arm64\n"
        self.assertEqual(len(make_image.program_folder_problems("dsa-arm-translation.rc", sideways)), 1)

    def _modify_for(self, root, arch, extra=()):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
            archive.writestr("META/filesystem_config.txt", "system/bin/sh 0 2000 755 capabilities=0x0\n")
            archive.writestr("META/system_ext_filesystem_config.txt", "system_ext 0 0 755 capabilities=0x0\n")
            archive.writestr("META/product_filesystem_config.txt", "product 0 0 755 capabilities=0x0\n")
            archive.writestr("PRODUCT/etc/build.prop", "ro.product.product.name=dsa\n")
            archive.writestr("PRODUCT/app/Camera/Camera.apk", "apk")
            archive.writestr("SYSTEM_EXT/priv-app/", "")
            for name in extra:
                archive.writestr(name, "x")
        modified = root / f"modified-{arch}.zip"
        make_image.modify(source, modified, arch=arch)
        return zipfile.ZipFile(modified)

    def test_fr25_an_x86_64_image_carries_empty_places_for_the_package_and_an_arm64_image_none(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self._modify_for(root, "x86_64") as archive:
                names = archive.namelist()
                for folder in make_image.ARM_TRANSLATION_FOLDERS:
                    info = archive.getinfo(folder + "/")
                    self.assertTrue(stat.S_ISDIR(info.external_attr >> 16), folder)
                    self.assertFalse([n for n in names if n.startswith(folder + "/") and n != folder + "/"],
                                     f"{folder} stays empty, so the package hides nothing")
                self.assertEqual(archive.read("SYSTEM/etc/ld.config.arm64.txt"), b"")
                self.assertEqual(archive.read("SYSTEM/lib64/libberberis_exec_region.so"), b"")
                for name, target in make_image.ARM_TRANSLATION_LINKS.items():
                    self.assertTrue(stat.S_ISLNK(archive.getinfo(name).external_attr >> 16), name)
                    self.assertEqual(archive.read(name).decode(), target)
                    if name.startswith("SYSTEM/etc/"):
                        self.assertTrue(target.startswith("/mnt/dsa-arm-translation/system/etc/"), target)
                    else:
                        # An older package's rule names /system/bin/<runner>; the link leads
                        # there from beside it, to where the package's system/bin is bound.
                        self.assertEqual(target, "dsa-arm-translation/" + name.rsplit("/", 1)[1])
                system = archive.read("META/filesystem_config.txt").decode()
                system_ext = archive.read("META/system_ext_filesystem_config.txt").decode()
                for rule in make_image.ARM_TRANSLATION_REACH_FS_CONFIG["META/filesystem_config.txt"]:
                    self.assertIn(rule, system)
                self.assertIn("system_ext/lib64 0 0 755 capabilities=0x0", system_ext)
                manifest = {"arch": "x86_64"}
                self.assertEqual(make_image.check_arm_translation_reach(
                    manifest, names, archive.read, archive.getinfo), [])
                # An arm64 image with these would have its own linker read the empty file.
                self.assertTrue(make_image.check_arm_translation_reach(
                    {"arch": "arm64"}, names, archive.read, archive.getinfo))
            with self._modify_for(root, "arm64") as archive:
                names = archive.namelist()
                for name in make_image.arm_translation_reach_names():
                    self.assertNotIn(name, names)
                self.assertNotIn("system/lib64/arm64 ", archive.read("META/filesystem_config.txt").decode())
                self.assertEqual(make_image.check_arm_translation_reach(
                    {"arch": "arm64"}, names, archive.read, archive.getinfo), [])
                problems = make_image.check_arm_translation_reach(
                    {"arch": "x86_64"}, names, archive.read, archive.getinfo)
                self.assertEqual(len(problems), len(make_image.arm_translation_reach_names()), problems)

    def test_fr25_a_build_with_files_where_the_package_goes_is_refused(self):
        for name in ["SYSTEM_EXT/lib64/libfoo.so", "SYSTEM/lib64/arm64/libc.so", "SYSTEM/bin/arm64/linker64",
                     "SYSTEM/etc/ld.config.arm64.txt", "SYSTEM/etc/berberis/cpuinfo.arm64.txt",
                     "SYSTEM/lib64/libberberis_exec_region.so"]:
            with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError, msg=name) as caught:
                self._modify_for(Path(tmp), "x86_64", extra=[name]).close()
            self.assertIn(name.split("/")[0], str(caught.exception))
        # An arm64 build is not bound into, so its own files there stay.
        with tempfile.TemporaryDirectory() as tmp:
            self._modify_for(Path(tmp), "arm64", extra=["SYSTEM/lib64/arm64/libc.so"]).close()

    def test_fr25_the_exec_region_library_is_bound_where_berberis_opens_it(self):
        # Berberis opens its exec region library by this absolute path (runtime_primitives/
        # exec_region_elf_backed.cc, kExecRegionLibraryPath), not by name. While the package's
        # system/lib64 was bound onto /system_ext/lib64 alone, the path held nothing and every
        # ARM app aborted as it started: "exec_region_elf_backed.cc:48: CHECK failed: 0 != 0".
        opened = "/system/lib64/libberberis_exec_region.so"
        self.assertEqual("/" + make_image.ARM_TRANSLATION_EXEC_REGION.lower(), opened)
        self.assertIn(make_image.ARM_TRANSLATION_EXEC_REGION, make_image.ARM_TRANSLATION_BOUND_FILES)
        self.assertIn(make_image.ARM_TRANSLATION_EXEC_REGION, make_image.arm_translation_reach_names())
        rc = (Path(make_image.__file__).with_name("dsa-arm-translation.rc")).read_text()
        sections = dict(self._sections(rc))
        bind = f"mount none /mnt/dsa-arm-translation{opened} {opened} bind"
        for value in ["on", "default"]:
            section = sections[f"on post-fs-data && property:ro.boot.dsa.arm_translation={value}"]
            self.assertIn(bind, section, value)
            # Bound before the binfmt_misc rules let any ARM64 program start.
            self.assertLess(section.index(bind), section.index(
                "mount binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc nodev noexec nosuid"), value)
        # A bind, not a link: the linker checks the path it really opened against the system
        # namespace, and a link's would be under /mnt.
        self.assertNotIn(make_image.ARM_TRANSLATION_EXEC_REGION, make_image.ARM_TRANSLATION_LINKS)
        with tempfile.TemporaryDirectory() as tmp, self._modify_for(Path(tmp), "x86_64") as archive:
            info = archive.getinfo(make_image.ARM_TRANSLATION_EXEC_REGION)
            self.assertFalse(stat.S_ISLNK(info.external_attr >> 16))
            self.assertEqual(archive.read(make_image.ARM_TRANSLATION_EXEC_REGION), b"")
            self.assertIn("system/lib64/libberberis_exec_region.so 0 0 644 capabilities=0x0",
                          archive.read("META/filesystem_config.txt").decode())
            files = {name: archive.read(name) for name in archive.namelist()}
            infos = {name: archive.getinfo(name) for name in archive.namelist()}

        def check(files):
            return make_image.check_arm_translation_reach({"arch": "x86_64"}, list(files), files.__getitem__,
                                                         infos.get)

        self.assertEqual(check(files), [])
        gone = {n: d for n, d in files.items() if n != make_image.ARM_TRANSLATION_EXEC_REGION}
        self.assertTrue(any("every ARM app aborts" in problem for problem in check(gone)), check(gone))
        filled = dict(files, **{make_image.ARM_TRANSLATION_EXEC_REGION: b"\x7fELF"})
        self.assertTrue(any("not empty" in problem for problem in check(filled)), check(filled))

    def test_fr25_check_image_finds_a_place_of_the_package_missing_or_filled(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify_for(Path(tmp), "x86_64") as archive:
            files = {name: archive.read(name) for name in archive.namelist()}
            infos = {name: archive.getinfo(name) for name in archive.namelist()}

        def check(files):
            return make_image.check_arm_translation_reach({"arch": "x86_64"}, list(files), files.__getitem__,
                                                         infos.get)

        self.assertEqual(check(files), [])
        for name in make_image.arm_translation_reach_names():
            gone = {n: d for n, d in files.items() if n != name}
            self.assertTrue(check(gone), name)
        filled = dict(files, **{"SYSTEM_EXT/lib64/libfoo.so": b"x"})
        self.assertTrue(any("hides" in problem for problem in check(filled)))
        config = dict(files, **{"SYSTEM/etc/ld.config.arm64.txt": b"[system]\n"})
        self.assertTrue(any("not empty" in problem for problem in check(config)))
        elsewhere = dict(files, **{"SYSTEM/etc/berberis": b"/system/etc/other"})
        self.assertTrue(any("does not lead" in problem for problem in check(elsewhere)))

    def test_fr32_the_folders_the_google_apps_are_laid_over_are_in_every_image(self):
        rc = (Path(make_image.__file__).with_name("dsa-gapps.rc")).read_text()
        overlaid = [point for kind, point in make_image.mounts_onto(rc) if kind == "overlay"]
        self.assertEqual(sorted(overlaid), sorted("/" + folder.lower() for folder in make_image.gapps_folders()))
        with tempfile.TemporaryDirectory() as tmp:
            for arch in ("x86_64", "arm64"):
                with self._modify_for(Path(tmp), arch) as archive:
                    names = archive.namelist()
                    self.assertEqual(make_image.gapps_folder_problems(names), [], arch)
                    # The build's own folders keep their entries; only the missing ones are added.
                    self.assertNotIn("PRODUCT/app/", names)
                    self.assertEqual(names.count("SYSTEM_EXT/priv-app/"), 1)
                    self.assertTrue(stat.S_ISDIR(archive.getinfo("PRODUCT/framework/").external_attr >> 16))
                    product = archive.read("META/product_filesystem_config.txt").decode()
                    self.assertIn("product/etc/default-permissions 0 0 755 capabilities=0x0", product)
                    self.assertNotIn("product/app ", product)
                    system_ext = archive.read("META/system_ext_filesystem_config.txt").decode()
                    self.assertIn("system_ext/etc/preferred-apps 0 0 755 capabilities=0x0", system_ext)
        self.assertEqual(len(make_image.gapps_folder_problems([])), len(make_image.gapps_folders()))

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
        sections = self._sections(rc)
        triggers = [trigger for trigger, _ in sections]
        on = "on post-fs-data && property:ro.boot.dsa.arm_translation=on"
        # Where the image is: a user's package on the add-on disk, or, for one boot after an
        # update, where an older DSA kept it in /data; the image's own default package.
        images = {
            f"{on} && property:ro.boot.dsa.arm_translation_image=disk": "/mnt/dsa-addons/arm-translation.img",
            f"{on} && property:ro.boot.dsa.arm_translation_image=data":
                "/data/vendor/dsa/arm-translation/system.img",
            "on post-fs-data && property:ro.boot.dsa.arm_translation=default":
                "/system/etc/dsa/arm-translation/default.img",
        }
        # In the host's direct-mapped mode, the image's own partition on the persistent memory
        # the add-on disk is then, mapped with DAX rather than read into the guest's cache.
        mapped = f"{on} && property:ro.boot.dsa.arm_translation_image=dax"
        images[mapped] = "/dev/block/by-name/dsa_arm_translation"
        for trigger, image in images.items():
            commands = dict(sections)[trigger]
            option = "dax=always" if trigger == mapped else "directio"
            mount = (f"mount erofs {image} /mnt/dsa-arm-translation ro nodev nosuid "
                     f"{option},fscontext=u:object_r:dsa_arm_translation_fs:s0,"
                     "context=u:object_r:system_lib_file:s0")
            self.assertIn(mount, commands, trigger)
            self.assertEqual([c for c in commands if c.startswith("mount erofs ")], [mount], trigger)
        # A user's package is mounted, from wherever it is, before its parts are bound: init
        # runs the actions of one trigger in the order of the file.
        for trigger in images:
            if trigger.startswith(on + " "):
                self.assertLess(triggers.index(trigger), triggers.index(on), trigger)
        binders = {
            on: dict(sections)[on],
            "default": dict(sections)["on post-fs-data && property:ro.boot.dsa.arm_translation=default"],
        }
        for value, section in binders.items():
            # Each part of the package is bound onto the empty place the image carries for it,
            # once the image is mounted, and nothing else is mounted from it.
            binds = []
            for folder, inside in make_image.ARM_TRANSLATION_FOLDERS.items():
                binds.append(f"mount none /mnt/dsa-arm-translation/{inside} /{folder.lower()} bind")
            for name in make_image.ARM_TRANSLATION_BOUND_FILES:
                binds.append(f"mount none /mnt/dsa-arm-translation/{name.lower()} /{name.lower()} bind")
            erofs = [i for i, c in enumerate(section) if c.startswith("mount erofs ")]
            for bind in binds:
                self.assertIn(bind, section, value)
                if erofs:
                    self.assertGreater(section.index(bind), erofs[0], bind)
            self.assertEqual(sorted(c for c in section if c.startswith("mount none ")), sorted(binds))
            # Nothing else in Android mounts binfmt_misc, so the handlers are registered only
            # once it is mounted here.
            mount = section.index("mount binfmt_misc binfmt_misc /proc/sys/fs/binfmt_misc nodev noexec nosuid")
            for rule in ["arm64_exe", "arm64_dyn"]:
                register = section.index(f"copy /mnt/dsa-arm-translation/system/etc/binfmt_misc/{rule} "
                                         "/proc/sys/fs/binfmt_misc/register")
                self.assertGreater(register, mount)
        # Every mount waits for the host's word, and nothing makes a folder in /data for a
        # package any more: the host keeps it on the add-on disk.
        for trigger, commands in sections:
            if trigger not in set(images) | {on}:
                self.assertFalse([c for c in commands if c.startswith("mount ")], trigger)
            self.assertFalse([c for c in commands if c.startswith("mkdir /data")], trigger)
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            self.assertEqual(archive.read("SYSTEM/etc/init/dsa-arm-translation.rc").decode(), rc)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            # The places the package is bound onto: /system_ext/lib64 and /system/lib64/arm64
            # (system libraries), /system/bin/arm64 and /system/bin/dsa-arm-translation (system
            # files), and /system/etc/ld.config.arm64.txt (the linker's configuration).
            self.assertIn("(allow init system_lib_file (dir (mounton)))", policy)
            self.assertIn("(allow init system_file (dir (mounton)))", policy)
            self.assertIn("(allow init system_linker_config_file (file (mounton)))", policy)
            self.assertIn("(allow init system_lib_file (file (mounton)))", policy)
            self.assertIn("(typeattributeset fs_type (dsa_arm_translation_fs))", policy)
            self.assertIn("(typeattributeset contextmount_type (dsa_arm_translation_fs))", policy)
            # No domain is given filesystem getattr on a file type: the superblock is not one.
            self.assertNotIn("(allow init system_lib_file (filesystem (getattr)))", policy)
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
            self.assertEqual([line.split("=", 1)[0] for line in props],
                             ["ro.oem.build.fingerprint", "ro.control_privapp_permissions"])
            off = archive.read("SYSTEM/etc/dsa/gapps-off.prop").decode().splitlines()
            self.assertEqual([line for line in off if line and not line.startswith("#")], [])
            config = archive.read("META/filesystem_config.txt").decode()
            for rule in make_image.GAPPS_FS_CONFIG:
                self.assertIn(rule, config)

    def test_fr32_a_boot_with_the_google_apps_logs_a_missing_privileged_permission(self):
        # LiteGapps' lists for Android 17 left out ACCESS_COMPANION_INFO, and with the allowlist
        # enforced system_server stopped at systemReady every ten seconds: Android never started.
        on = (Path(make_image.__file__).with_name("gapps-on.prop")).read_text().splitlines()
        self.assertIn(make_image.GAPPS_PRIVAPP_LOG, on)
        self.assertEqual(make_image.GAPPS_PRIVAPP_LOG, "ro.control_privapp_permissions=log")
        off = (Path(make_image.__file__).with_name("gapps-off.prop")).read_text()
        self.assertNotIn("ro.control_privapp_permissions", off)
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            self.assertIn(make_image.GAPPS_PRIVAPP_LOG,
                          archive.read("SYSTEM/etc/dsa/gapps-on.prop").decode().splitlines())
            # Nothing the product properties read after the import sets it back.
            product = archive.read("PRODUCT/etc/build.prop").decode().splitlines()
            after = product[product.index(make_image.GAPPS_IMPORT) + 1:]
            self.assertFalse([line for line in after if "control_privapp_permissions" in line])

    def test_fr32_the_google_apps_are_laid_over_only_when_the_host_says_they_are_installed(self):
        rc = (Path(make_image.__file__).with_name("dsa-gapps.rc")).read_text()
        sections = self._sections(rc)
        triggers = [trigger for trigger, _ in sections]
        on = "on post-fs-data && property:ro.boot.dsa.gapps=on"
        # Where the image is: on the add-on disk, or, for one boot after an update, where an
        # older DSA kept it in /data.
        images = {
            f"{on} && property:ro.boot.dsa.gapps_image=disk": "/mnt/dsa-addons/gapps.img",
            f"{on} && property:ro.boot.dsa.gapps_image=data": "/data/vendor/dsa/gapps/system.img",
            # In the host's direct-mapped mode, the image's own partition on the persistent
            # memory the add-on disk is then, mapped with DAX rather than cached by the guest.
            f"{on} && property:ro.boot.dsa.gapps_image=dax": "/dev/block/by-name/dsa_gapps",
        }
        for trigger, image in images.items():
            commands = dict(sections)[trigger]
            option = "dax=always" if trigger.endswith("=dax") else "directio"
            mount = (f"mount erofs {image} /mnt/dsa-gapps ro nodev nosuid "
                     f"{option},fscontext=u:object_r:dsa_gapps_fs:s0,context=u:object_r:system_file:s0")
            self.assertEqual([c for c in commands if c.startswith("mount ")], [mount], trigger)
            # Mounted before it is laid over: init runs one trigger's actions in file order.
            self.assertLess(triggers.index(trigger), triggers.index(on), trigger)
        # Every action of that trigger, in order: the overlays, and anything else done then.
        section = [command for trigger, commands in sections if trigger == on for command in commands]
        for folder in make_image.gapps_folders():
            point = "/" + folder.lower()
            self.assertIn(
                f"mount overlay overlay {point} ro nodev "
                f"lowerdir=/mnt/dsa-gapps{point}:{point}",
                section,
            )
        self.assertEqual(len([c for c in section if c.startswith("mount ")]), len(make_image.gapps_folders()))
        for partition in ["product", "system_ext"]:
            self.assertNotIn(f"mount overlay overlay /{partition} ro nodev lowerdir=/mnt/dsa-gapps/{partition}:/{partition}",
                             section)
        # Every mount waits for the host's word, and nothing makes a folder in /data for the
        # image any more: the host keeps it on the add-on disk.
        for trigger, commands in sections:
            if trigger not in set(images) | {on}:
                self.assertFalse([c for c in commands if c.startswith("mount ")], trigger)
            self.assertFalse([c for c in commands if c.startswith("mkdir /data")], trigger)
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            self.assertEqual(archive.read("SYSTEM/etc/init/dsa-gapps.rc").decode(), rc)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertIn("(typeattributeset fs_type (dsa_gapps_fs))", policy)
            self.assertIn("(typeattributeset contextmount_type (dsa_gapps_fs))", policy)
            self.assertIn("(allow init dsa_gapps_fs (filesystem (mount relabelfrom relabelto)))", policy)
            self.assertIn("(allow system_file dsa_gapps_fs (filesystem (associate)))", policy)
            # No domain is given filesystem getattr on a file type: the superblock is not one.
            self.assertNotIn("(allow init system_file (filesystem (getattr)))", policy)

    def test_fr32_the_addon_disk_is_mounted_read_only_before_the_images_on_it(self):
        rc = (Path(make_image.__file__).with_name("dsa-addons.rc")).read_text()
        sections = self._sections(rc)
        self.assertEqual([trigger for trigger, _ in sections], [make_image.ADDONS_TRIGGER])
        commands = sections[0][1]
        self.assertEqual([c for c in commands if c.startswith("mount ")], [make_image.ADDONS_MOUNT])
        self.assertIn("mkdir /mnt/dsa-addons 0755 root root", commands)
        self.assertLess(commands.index("mkdir /mnt/dsa-addons 0755 root root"),
                        commands.index(make_image.ADDONS_MOUNT))
        # Read only, nothing on it runs or is a device, and labelled as system files, which init
        # reads as it mounts each image from its file.
        words = make_image.ADDONS_MOUNT.split()
        for flag in ["ro", "nodev", "nosuid", "noexec"]:
            self.assertIn(flag, words)
        self.assertEqual(self._dropped_mount_options(rc), [])
        self.assertEqual(self._erofs_mounts(rc), [("/mnt/dsa-addons", "dsa_addons_fs", "system_file")])
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            self.assertEqual(archive.read(make_image.ADDONS_RC).decode(), rc)
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            config = archive.read("META/filesystem_config.txt").decode()
            files = {name: archive.read(name) for name in archive.namelist()}
        self.assertEqual(self._context_mount_problems("dsa-addons.rc", policy), [])
        for rule in make_image.ADDONS_FS_CONFIG:
            self.assertIn(rule, config)
        self.assertEqual(make_image.addon_disk_problems(list(files), files.__getitem__), [])
        # An image without the script, one that mounts the disk without waiting for the host, or
        # whose Google apps still come only from /data, is refused.
        gone = {n: d for n, d in files.items() if n != make_image.ADDONS_RC}
        self.assertIn("is missing", make_image.addon_disk_problems(list(gone), gone.__getitem__)[0])
        early = dict(files, **{make_image.ADDONS_RC: ("on early-init\n    " + make_image.ADDONS_MOUNT + "\n").encode()})
        self.assertEqual(len(make_image.addon_disk_problems(list(early), early.__getitem__)), 1)
        writable = dict(files, **{make_image.ADDONS_RC: rc.replace(" ro nodev", " rw nodev").encode()})
        self.assertEqual(len(make_image.addon_disk_problems(list(writable), writable.__getitem__)), 1)
        old_gapps = files[make_image.GAPPS_RC].decode().replace("/mnt/dsa-addons/gapps.img", "/data/x.img")
        old = dict(files, **{make_image.GAPPS_RC: old_gapps.encode()})
        problems = make_image.addon_disk_problems(list(old), old.__getitem__)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("dsa-gapps.rc never mounts /mnt/dsa-addons/gapps.img", problems[0])

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
                        # The "always" image also has 0.2.0's "on" file, which enforced the
                        # allowlist.
                        on = ("ro.oem.build.fingerprint=x\n" if case == "always"
                              else (Path(make_image.__file__).with_name("gapps-on.prop")).read_text())
                        archive.writestr("SYSTEM/etc/dsa/gapps-on.prop", on)
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
        self.assertIn("stops Android at every boot", always)
        self.assertNotIn("stops Android at every boot", good)
        self.assertIn("x86_64 is not the ABI apps are installed for first", arm_first)
        self.assertNotIn("x86_64 is not the ABI apps are installed for first", good)


# What `cmd package art dump` prints for an updated Google Play services (Android 17), with the
# filter and reason of its base APK left to fill in.
ART_DUMP = """[{package}]
  path: /data/app/~~tRmniAnGjxf1d7FhqhzuvQ==/{package}-JblIlLyjElnBue39spDegw==/base.apk
    x86_64: [status={status}] [reason={reason}] [primary-abi]
      [location is /data/app/~~tRmniAnGjxf1d7FhqhzuvQ==/{package}-JblIlLyjElnBue39spDegw==/oat/x86_64/base.odex]
    used by other apps: [com.google.android.youtube (isa=x86_64)]
  path: /data/app/~~tRmniAnGjxf1d7FhqhzuvQ==/{package}-JblIlLyjElnBue39spDegw==/split_AdsDynamite_installtime.apk
    x86_64: [status=verify] [reason=vdex-dm] [primary-abi]
      [location is primary.vdex mapped directly in memory]
"""


class GoogleAppsCompileTests(unittest.TestCase):
    """Google Play services and the Play Store compiled again without an app image, which each
    of their processes loaded into memory of its own: 25-30 MiB each for
    com.google.android.gms and com.google.android.gms.unstable on a PC."""

    RC = Path(__file__).with_name("dsa-gapps.rc")
    SCRIPT = Path(__file__).with_name("gapps-compile.sh")

    @staticmethod
    def _sections(rc):
        """Each action and service of an init script: its first line and its command lines."""
        sections = []
        for line in rc.splitlines():
            if line.startswith(("on ", "service ")):
                sections.append((line, []))
            elif line.strip() and not line.lstrip().startswith("#") and sections:
                sections[-1][1].append(line.strip())
        return sections

    def test_nfr2_init_runs_the_compile_as_the_shell_with_the_app_image_format_cleared(self):
        sections = dict(self._sections(self.RC.read_text()))
        service = sections["service dsa_gapps_compile /system/bin/sh /system/etc/dsa/gapps-compile.sh"]
        self.assertIn("user shell", service)
        self.assertIn("seclabel u:r:shell:s0", service)
        self.assertIn("oneshot", service)
        self.assertIn("disabled", service)
        clear_and_start = ['setprop dalvik.vm.appimageformat ""', "start dsa_gapps_compile"]
        # At each boot with the Google apps, and whenever the agent asks.
        self.assertEqual(sections["on property:sys.boot_completed=1 && property:ro.boot.dsa.gapps=on"],
                         clear_and_start)
        self.assertEqual(sections[f"on property:{make_image.GAPPS_COMPILE_PROPERTY}=* && property:ro.boot.dsa.gapps=on"],
                         clear_and_start)
        # The build's format is kept before the first compile and put back after every one, with
        # the image's own format should the keeping have failed.
        self.assertIn(f"setprop {make_image.GAPPS_IMAGE_FORMAT_PROPERTY} ${{dalvik.vm.appimageformat}}",
                      [command for line, commands in self._sections(self.RC.read_text())
                       if line == "on post-fs-data && property:ro.boot.dsa.gapps=on" for command in commands])
        self.assertEqual(sections["on property:init.svc.dsa_gapps_compile=stopped"],
                         [f"setprop dalvik.vm.appimageformat ${{{make_image.GAPPS_IMAGE_FORMAT_PROPERTY}:-lz4}}"])

    def test_nfr2_the_image_carries_the_script_its_properties_and_its_policy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
                archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("META/filesystem_config.txt", "system/bin/sh 0 2000 755 capabilities=0x0\n")
                archive.writestr("PRODUCT/etc/build.prop", "ro.product.product.name=dsa\n")
            modified = root / "modified.zip"
            make_image.modify(source, modified)
            with zipfile.ZipFile(modified) as archive:
                self.assertEqual(archive.read(make_image.GAPPS_COMPILE_SCRIPT), self.SCRIPT.read_bytes())
                config = archive.read("META/filesystem_config.txt").decode()
                contexts = archive.read(make_image.VENDOR_PROPERTY_CONTEXTS).decode().splitlines()
                policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        self.assertIn("system/etc/dsa/gapps-compile.sh 0 0 644 capabilities=0x0", config)
        self.assertIn("vendor.dsa.gapps.compile u:object_r:dsa_gapps_compile_prop:s0 exact string", contexts)
        self.assertIn("vendor.dsa.gapps.appimageformat u:object_r:dsa_gapps_compile_prop:s0 exact string", contexts)
        # The agent asks; nothing else but init sets the property, and the script reads it.
        setters = set(re.findall(r"\(allow (\S+) dsa_gapps_compile_prop \(property_service \(set\)\)\)", policy))
        self.assertEqual(setters, {"platform_app"})
        self.assertIn("(allow shell dsa_gapps_compile_prop (file (read open getattr map)))", policy)

    def _run_script(self, packages, request_changes=0):
        """Runs gapps-compile.sh with a fake `cmd`, `getprop` and `log`. `packages` maps each
        package to its base APK's (filter, reason), or None when it is not installed. Returns
        the compile commands it ran and what it logged."""
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        root = Path(folder.name)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        state = root / "state"
        state.mkdir()
        for package, status in packages.items():
            if status is not None:
                (state / package).write_text(ART_DUMP.format(package=package, status=status[0], reason=status[1]))
        (state / "request").write_text("1")
        (state / "changes").write_text(str(request_changes))
        dump = ART_DUMP.replace("{status}", "speed-profile").replace("{reason}", "cmdline")
        (state / "compiled-dump").write_text(dump)
        scripts = {
            # The package manager: ART's dump of a package, and a compile, which ART records with
            # the reason "cmdline".
            "cmd": f"""#!/bin/sh
s={state}
if [ "$1 $2 $3" = "package art dump" ]; then cat "$s/$4" 2>/dev/null; exit 0; fi
if [ "$1 $2" = "package compile" ]; then
    echo "$*" >> "$s/compiles"
    for last; do :; done
    sed "s/{{package}}/$last/g" "$s/compiled-dump" > "$s/$last"
    exit 0
fi
exit 1
""",
            # The request property, which changes `changes` times while the script runs.
            "getprop": f"""#!/bin/sh
s={state}
n=$(cat "$s/request")
c=$(cat "$s/changes")
if [ "$c" -gt 0 ] && [ -f "$s/compiles" ]; then echo $((n + 1)) > "$s/request"; echo $((c - 1)) > "$s/changes"; fi
cat "$s/request"
""",
            "log": f"""#!/bin/sh
echo "$*" >> {state}/log
""",
        }
        for name, text in scripts.items():
            (bin_dir / name).write_text(text)
            (bin_dir / name).chmod(0o755)
        env = {"PATH": f"{bin_dir}:/usr/bin:/bin"}
        subprocess.run(["sh", str(self.SCRIPT)], env=env, check=True, timeout=30)
        compiles = (state / "compiles").read_text().splitlines() if (state / "compiles").exists() else []
        log = (state / "log").read_text() if (state / "log").exists() else ""
        return compiles, log

    @unittest.skipUnless(sys.platform.startswith("linux"), "runs the script with a POSIX sh")
    def test_nfr2_a_package_compiled_with_an_app_image_is_compiled_again_and_only_once(self):
        compiles, log = self._run_script({
            "com.google.android.gms": ("speed-profile", "install-dm"),
            "com.android.vending": ("speed-profile", "bg-dexopt"),
        })
        self.assertEqual(compiles, [
            "package compile -m speed-profile -p PRIORITY_BACKGROUND -f com.google.android.gms",
            "package compile -m speed-profile -p PRIORITY_BACKGROUND -f com.android.vending",
        ])
        self.assertIn("compiled com.google.android.gms without an app image: speed-profile cmdline", log)
        self.assertIn("checked com.google.android.gms com.android.vending", log)

    @unittest.skipUnless(sys.platform.startswith("linux"), "runs the script with a POSIX sh")
    def test_nfr2_a_package_without_an_app_image_is_left_alone(self):
        # Its own earlier compile, a compile without a profile, and Google Play not installed.
        for packages in [
            {"com.google.android.gms": ("speed-profile", "cmdline")},
            {"com.google.android.gms": ("verify", "boot-after-ota"), "com.android.vending": ("verify", "install")},
            {"com.google.android.gms": None, "com.android.vending": None},
        ]:
            compiles, log = self._run_script(packages)
            self.assertEqual(compiles, [], packages)
            self.assertIn("checked", log)

    @unittest.skipUnless(sys.platform.startswith("linux"), "runs the script with a POSIX sh")
    def test_nfr2_a_request_that_comes_while_the_script_runs_is_not_lost(self):
        # The agent asks again while the first compile runs: the script looks once more, and
        # finds both packages already compiled by it.
        compiles, log = self._run_script({"com.google.android.gms": ("speed-profile", "install")},
                                         request_changes=1)
        self.assertEqual(len(compiles), 1)
        self.assertEqual(log.count("checked"), 1)


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


class KeyMintImageTests(unittest.TestCase):
    """DSA's KeyMint from the Framework build, whose root key of key blobs comes from the host, in
    place of the KeyMint APEX, whose root key is a fixed all-zero key."""

    MODULE = "com.android.hardware.keymint"
    APEX = "VENDOR/apex/com.android.hardware.keymint.rust_nonsecure.apex"
    FRAGMENTS = {
        "VENDOR/etc/vintf/manifest/android.hardware.security.keymint-service.xml":
            b'<manifest version="1.0" type="device">\n'
            b'    <hal format="aidl">\n        <name>android.hardware.security.keymint</name>\n'
            b'        <version>4</version>\n        <fqname>IKeyMintDevice/default</fqname>\n    </hal>\n'
            b'    <hal format="aidl">\n        <name>android.hardware.security.keymint</name>\n'
            b'        <version>3</version>\n        <fqname>IRemotelyProvisionedComponent/default</fqname>\n'
            b'    </hal>\n</manifest>\n',
        "VENDOR/etc/vintf/manifest/android.hardware.security.secureclock-service.xml":
            b'<manifest version="1.0" type="device">\n    <hal format="aidl">\n'
            b'        <name>android.hardware.security.secureclock</name>\n'
            b'        <fqname>ISecureClock/default</fqname>\n    </hal>\n</manifest>\n',
        "VENDOR/etc/vintf/manifest/android.hardware.security.sharedsecret-service.xml":
            b'<manifest version="1.0" type="device">\n    <hal format="aidl">\n'
            b'        <name>android.hardware.security.sharedsecret</name>\n'
            b'        <fqname>ISharedSecret/default</fqname>\n    </hal>\n</manifest>\n',
        "VENDOR/etc/permissions/android.hardware.hardware_keystore.xml":
            b'<permissions>\n  <feature name="android.hardware.hardware_keystore" version="400" />\n'
            b'</permissions>\n',
    }
    # What the nonsecure service links once its Rust crates are linked in.
    NEEDED = ["libbinder_ndk.so", "libcrypto.so", "libssl.so", "libcutils.so", "liblog.so", "libc++.so",
              "libc.so", "libm.so", "libdl.so"]
    VENDOR_LIBRARIES = ["VENDOR/lib64/libcrypto.so", "VENDOR/lib64/libcutils.so", "VENDOR/lib64/libbase.so",
                        "VENDOR/lib64/libc++.so"]

    @staticmethod
    def _elf(machine, needed=(), body=b""):
        """A 64-bit ELF file whose dynamic section needs `needed`."""
        strtab = b"\0"
        offsets = []
        for library in needed:
            offsets.append(len(strtab))
            strtab += library.encode() + b"\0"
        dynamic = b"".join(struct.pack("<qQ", 1, offset) for offset in offsets) + struct.pack("<qQ", 0, 0)
        strtab_at = 64 + len(body)
        dynamic_at = strtab_at + len(strtab)
        sections_at = dynamic_at + len(dynamic)
        header = bytearray(64)
        header[:4] = b"\x7fELF"
        header[4] = 2
        header[5] = 1
        header[18:20] = machine.to_bytes(2, "little")
        header[0x28:0x30] = sections_at.to_bytes(8, "little")
        header[0x3A:0x3C] = (64).to_bytes(2, "little")
        header[0x3C:0x3E] = (3).to_bytes(2, "little")

        def section(kind, offset, size, link):
            return struct.pack("<IIQQQQIIQQ", 0, kind, 0, 0, offset, size, link, 0, 0, 0)

        sections = section(0, 0, 0, 0) + section(3, strtab_at, len(strtab), 0) + section(6, dynamic_at, len(dynamic), 1)
        return bytes(header) + body + strtab + dynamic + sections

    @staticmethod
    def _vendor_apex(module, payload=b""):
        name = module.encode()
        data = io.BytesIO()
        with zipfile.ZipFile(data, "w") as apex:
            apex.writestr("apex_manifest.pb", b"\x0a" + bytes([len(name)]) + name + b"\x10\x01")
            apex.writestr("apex_payload.img", b"\0" * 64 + payload + b"\0" * 64)
        return data.getvalue()

    def _framework(self, root, machine=62, needed=None):
        folder = root / "framework"
        contents = {name: self._elf(machine, body=b"patched " + name.encode()) for name in make_image.FRAMEWORK_FILES}
        contents[make_image.KEYMINT_BINARY] = self._elf(machine, self.NEEDED if needed is None else needed,
                                                        b"dsa-keymint")
        contents["VENDOR/lib64/libssl.so"] = self._elf(machine, ["libcrypto.so", "libc.so"], b"libssl")
        contents.update(self.FRAGMENTS)
        files = {}
        for name, data in contents.items():
            (folder / name).parent.mkdir(parents=True, exist_ok=True)
            (folder / name).write_bytes(data)
            files[name] = {"size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        (folder / "framework.json").write_text(json.dumps({
            "android": "android16", "android_major": 16, "build_id": "15581820", "release": "bp4a",
            "arch": "x86_64", "version": "16-0123456789ab", "files": files}))
        return folder

    def _source(self, root):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type surfaceflinger)\n")
            archive.writestr("SYSTEM/build.prop", "ro.debuggable=1\n")
            archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/hw 0 2000 751 capabilities=0x0\n")
            archive.writestr("VENDOR/etc/selinux/vendor_file_contexts",
                             "/(vendor|system/vendor)/bin/hw/android\\.hardware\\.security\\.keymint-service"
                             "   u:object_r:hal_keymint_default_exec:s0\n")
            for name in make_image.FRAMEWORK_FILES:
                archive.writestr(name, self._elf(62, body=b"published " + name.encode()))
            for name in self.VENDOR_LIBRARIES:
                archive.writestr(name, self._elf(62))
            archive.writestr(self.APEX, self._vendor_apex(
                self.MODULE, self.FRAGMENTS["VENDOR/etc/vintf/manifest/android.hardware.security.keymint-service.xml"]))
        return source

    def _image(self, root):
        payloads, description = make_image.checked_framework(self._framework(root), "15581820", 16, "x86_64")
        modified = root / "modified.zip"
        make_image.modify(self._source(root), modified, framework=payloads)
        manifest = {"arch": "x86_64", "source": {"build_id": "15581820"},
                    "kernel_cmdline": make_image.kernel_cmdline("blkdevparts=x:1@0(misc)", keymint=True),
                    "framework": make_image.framework_entry(description)}
        return payloads, modified, manifest

    def test_fr36_the_keymint_is_installed_as_vendor_files_in_the_apexs_domain(self):
        with tempfile.TemporaryDirectory() as tmp:
            payloads, modified, _ = self._image(Path(tmp))
            self.assertEqual(sorted(payloads), sorted(make_image.FRAMEWORK_FILES + make_image.KEYMINT_FILES))
            with zipfile.ZipFile(modified) as archive:
                for name in make_image.KEYMINT_FILES:
                    self.assertEqual(archive.read(name), payloads[name], name)
                rc = archive.read(make_image.KEYMINT_RC).decode()
                contexts = archive.read("VENDOR/etc/selinux/vendor_file_contexts").decode().splitlines()
                fs_config = archive.read("META/vendor_filesystem_config.txt").decode().splitlines()
            # The APEX's service, started as it was, from the vendor partition; the user nobody
            # alone may read the host's key.
            self.assertIn("service vendor.keymint-default /vendor/bin/hw/dsa-keymint\n", rc)
            for line in ("class early_hal", "user nobody", "group nobody"):
                self.assertIn(f"    {line}\n", rc)
            # The label the APEX gave its binary, so init starts it in hal_keymint_default.
            self.assertIn("/vendor/bin/hw/dsa-keymint u:object_r:hal_keymint_default_exec:s0", contexts)
            self.assertIn("vendor/bin/hw/dsa-keymint 0 2000 755 capabilities=0x0", fs_config)
            self.assertIn("vendor/lib64/libssl.so 0 0 644 capabilities=0x0", fs_config)
            self.assertIn("vendor/etc/init/dsa-keymint.rc 0 0 644 capabilities=0x0", fs_config)
            for name in self.FRAGMENTS:
                self.assertIn("vendor/" + name.split("/", 1)[1] + " 0 0 644 capabilities=0x0", fs_config)

    def test_fr36_no_keymint_apex_is_selected_beside_the_keymint(self):
        selected = "androidboot.vendor.apex.com.android.hardware.keymint=com.android.hardware.keymint.rust_nonsecure"
        cmdline = make_image.kernel_cmdline("blkdevparts=x:1@0(misc)", keymint=True).split()
        self.assertIn("androidboot.vendor.apex.com.android.hardware.keymint=none", cmdline)
        self.assertNotIn(selected, cmdline)
        # The other selections stay.
        self.assertIn("androidboot.vendor.apex.com.android.hardware.gatekeeper=com.android.hardware.gatekeeper.nonsecure",
                      cmdline)
        # An image without the KeyMint keeps the APEX.
        self.assertIn(selected, make_image.kernel_cmdline("blkdevparts=x:1@0(misc)").split())

    def test_fr36_one_keymint_declares_each_instance(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, modified, manifest = self._image(Path(tmp))
            with zipfile.ZipFile(modified) as archive:
                names = archive.namelist()
                self.assertIn(self.APEX, names)
                self.assertEqual(make_image.vendor_vintf_problems(names, archive.read, manifest["kernel_cmdline"]), [])
                # Were the APEX still selected, it would declare the KeyMint a second time.
                apex = make_image.kernel_cmdline("blkdevparts=x:1@0(misc)")
                problems = make_image.vendor_vintf_problems(names, archive.read, apex)
            self.assertTrue(any("IKeyMintDevice/default" in p for p in problems), problems)

    def test_fr36_check_image_finds_the_keymint_in_the_image(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _, modified, manifest = self._image(root)
            with zipfile.ZipFile(modified) as archive:
                self.assertEqual(make_image.check_framework(manifest, archive.namelist(), archive.read), [])
                # The APEX selected again on the command line.
                selected = dict(manifest, kernel_cmdline=make_image.kernel_cmdline("blkdevparts=x:1@0(misc)"))
                problems = make_image.check_framework(selected, archive.namelist(), archive.read)
                self.assertTrue(any("keymint=none" in p for p in problems), problems)
                self.assertTrue(any(self.APEX in p and "active" in p for p in problems), problems)
            # An image without DSA's KeyMint, as the build made it.
            with zipfile.ZipFile(self._source(root)) as archive:
                problems = make_image.check_framework(manifest, archive.namelist(), archive.read)
            for name in make_image.KEYMINT_FILES + [make_image.KEYMINT_RC]:
                self.assertTrue(any(name in p for p in problems), name)
            self.assertTrue(any("hal_keymint_default_exec" in p for p in problems), problems)

    def test_fr36_check_image_finds_another_keymint_binary(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, modified, manifest = self._image(Path(tmp))
            changed = Path(tmp) / "changed.zip"
            with zipfile.ZipFile(modified) as source, zipfile.ZipFile(changed, "w") as out:
                for info in source.infolist():
                    data = source.read(info.filename)
                    if info.filename == make_image.KEYMINT_BINARY:
                        data = self._elf(62, self.NEEDED, b"the APEX's binary")
                    out.writestr(info, data)
            with zipfile.ZipFile(changed) as archive:
                problems = make_image.check_framework(manifest, archive.namelist(), archive.read)
            self.assertTrue(any(make_image.KEYMINT_BINARY in p and "not the framework build's" in p
                                for p in problems), problems)

    def test_fr36_a_keymint_needing_a_library_the_vendor_lacks_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            framework = self._framework(root, needed=self.NEEDED + ["libkeymint_extra.so"])
            payloads, _ = make_image.checked_framework(framework, "15581820", 16, "x86_64")
            with self.assertRaisesRegex(ValueError, "needs libkeymint_extra.so"):
                make_image.modify(self._source(root), root / "modified.zip", framework=payloads)
        self.assertEqual(make_image.elf_needed(self._elf(62, ["liba.so", "libb.so"])), ["liba.so", "libb.so"])
        self.assertIsNone(make_image.elf_needed(b"not an ELF file"))

    def test_fr36_a_framework_with_part_of_the_keymint_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            folder = self._framework(root)
            description = json.loads((folder / "framework.json").read_text())
            del description["files"]["VENDOR/lib64/libssl.so"]
            (folder / "framework.json").write_text(json.dumps(description))
            with self.assertRaisesRegex(SystemExit, "VENDOR/lib64/libssl.so"):
                make_image.checked_framework(folder, "15581820", 16, "x86_64")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaisesRegex(SystemExit, "dsa-keymint is built for ELF machine 183"):
                make_image.checked_framework(self._framework(root, machine=183), "15581820", 16, "x86_64")

    def test_fr36_the_keymint_fragments_declare_every_instance_it_registers(self):
        # kmr_hal's ALL_HALS: the service registers these four, and servicemanager refuses an
        # undeclared one.
        declared = set()
        for data in self.FRAGMENTS.values():
            declared |= make_image.vintf_instances(data)
        self.assertEqual(sorted(declared), sorted(make_image.KEYMINT_INSTANCES))
        self.assertEqual(make_image.DSA_SERVED_INSTANCES[make_image.KEYMINT_BINARY],
                         ("aidl", "android.hardware.security.keymint", "IKeyMintDevice", "default"))


class RootElevationImageTests(unittest.TestCase):
    """The guest image's side of root elevation: the su client's label, the elevation helper's
    domain and sockets, and the su guard that hides su from every app the host did not allow."""

    HERE = Path(__file__).parent
    POLICY = (Path(__file__).parent / "sepolicy" / "su.cil").read_text()
    RELAY = (Path(__file__).parents[1] / "channeld" / "src" / "su_relay.rs").read_text()

    def _modify(self, root, channeld=True, plat_file_contexts="/system/bin/sh u:object_r:shell_exec:s0\n"):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
            archive.writestr("SYSTEM/etc/selinux/plat_file_contexts", plat_file_contexts)
            archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
            archive.writestr("META/file_contexts.bin", b"\x8a\xff\x7c\xf9 compiled")
            archive.writestr("META/filesystem_config.txt", "system/bin/sh 0 0 755 capabilities=0x0\n")
            archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
        daemon = None
        client = None
        if channeld:
            daemon = root / "dsa-channeld"
            daemon.write_bytes(b"daemon")
            client = root / "su"
            client.write_bytes(b"client")
        modified = root / "modified.zip"
        make_image.modify(source, modified, channeld=daemon, su_client=client)
        return zipfile.ZipFile(modified)

    def _rules(self, text):
        """(kind, source, target, class, permissions) of every allow, dontaudit and
        typetransition rule in `text`, comments aside."""
        rules = []
        for line in text.splitlines():
            line = line.strip()
            m = re.match(r"\((allow|dontaudit)\s+(\S+)\s+(\S+)\s+\((\S+)\s+\(([^)]*)\)\)\)$", line)
            if m:
                rules.append((m.group(1), m.group(2), m.group(3), m.group(4), set(m.group(5).split())))
            m = re.match(r"\(typetransition\s+(\S+)\s+(\S+)\s+(\S+)\s+(\S+)\)$", line)
            if m:
                rules.append(("typetransition", m.group(1), m.group(2), m.group(3), {m.group(4)}))
        return rules

    def _relay_paths(self):
        """The su paths the daemon's su relay tells the kernel's su guard to watch."""
        listed = re.search(r"pub const SU_PATHS: \[&str; \d+\] = \[([^\]]*)\]", self.RELAY)
        self.assertIsNotNone(listed, "su_relay.rs no longer lists SU_PATHS")
        return re.findall(r'"(/[^"]+)"', listed.group(1))

    def test_fr35_the_image_carries_root_elevations_policy_with_the_daemon(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            names = archive.namelist()
            self.assertEqual(make_image.root_elevation_problems(names, archive.read), [])
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            for domain in ["dsa_su", "dsa_su_root"]:
                self.assertIn(f"(typeattributeset domain ({domain}))", policy)
            plat_contexts = archive.read("SYSTEM/etc/selinux/plat_file_contexts").decode().splitlines()
            self.assertIn("/system/bin/sh u:object_r:shell_exec:s0", plat_contexts)
            self.assertIn("/system/bin/su u:object_r:dsa_su_exec:s0", plat_contexts)
            # The system image is labelled from file_contexts.bin, not from plat_file_contexts.
            self.assertIn("/system/bin/su u:object_r:dsa_su_exec:s0",
                          archive.read("META/file_contexts.bin").decode().splitlines())
            self.assertIn("vendor.dsa.root_elevation u:object_r:dsa_root_elevation_prop:s0 exact bool",
                          archive.read("VENDOR/etc/selinux/vendor_property_contexts").decode().splitlines())

    def test_fr35_without_the_daemon_the_image_has_no_root_elevation(self):
        # The su client's type is declared with the daemon's policy; a label naming it in an
        # image without that policy would fail to load.
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp), channeld=False) as archive:
            policy = archive.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
            self.assertNotIn("dsa_su", policy)
            self.assertNotIn("dsa_su_exec", archive.read("SYSTEM/etc/selinux/plat_file_contexts").decode())

    def test_fr35_the_su_clients_label_replaces_one_the_build_already_has(self):
        # Two specifications for one path conflict, so the build's own line goes.
        text = make_image.update_plat_file_contexts(
            "/system/bin/sh u:object_r:shell_exec:s0\n/system/bin/su u:object_r:su_exec:s0\n")
        self.assertEqual(text.splitlines(), ["/system/bin/sh u:object_r:shell_exec:s0",
                                             "/system/bin/su u:object_r:dsa_su_exec:s0"])
        # AOSP's own su keeps its label, which apps may not run.
        kept = make_image.update_plat_file_contexts("/system/xbin/su u:object_r:su_exec:s0\n")
        self.assertIn("/system/xbin/su u:object_r:su_exec:s0", kept.splitlines())

    def test_fr35_the_su_client_is_where_the_su_guard_watches(self):
        # The guard hides only the inodes the relay names; a client anywhere else would be
        # visible to every app.
        self.assertIn(make_image.SU_CLIENT_PATH, self._relay_paths())

    def test_fr35_the_image_installs_the_unprivileged_su_client(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source.zip"
            with zipfile.ZipFile(source, "w") as archive:
                archive.writestr("SYSTEM/bin/su", b"old su")
                archive.writestr("SYSTEM/xbin/su", b"aosp setuid su")
                archive.writestr(
                    "META/filesystem_config.txt",
                    "system/bin/su 0 0 6755 capabilities=0x0\n"
                    "system/xbin 0 2000 750 capabilities=0x0\n"
                    "system/xbin/su 0 2000 4750 capabilities=0x0\n",
                )
            daemon = root / "dsa-channeld"
            client = root / "su"
            daemon.write_bytes(b"daemon")
            client.write_bytes(b"unprivileged client")
            modified = root / "modified.zip"
            make_image.modify(source, modified, channeld=daemon, su_client=client)
            with zipfile.ZipFile(modified) as archive:
                self.assertEqual(archive.read("SYSTEM/bin/su"), b"unprivileged client")
                self.assertEqual(archive.namelist().count("SYSTEM/bin/su"), 1)
                self.assertNotIn("SYSTEM/xbin/su", archive.namelist())
                config = archive.read("META/filesystem_config.txt").decode().splitlines()
                self.assertIn("system/bin/su 0 0 755 capabilities=0x0", config)
                self.assertNotIn("system/bin/su 0 0 6755 capabilities=0x0", config)
                self.assertIn("system/xbin 0 2000 755 capabilities=0x0", config)
                self.assertNotIn("system/xbin 0 2000 750 capabilities=0x0", config)
                self.assertNotIn("system/xbin/su 0 2000 4750 capabilities=0x0", config)

    def test_fr35_an_app_not_allowed_sees_no_su_from_boot(self):
        # The guard does nothing until something writes it, and the helper runs only while root
        # elevation is on. init hides every su the image can have on its own, before zygote
        # starts. A system script runs the writes as init, not vendor_init.
        rc = (self.HERE / "dsa-su-guard.rc").read_text()
        start = rc.index("on post-fs-data\n")
        block = rc[start:rc.index("\n\n", start)].splitlines()
        system_paths = [path for path in self._relay_paths() if path.startswith("/system/")]
        self.assertTrue(system_paths)
        writes = [line.strip() for line in block[1:]]
        for path in system_paths:
            self.assertIn(f'write /sys/kernel/security/dsa_su/control "path {path}"', writes)
        # Enabled last: with no uid allowed, every task gets ENOENT for the watched paths.
        self.assertEqual(writes[-1], "write /sys/kernel/security/dsa_su/control enable")
        self.assertNotIn("allow", " ".join(writes))
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            self.assertEqual(archive.read("SYSTEM/etc/init/dsa-su-guard.rc").decode(), rc)
            self.assertNotIn("/sys/kernel/security/dsa_su/control",
                             archive.read(make_image.CHANNELD_RC).decode())
            files = {name: archive.read(name) for name in archive.namelist()}
            self.assertIn("system/etc/init/dsa-su-guard.rc 0 0 644 capabilities=0x0",
                          files[make_image.SYSTEM_FS_CONFIG].decode().splitlines())
            del files[make_image.SU_GUARD_RC]
            self.assertTrue(any("su is not hidden at boot" in problem for problem in
                                make_image.root_elevation_problems(list(files), files.__getitem__)))

    def test_fr35_the_helper_runs_only_while_root_elevation_is_on(self):
        rc = (self.HERE / "dsa-channeld.rc").read_text()
        start = rc.index(make_image.SU_HELPER_SERVICE)
        service = rc[start:rc.index("\n\n", start)].splitlines()
        options = [line.strip() for line in service[1:]]
        self.assertIn("user root", options)
        self.assertIn("seclabel u:r:dsa_su:s0", options)
        self.assertIn("disabled", options)
        # The sockets carry the helper's own context (init's default), which apps and the daemon
        # may connect to; their files take the labels vendor_file_contexts gives their paths.
        self.assertIn("socket dsa_su stream 0666 root root", options)
        self.assertIn("socket dsa_su_relay seqpacket 0660 root system", options)
        contexts = make_image.update_vendor_file_contexts("", channeld=True).splitlines()
        self.assertIn("/dev/socket/dsa_su u:object_r:dsa_su_socket:s0", contexts)
        self.assertIn("/dev/socket/dsa_su_relay u:object_r:dsa_su_relay_socket:s0", contexts)
        self.assertNotIn("dsa_su", make_image.update_vendor_file_contexts("", channeld=False))
        self.assertEqual(Path(make_image.SU_SOCKET).name, "dsa_su")
        self.assertEqual(Path(make_image.SU_RELAY_SOCKET).name, "dsa_su_relay")
        self.assertIn("on property:vendor.dsa.root_elevation=1\n    start dsa_su", rc)
        self.assertIn("on property:vendor.dsa.root_elevation=0\n    stop dsa_su", rc)
        # Nothing else starts it.
        self.assertEqual(rc.count("start dsa_su\n"), 1)

    def test_fr35_apps_reach_the_helper_only_through_its_socket(self):
        apps = {"appdomain", "untrusted_app_all", "untrusted_app", "platform_app", "priv_app", "system_app"}
        granted = {(target, cls, frozenset(perms)) for kind, source, target, cls, perms in self._rules(self.POLICY)
                   if kind == "allow" and source in apps}
        self.assertEqual(granted, {
            # Running the su client, in the app's own domain.
            ("dsa_su_exec", "file", frozenset({"read", "open", "getattr", "map", "execute", "execute_no_trans"})),
            # Its socket, and the helper behind it.
            ("dsa_su_socket", "sock_file", frozenset({"write"})),
            ("dsa_su", "unix_stream_socket", frozenset({"connectto"})),
        })
        # No app changes domain by running su: it gains nothing by itself.
        for kind, source, target, cls, perms in self._rules(self.POLICY):
            if kind == "typetransition":
                self.assertNotIn(source, apps)

    def test_fr35_adb_shell_asks_for_root_like_an_app(self):
        # adb shell gets exactly what an app gets: the su client, its socket and the helper.
        granted = {(target, cls, frozenset(perms)) for kind, source, target, cls, perms in self._rules(self.POLICY)
                   if kind == "allow" and source == "shell"}
        self.assertEqual(granted, {
            ("dsa_su_exec", "file", frozenset({"read", "open", "getattr", "map", "execute", "execute_no_trans"})),
            ("dsa_su_socket", "sock_file", frozenset({"write"})),
            ("dsa_su", "unix_stream_socket", frozenset({"connectto"})),
        })
        for kind, source, target, cls, perms in self._rules(self.POLICY):
            if kind == "typetransition":
                self.assertNotEqual(source, "shell")

    def test_fr35_only_the_helper_enters_the_root_domain(self):
        rules = self._rules(self.POLICY)
        entries = {source for kind, source, target, cls, perms in rules
                   if (kind == "typetransition" and perms == {"dsa_su_root"})
                   or (kind == "allow" and target == "dsa_su_root" and cls == "process"
                       and perms & {"transition", "dyntransition"})}
        self.assertEqual(entries, {"dsa_su"})
        # Only through a shell, which runs the approved command.
        self.assertIn(("typetransition", "dsa_su", "shell_exec", "process", {"dsa_su_root"}), rules)
        self.assertIn("(typepermissive dsa_su_root)", self.POLICY)
        # The helper itself stays enforcing.
        self.assertNotIn("(typepermissive dsa_su)", self.POLICY)

    def test_fr35_only_init_and_the_helper_write_the_su_guard(self):
        writers = {source for kind, source, target, cls, perms in self._rules(self.POLICY)
                   if kind == "allow" and target == "dsa_su_control_file" and cls == "file"
                   and perms & {"write", "append"}}
        self.assertEqual(writers, {"init", "dsa_su"})
        self.assertIn('(genfscon securityfs "/dsa_su" (u object_r dsa_su_control_file ((s0) (s0))))', self.POLICY)

    def test_fr35_only_the_daemon_reaches_the_helpers_link_to_the_host(self):
        rules = self._rules(self.POLICY)
        relay = {source for kind, source, target, cls, perms in rules
                 if kind == "allow" and target == "dsa_su_relay_socket"}
        self.assertEqual(relay, {"dsa_channeld"})
        setters = {source for kind, source, target, cls, perms in rules
                   if kind == "allow" and target == "dsa_root_elevation_prop" and cls == "property_service"}
        self.assertEqual(setters, {"dsa_channeld"})

    def test_fr35_the_helper_holds_no_part_of_the_control_channel(self):
        # The host's decision reaches the helper only through the daemon: the helper may not
        # read the boot's secret or open a socket to the host itself.
        for kind, source, target, cls, perms in self._rules(self.POLICY):
            if kind == "allow" and source == "dsa_su":
                self.assertNotEqual(cls, "vsock_socket")
                self.assertNotEqual(target, "proc_bootconfig")
                self.assertNotIn("dsa_channel_service", target)

    def test_fr35_check_image_finds_what_root_elevation_lacks(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            files = {name: archive.read(name) for name in archive.namelist()}
        policy = files["SYSTEM/etc/selinux/plat_sepolicy.cil"].decode()
        # A rule gone, another domain let into the root domain, another writer of the guard.
        files["SYSTEM/etc/selinux/plat_sepolicy.cil"] = (
            policy.replace("(allow untrusted_app_all dsa_su (unix_stream_socket (connectto)))\n", "")
            + "(typetransition untrusted_app shell_exec process dsa_su_root)\n"
            + "(allow dsa_channeld dsa_su_control_file (file (write open)))\n"
            + "(allow system_server dsa_root_elevation_prop (property_service (set)))\n"
        ).encode()
        files["META/file_contexts.bin"] = b"/system/bin/sh u:object_r:shell_exec:s0\n"
        del files["SYSTEM/bin/su"]
        files["SYSTEM/xbin/su"] = b"aosp su"
        files[make_image.SYSTEM_FS_CONFIG] = (
            b"system/xbin 0 2000 750 capabilities=0x0\n"
            b"system/xbin/su 0 2000 4750 capabilities=0x0\n"
        )
        rc = files[make_image.CHANNELD_RC].decode()
        files[make_image.CHANNELD_RC] = "\n".join(
            line for line in rc.splitlines() if line != "    seclabel u:r:dsa_su:s0"
        ).encode()
        problems = make_image.root_elevation_problems(list(files), files.__getitem__)
        expected = [
            "SYSTEM/bin/su is missing",
            "SYSTEM/xbin/su is in the image",
            "apps cannot connect to the elevation helper",
            "untrusted_app may enter dsa_su_root",
            "dsa_channeld may write the su guard",
            "system_server may start the elevation helper",
            "META/file_contexts.bin does not label /system/bin/su dsa_su_exec",
            'lacks "seclabel u:r:dsa_su:s0"',
            "does not make system/xbin mode 755",
            "still lists system/xbin/su",
        ]
        for text in expected:
            self.assertTrue(any(text in p for p in problems), (text, problems))
        self.assertEqual(len(problems), len(expected), problems)

    def test_fr35_the_policy_has_only_rules_the_tests_can_read(self):
        # Every statement is one the checks above parse, so none slips past them.
        for line in self.POLICY.splitlines():
            line = line.strip()
            if not line or line.startswith(";"):
                continue
            self.assertTrue(
                re.match(r"\((type|roletype|typeattributeset|typepermissive|genfscon)\s", line)
                or self._rules(line),
                line,
            )


class KeyMintKeyImageTests(unittest.TestCase):
    """The guest image's side of KeyMint's root key from the host: the daemon's mode that
    fetches it once each boot, the tmpfs folder it goes to, and the policy that lets only
    KeyMint read it."""

    HERE = Path(__file__).parent
    POLICY = (Path(__file__).parent / "sepolicy" / "channeld.cil").read_text()

    def _modify(self, root):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
            archive.writestr("SYSTEM/etc/selinux/plat_file_contexts", "/system/bin/sh u:object_r:shell_exec:s0\n")
            archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
            archive.writestr("META/file_contexts.bin", b"\x8a\xff\x7c\xf9 compiled")
            archive.writestr("META/filesystem_config.txt", "system/bin/sh 0 0 755 capabilities=0x0\n")
            archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
        daemon = root / "dsa-channeld"
        daemon.write_bytes(b"daemon")
        client = root / "su"
        client.write_bytes(b"client")
        modified = root / "modified.zip"
        make_image.modify(source, modified, channeld=daemon, su_client=client)
        return zipfile.ZipFile(modified)

    def _block(self, text, first_line):
        start = text.index(first_line)
        end = text.find("\n\n", start)
        return [line.strip() for line in text[start:end if end >= 0 else len(text)].splitlines()]

    def test_fr36_keymint_root_key_is_fetched_once_before_keymint_starts(self):
        rc = (self.HERE / "dsa-channeld.rc").read_text()
        service = self._block(rc, make_image.KEYMINT_KEY_SERVICE)
        for option in ["user nobody", "group nobody", "seclabel u:r:dsa_keymint_key:s0", "oneshot", "disabled"]:
            self.assertIn(option, service)
        script = (self.HERE / "dsa-keymint-key.rc").read_text()
        # On init: before late-fs, where init starts KeyMint with class early_hal.
        block = self._block(script, "on init\n")
        self.assertEqual(block[1:], ["mkdir /dev/dsa_keymint 0700 nobody nobody", "start dsa_keymint_key"])
        self.assertNotIn("/data", script + "\n".join(service))
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            names = archive.namelist()
            self.assertEqual(make_image.keymint_key_problems(names, archive.read), [])
            self.assertEqual(archive.read(make_image.KEYMINT_KEY_RC).decode(), script)
            self.assertIn("system/etc/init/dsa-keymint-key.rc 0 0 644 capabilities=0x0",
                          archive.read(make_image.SYSTEM_FS_CONFIG).decode().splitlines())
            self.assertIn(make_image.KEYMINT_KEY_CONTEXT,
                          archive.read("VENDOR/etc/selinux/vendor_file_contexts").decode().splitlines())

    def test_fr36_only_keymint_reads_its_root_key(self):
        readers = re.findall(r"\(allow\s+(\S+)\s+dsa_keymint_key_file\s+\(\s*file\s+\(([^)]*)\)", self.POLICY)
        self.assertTrue(readers)
        self.assertEqual({who for who, perms in readers if "read" in perms.split()}, {"hal_keymint_server"})
        writers = {who for who, perms in readers if {"write", "create"} & set(perms.split())}
        self.assertEqual(writers, {"dsa_keymint_key"})
        # The daemon proper, which talks to every client in the guest, never holds the key.
        self.assertNotRegex(self.POLICY, r"\(allow\s+dsa_channeld\s+dsa_keymint_key_file")
        self.assertNotIn("dsa_keymint", make_image.update_vendor_file_contexts("", channeld=False))
        self.assertIn(make_image.KEYMINT_KEY_CONTEXT,
                      make_image.update_vendor_file_contexts("", channeld=True).splitlines())

    def test_fr36_check_image_names_what_the_root_key_lacks(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            files = {name: archive.read(name) for name in archive.namelist()}
        del files[make_image.KEYMINT_KEY_RC]
        rc = files[make_image.CHANNELD_RC].decode()
        files[make_image.CHANNELD_RC] = "\n".join(
            line for line in rc.splitlines() if line != "    seclabel u:r:dsa_keymint_key:s0").encode()
        files["SYSTEM/etc/selinux/plat_sepolicy.cil"] += (
            b"\n(allow untrusted_app dsa_keymint_key_file (file (read open)))\n")
        files["VENDOR/etc/selinux/vendor_file_contexts"] = b"/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n"
        problems = make_image.keymint_key_problems(list(files), files.__getitem__)
        expected = [
            'lacks "seclabel u:r:dsa_keymint_key:s0"',
            "dsa-keymint-key.rc is missing",
            "vendor_file_contexts lacks",
            "lets untrusted_app read KeyMint's root key",
        ]
        for text in expected:
            self.assertTrue(any(text in p for p in problems), (text, problems))
        self.assertEqual(len(problems), len(expected), problems)


class HostDnsTests(unittest.TestCase):
    """The guest's DNS, answered by the host's resolver: the daemon's stub, the root helper that
    sends every DNS query to it, and the property that switches the helper."""

    HERE = Path(__file__).parent
    POLICY = (Path(__file__).parent / "sepolicy" / "channeld.cil").read_text()

    def _modify(self, root):
        source = root / "target-files.zip"
        with zipfile.ZipFile(source, "w") as archive:
            archive.writestr("SYSTEM/etc/selinux/plat_sepolicy.cil", "(type init)\n")
            archive.writestr("SYSTEM/etc/selinux/plat_file_contexts", "/system/bin/sh u:object_r:shell_exec:s0\n")
            archive.writestr("VENDOR/etc/selinux/vendor_file_contexts", "/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n")
            archive.writestr("META/file_contexts.bin", b"\x8a\xff\x7c\xf9 compiled")
            archive.writestr("META/filesystem_config.txt", "system/bin/sh 0 0 755 capabilities=0x0\n")
            archive.writestr("META/vendor_filesystem_config.txt", "vendor/bin/sh 0 2000 755 capabilities=0x0\n")
        daemon = root / "dsa-channeld"
        daemon.write_bytes(b"daemon")
        client = root / "su"
        client.write_bytes(b"client")
        modified = root / "modified.zip"
        make_image.modify(source, modified, channeld=daemon, su_client=client)
        return zipfile.ZipFile(modified)

    def _block(self, text, first_line):
        start = text.index(first_line)
        end = text.find("\n\n", start)
        return [line.strip() for line in text[start:end if end >= 0 else len(text)].splitlines()]

    def test_fr28_the_dns_redirect_runs_as_root_in_its_own_domain_when_the_daemon_switches_it(self):
        rc = (self.HERE / "dsa-channeld.rc").read_text()
        service = self._block(rc, make_image.DNS_REDIRECT_SERVICE)
        for option in ["user root", "seclabel u:r:dsa_dns_redirect:s0", "oneshot", "disabled",
                       "capabilities NET_ADMIN NET_RAW"]:
            self.assertIn(option, service)
        trigger = self._block(rc, make_image.DNS_REDIRECT_TRIGGER + "\n")
        self.assertEqual(trigger[1:], ["start dsa_dns_redirect"])
        # vendor_init may not read a service's state, and init drops an action that triggers
        # on one ("unexported property trigger found: init.svc.netd"), so the redirect never ran.
        for line in rc.splitlines():
            if line.startswith("on "):
                self.assertNotIn("property:init.svc.", line)

    def test_fr28_adb_shell_reads_whether_the_host_answers_dns(self):
        # The DNS boot check reads vendor.dsa.dns with getprop; a refused read reads as "".
        self.assertRegex(self.POLICY, r"\(allow\s+shell\s+dsa_dns_prop\s+\(\s*file\s+\(\s*read\b")

    def test_fr28_only_the_daemon_switches_the_dns_redirect(self):
        setters = re.findall(r"\(allow\s+(\S+)\s+dsa_dns_prop\s+\(\s*property_service\s+\(([^)]*)\)",
                             self.POLICY)
        self.assertEqual({who for who, perms in setters if "set" in perms.split()}, {"dsa_channeld"})
        # The helper may change the packet filter, and nothing on the disk but what it runs.
        self.assertRegex(self.POLICY, r"\(allow\s+dsa_dns_redirect\s+self\s+\(\s*capability\s+\([^)]*\bnet_admin\b")
        self.assertNotRegex(self.POLICY, r"\(allow\s+dsa_dns_redirect\s+\S*data\S*")
        # The daemon listens for the guest's queries on the loopback address.
        for cls in ("udp_socket", "tcp_socket"):
            self.assertRegex(self.POLICY, rf"\(allow\s+dsa_channeld\s+port\s+\(\s*{cls}\s+\(\s*name_bind")
        self.assertIn(make_image.DNS_PROPERTY_CONTEXT,
                      make_image.update_vendor_property_contexts("").splitlines())

    def test_fr28_check_image_names_what_the_hosts_dns_lacks(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            files = {name: archive.read(name) for name in archive.namelist()}
        self.assertEqual(make_image.dns_problems(list(files), files.__getitem__), [])
        rc = files[make_image.CHANNELD_RC].decode()
        files[make_image.CHANNELD_RC] = "\n".join(
            line for line in rc.splitlines() if line != "    seclabel u:r:dsa_dns_redirect:s0").encode()
        files[make_image.VENDOR_PROPERTY_CONTEXTS] = b""
        files[make_image.CHANNELD_RC] += b"\n\non property:sys.boot_completed=1 && property:init.svc.netd=running\n"
        problems = make_image.dns_problems(list(files), files.__getitem__)
        expected = ['lacks "seclabel u:r:dsa_dns_redirect:s0"', f"does not give {make_image.DNS_PROPERTY}",
                    "unexported property trigger"]
        for text in expected:
            self.assertTrue(any(text in p for p in problems), (text, problems))
        self.assertEqual(len(problems), len(expected), problems)


class HostCertificatesImageTests(KeyMintKeyImageTests):
    """The guest image's side of the root certificates a user or administrator added to the
    host: the tmpfs copies of Android's certificate folders bound over the originals before the
    zygote starts, the setup that copies them, and the policy that lets only the daemon write
    there and every domain read."""

    CACERTS_POLICY = (Path(__file__).parent / "sepolicy" / "cacerts.cil").read_text()

    # The key's tests run once, in the class they belong to.
    test_fr36_keymint_root_key_is_fetched_once_before_keymint_starts = None
    test_fr36_only_keymint_reads_its_root_key = None
    test_fr36_check_image_names_what_the_root_key_lacks = None

    def test_fr28_the_host_certificates_folders_are_bound_before_the_zygote_starts(self):
        rc = (self.HERE / "dsa-channeld.rc").read_text()
        service = self._block(rc, make_image.CACERTS_SERVICE)
        for option in ["user system", "group system", "oneshot", "disabled"]:
            self.assertIn(option, service)
        script = (self.HERE / "dsa-cacerts.rc").read_text()
        block = self._block(script, "on post-fs-data\n")
        self.assertEqual(block, [line.strip() for line in make_image.CACERTS_SCRIPT])
        # The copy is made before either bind, so the binds find whole folders.
        self.assertLess(script.index("exec_start dsa_cacerts_setup"), script.index("    mount none"))
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            names = archive.namelist()
            self.assertEqual(make_image.cacerts_problems(names, archive.read), [])
            self.assertEqual(archive.read(make_image.CACERTS_RC).decode(), script)
            self.assertIn("system/etc/init/dsa-cacerts.rc 0 0 644 capabilities=0x0",
                          archive.read(make_image.SYSTEM_FS_CONFIG).decode().splitlines())
            self.assertIn(make_image.CACERTS_CONTEXT,
                          archive.read("VENDOR/etc/selinux/vendor_file_contexts").decode().splitlines())

    def test_fr28_apps_read_the_certificate_folder_android_chooses(self):
        # The bind under /apex reaches the zygote's mount namespace, so apps read the copy there
        # as Android chooses, and no VM is told to read the folder under /system instead.
        self.assertIn("    mount none /dev/dsa_cacerts/conscrypt /apex/com.android.conscrypt/cacerts bind",
                      make_image.CACERTS_SCRIPT)
        for vendor in (False, True):
            props = make_image.edit_props("ro.build.type=user\n", vendor=vendor)
            self.assertNotIn("system.certs.enabled", props)

    def test_fr28_only_the_daemon_writes_androids_system_certificates(self):
        rules = re.findall(r"\(allow\s+(\S+)\s+dsa_cacerts_file\s+\(\s*file\s+\(([^)]*)\)", self.CACERTS_POLICY)
        writers = {who for who, perms in rules if {"write", "create", "append", "unlink"} & set(perms.split())}
        self.assertEqual(writers, {"dsa_channeld"})
        readers = {who for who, perms in rules if "read" in perms.split()}
        self.assertIn("domain", readers, "every process that checks a certificate reads them")
        for rule in make_image.CACERTS_RULES:
            self.assertIn(rule, self.CACERTS_POLICY)
        self.assertNotIn("dsa_cacerts", make_image.update_vendor_file_contexts("", channeld=False))
        self.assertIn(make_image.CACERTS_CONTEXT,
                      make_image.update_vendor_file_contexts("", channeld=True).splitlines())

    def test_fr28_check_image_names_what_the_host_certificates_lack(self):
        with tempfile.TemporaryDirectory() as tmp, self._modify(Path(tmp)) as archive:
            files = {name: archive.read(name) for name in archive.namelist()}
        del files[make_image.CACERTS_RC]
        files["SYSTEM/etc/selinux/plat_sepolicy.cil"] += (
            b"\n(allow untrusted_app dsa_cacerts_file (file (write open)))\n")
        files["VENDOR/etc/selinux/vendor_file_contexts"] = b"/vendor/bin/sh u:object_r:vendor_shell_exec:s0\n"
        problems = make_image.cacerts_problems(list(files), files.__getitem__)
        expected = [
            "dsa-cacerts.rc is missing",
            "vendor_file_contexts lacks",
            "lets untrusted_app write Android's system certificates",
        ]
        for text in expected:
            self.assertTrue(any(text in p for p in problems), (text, problems))
        self.assertEqual(len(problems), len(expected), problems)


INSETS_POLICY = "Lcom/android/server/wm/InsetsPolicy;->enforceInsetsPolicyForTarget"
CAPTION_BAR = "Landroid/view/WindowInsets$Type;->captionBar"
SYSTEM_BARS = "Landroid/view/WindowInsets$Type;->systemBars"
IS_FLOATING = "Landroid/app/WindowConfiguration;->isFloating"


def services_dex(caption_test_before=False, floating=True):
    """InsetsPolicy.enforceInsetsPolicyForTarget of services.jar as d8 compiles Android 17's,
    cut down to the calls that matter: the windowing mode, isFloating, then the types a
    floating window keeps (captionBar(), and ime()). With `caption_test_before`, it first tests
    a source's type against captionBar(), as a later Android does for non-fullscreen windows."""
    methods = [
        ("Lcom/android/server/wm/InsetsPolicy;", "enforceInsetsPolicyForTarget", "LLL"),
        ("Landroid/view/WindowInsets$Type;", "captionBar", "I"),
        ("Landroid/view/WindowInsets$Type;", "systemBars", "I"),
        ("Lcom/android/server/wm/ConfigurationContainer;", "getWindowingMode", "I"),
        ("Landroid/app/WindowConfiguration;", "isFloating", "ZI"),
        ("Landroid/view/WindowInsets$Type;", "ime", "I"),
    ]
    code = ""
    if caption_test_before:
        code += "71000100 0000" "0a00"  # invoke-static {}, captionBar; move-result v0
    code += "71000200 0000" "0a00"  # invoke-static {}, systemBars (the provided insets' types)
    code += "6e100300 0300" "0a00"  # invoke-virtual {v3}, getWindowingMode; move-result v0
    if floating:
        code += "71100400 0000" "0a01"  # invoke-static {v0}, isFloating; move-result v1
    code += "38010900"  # if-eqz v1, :keep
    code += "71000100 0000" "0a00"  # invoke-static {}, captionBar; move-result v0
    code += "71000500 0000" "0a01"  # invoke-static {}, ime; move-result v1
    code += "0f00"  # :keep return v0
    return dex_file(methods, {0: (4, bytes.fromhex(code))})


def dex_calls(dex, method):
    """The methods `method` calls, in order."""
    defined, refs = make_image.dex_methods(dex)
    (code_off, _), = defined[method]
    return [refs[struct.unpack_from("<H", dex, at + 2)[0]]
            for at, op in make_image.dex_instructions(dex, code_off) if op in make_image.DEX_INVOKES]


def jar_of(dex, misaligned=True):
    """A jar with `dex` stored as classes.dex, as the build stores a system server jar's dex,
    after a stored member whose length leaves the dex unaligned when `misaligned`."""
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w") as jar:
        jar.writestr("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n" + (b"x" if misaligned else b""))
        jar.writestr("classes.dex", dex)
        jar.writestr("com/android/server/x.xml", b"<x/>" * 50, compress_type=zipfile.ZIP_DEFLATED)
    return out.getvalue()


class CaptionIsStatusBarTests(unittest.TestCase):
    """The host window's title bar is Android's status bar: an app that pads by its status bar
    inset, or by the framework's status bar height, keeps its content below the title bar."""

    def test_fr22_a_freeform_window_receives_its_status_bar_inset(self):
        # Android keeps only the caption bar (and the IME) of a floating window's insets, so
        # the status bar source the agent gives a task never reached its app, and an app that
        # pads by its status bar inset drew its toolbar under the title bar.
        self.assertEqual(dex_calls(services_dex(), INSETS_POLICY)[-2:], [CAPTION_BAR, "Landroid/view/WindowInsets$Type;->ime"])
        edited, changed = make_image.edit_dex(services_dex(), make_image.SERVICES_CODE)
        self.assertEqual(changed, [INSETS_POLICY])
        calls = dex_calls(edited, INSETS_POLICY)
        self.assertEqual(calls[calls.index(IS_FLOATING) + 1:],
                         [SYSTEM_BARS, "Landroid/view/WindowInsets$Type;->ime"])
        self.assertNotIn(CAPTION_BAR, calls)
        # Only the method the instruction names changes: the file is as long, the code as long.
        self.assertEqual(len(edited), len(services_dex()))
        self.assertEqual(len(dex_code(edited, INSETS_POLICY)), len(dex_code(services_dex(), INSETS_POLICY)))
        # Applied again, it changes nothing.
        self.assertEqual(make_image.edit_dex(edited, make_image.SERVICES_CODE)[1], [])

    def test_fr22_only_the_types_a_floating_window_keeps_change(self):
        # A later Android tests a source's type against captionBar() before isFloating: that
        # test stays as it is.
        edited, changed = make_image.edit_dex(services_dex(caption_test_before=True), make_image.SERVICES_CODE)
        self.assertEqual(changed, [INSETS_POLICY])
        calls = dex_calls(edited, INSETS_POLICY)
        self.assertEqual(calls[0], CAPTION_BAR)
        self.assertEqual(calls[calls.index(IS_FLOATING) + 1], SYSTEM_BARS)

    def test_fr22_an_insets_policy_built_from_other_sources_stops_the_image(self):
        with self.assertRaisesRegex(ValueError, "built from other sources"):
            make_image.edit_dex(services_dex(floating=False), make_image.SERVICES_CODE)

    def test_fr22_the_edited_services_jar_keeps_its_dex_stored_and_aligned(self):
        jar = jar_of(services_dex())
        self.assertEqual(make_image.zip_misaligned(jar), ["classes.dex"])
        self.assertTrue(make_image.apk_edits_left(jar, code=make_image.SERVICES_CODE))
        edited = make_image.edit_services_jar(jar, make_image.SERVICES_JAR)
        self.assertEqual(make_image.zip_misaligned(edited), [])
        self.assertEqual(make_image.apk_edits_left(edited, code=make_image.SERVICES_CODE), [])
        with zipfile.ZipFile(io.BytesIO(jar)) as before, zipfile.ZipFile(io.BytesIO(edited)) as after:
            self.assertEqual([i.filename for i in before.infolist()], [i.filename for i in after.infolist()])
            self.assertEqual([i.compress_type for i in before.infolist()], [i.compress_type for i in after.infolist()])
            self.assertEqual(after.read("com/android/server/x.xml"), before.read("com/android/server/x.xml"))
        # A jar that has the edit is left as it is.
        self.assertIs(make_image.edit_services_jar(edited, make_image.SERVICES_JAR), edited)

    def test_fr22_the_image_edits_services_jar_and_compiles_it_again(self):
        jar = jar_of(services_dex())
        odex, vdex = make_image.oat_files(make_image.SERVICES_JAR)
        self.assertEqual(odex, make_image.FRAMEWORK_ODEX)
        context = "PCL[/system/framework/com.android.location.provider.jar*1880069816]"
        published = {
            make_image.SERVICES_JAR: jar,
            odex: fake_odex(**{"bootclasspath": BOOTCLASSPATH, "bootclasspath-checksums": "i;2/cb8fc4a6:i;2/f8c5165e",
                               "classpath": context, "compiler-filter": "speed-profile"}),
            vdex: fake_vdex(make_image.apk_dex_checksums(jar)),
            odex[:-len(".odex")] + ".art": b"art",
        }
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "target-files.zip"
            with zipfile.ZipFile(source, "w") as archive:
                for name, data in published.items():
                    archive.writestr(name, data)
            make_image.modify(source, root / "modified.zip")
            with zipfile.ZipFile(root / "modified.zip") as archive:
                files = {name: archive.read(name) for name in archive.namelist()}
        edited = files[make_image.SERVICES_JAR]
        self.assertEqual(make_image.apk_edits_left(edited, code=make_image.SERVICES_CODE), [])
        self.assertEqual(make_image.zip_misaligned(edited), [])
        record = json.loads(files[make_image.PREBUILT_OAT_RECORD])
        self.assertEqual(record[make_image.SERVICES_JAR]["filter"], "speed-profile")
        self.assertEqual(record[make_image.SERVICES_JAR]["classpath"], context)
        # Its published odex was made for the unedited dex, so it is compiled again with the
        # published build's filter, class loader context and app image.
        self.assertEqual(make_image.compile_jobs(files, files.__getitem__),
                         [(make_image.SERVICES_JAR, "speed-profile", context, True)])
        self.assertIn(make_image.SERVICES_JAR, make_image.EDITED_JARS)
        # Until it is, the image check says so.
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            for name, data in files.items():
                archive.writestr(name, data)
        with zipfile.ZipFile(io.BytesIO(buffer.getvalue())) as archive:
            self.assertTrue(any("dex checksums" in p for p in make_image.prebuilt_oat_problems(archive)))

    def test_fr22_services_jar_is_compiled_with_the_published_builds_own_profile(self):
        lock = json.loads((Path(make_image.__file__).parent.parent / "framework/framework-lock.json").read_text())
        commits = {v["build_id"]: next(p["commit"] for p in v["projects"] if p["path"] == "frameworks/base")
                   for v in lock["versions"]}
        inputs = json.loads((Path(make_image.__file__).parent / "inputs.json").read_text())["images"]
        for image in inputs:
            base = f"https://android.googlesource.com/platform/frameworks/base/+/{commits[image['build_id']]}/services/"
            for key, file in [("services_profile", "art-profile"), ("services_profile_extra", "art-profile-extra")]:
                entry = image["files"][key]
                self.assertEqual(entry["url"], base + file + "?format=TEXT", image["version"])
                self.assertEqual(entry["encoding"], "base64")
                self.assertRegex(entry["sha256"], r"^[0-9a-f]{64}$")
                self.assertGreater(entry["size"], 0)
        fetch = (Path(make_image.__file__).parent / "fetch.sh").read_text()
        self.assertIn(".url // empty", fetch)
        self.assertIn('base64 -d "$file.download"', fetch)
        with tempfile.TemporaryDirectory() as tmp:
            spec = make_image.image_spec(inputs, 17)
            with self.assertRaisesRegex(ValueError, "fetch.sh"):
                make_image.services_profiles(spec, Path(tmp))
            for key in ("services_profile", "services_profile_extra"):
                (Path(tmp) / spec["files"][key]["name"]).write_text(key)
            self.assertEqual(make_image.services_profiles(spec, Path(tmp)), {make_image.SERVICES_JAR: [
                Path(tmp) / "services-art-profile", Path(tmp) / "services-art-profile-extra"]})

    def test_fr22_the_status_bar_height_is_the_title_bars_at_each_density(self):
        # Android draws at 131 x the monitor's scale, as the host's density rule says.
        self.assertEqual([make_image.status_bar_density(d) for d in (96, 120, 144, 168, 192, 240)],
                         [130, 164, 196, 228, 262, 328])
        heights = make_image.status_bar_heights()
        self.assertEqual(len(heights), len(make_image.STATUS_BAR_TITLE_HEIGHTS))
        for dpi, px in make_image.STATUS_BAR_TITLE_HEIGHTS.items():
            density = make_image.status_bar_density(dpi)
            # As getDimensionPixelSize rounds the dimension the overlay writes.
            dp = float(f"{heights[density]:.4f}")
            self.assertEqual(int(dp * density / 160 + 0.5), px, dpi)
            self.assertTrue(34 < dp < 39, (dpi, dp))
        # A 200% monitor: a 58 px title bar, the statusBars inset the owner's PC showed.
        self.assertEqual(make_image.STATUS_BAR_TITLE_HEIGHTS[192], 58)
        files = make_image.status_bar_overlay_files()
        self.assertIn('<dimen name="status_bar_height">35.4198dp</dimen>', files["res/values-262dpi/dsa_status_bar.xml"])
        self.assertIn(f'<dimen name="status_bar_height_default">{max(heights.values()):.4f}dp</dimen>',
                      files["res/values/dsa_status_bar.xml"])
        for name in make_image.STATUS_BAR_DIMENS:
            self.assertIn(f"dimen/{name}", make_image.framework_overlaid())
            for path, text in files.items():
                self.assertIn(f'<dimen name="{name}">', text, path)

    def test_fr22_check_image_rejects_a_status_bar_height_that_is_not_the_title_bars(self):
        self.assertEqual(make_image.status_bar_overlay_problems(status_bar_table(make_image.status_bar_heights())), [])
        problems = make_image.status_bar_overlay_problems(status_bar_table({0: 24}))
        self.assertTrue(any("status_bar_height has no value for 262 dpi" in p for p in problems), problems)
        self.assertTrue(any("not the title bar's" in p for p in problems), problems)
        phone = {density: 24 for density in make_image.status_bar_heights()}
        problems = make_image.status_bar_overlay_problems(status_bar_table(phone))
        self.assertIn("status_bar_height is 24 (unit 1) at 262 dpi, not the title bar's 58 px there", problems)

    def test_fr22_check_image_checks_services_jar(self):
        script = (Path(make_image.__file__).parent / "check-image.py").read_text()
        self.assertIn("apk_edits_left(services, code=make_image.SERVICES_CODE)", script)
        self.assertIn("zip_misaligned(services)", script)


if __name__ == "__main__":
    unittest.main()
