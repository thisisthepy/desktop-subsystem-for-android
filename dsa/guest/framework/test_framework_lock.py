"""Tests for the framework build's lock, its patches and the tools around them: the lock pins
the published build each image is made from, the patches cover what the lock fetches and
carry the shared recovery policy, the patches apply exactly or not at all, and the build's
files are collected for the image they belong to."""

import copy
import hashlib
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import framework_lock  # noqa: E402

REPO = HERE.parents[2]


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


apply_patches = load_module("apply_patches", HERE / "apply-patches.py")
collect = load_module("collect", HERE / "collect.py")
fetch_source = load_module("fetch_source", REPO / ".github/scripts/translator/fetch-source.py")

COMMIT = "0123456789abcdef0123456789abcdef01234567"
OTHER = "fedcba9876543210fedcba9876543210fedcba98"


def git(*args, cwd):
    subprocess.run(["git", "-c", "user.email=t@example.invalid", "-c", "user.name=t", *args],
                   cwd=cwd, check=True, capture_output=True)


def patched_paths(patch):
    """The files a patch changes or adds, from its +++ lines."""
    return [line[len("+++ b/"):] for line in patch.read_text().splitlines() if line.startswith("+++ b/")]


class LockTests(unittest.TestCase):
    def setUp(self):
        self.lock = framework_lock.load()
        self.images = json.loads(framework_lock.INPUTS.read_text())["images"]

    def test_nfr5_the_lock_pins_the_build_every_image_is_made_from(self):
        self.assertEqual(sorted(v["android"] for v in self.lock["versions"]), ["android16", "android17"])
        for version in self.lock["versions"]:
            images = [i for i in self.images if i["android_major"] == version["android_major"]]
            self.assertEqual(sorted(i.get("arch", "x86_64") for i in images), ["arm64", "x86_64"])
            for image in images:
                self.assertEqual(image["build_id"], version["build_id"], version["android"])
            self.assertTrue(version["manifest"]["url"].endswith(f"manifest_{version['build_id']}.xml"))
            for project in version["projects"]:
                self.assertRegex(project["commit"], r"^[0-9a-f]{40}$", project["path"])
                self.assertTrue(project["url"].startswith("https://android.googlesource.com/"),
                                project["path"])

    def test_nfr5_the_lock_names_the_builds_own_release_and_toolchains(self):
        # The images' SYSTEM/etc/build_flags.json name these (RELEASE_BUILD_CLANG_VERSION, and
        # the release of the build IDs BP4A and CP2A).
        pins = {v["android"]: (v["release"], v["clang"]) for v in self.lock["versions"]}
        self.assertEqual(pins["android16"], ("bp4a", "clang-r563880c"))
        self.assertEqual(pins["android17"], ("cp2a", "clang-r584948b"))

    def test_nfr5_apex_and_boot_jar_modules_the_build_never_uses_are_left_out(self):
        # Their Android.bp files need projects the tree leaves out (system/sepolicy's APEX file
        # contexts, the boot jars), and stopped the translator build's analysis; nothing the
        # framework build makes comes from them.
        wanted = {
            "bionic": ["apex/Android.bp"],
            "art": ["build/apex/Android.bp", "build/boot/Android.bp", "build/sdk/Android.bp"],
            "hardware/interfaces": ["power/aidl/default/apex/Android.bp"],
            "system/apex": ["libs/libapexsupport/tests/Android.bp"],
        }
        for version in self.lock["versions"]:
            projects = {p["path"]: p for p in version["projects"]}
            for path, files in wanted.items():
                self.assertEqual(projects[path].get("exclude"), files, f"{version['android']} {path}")

    def test_nfr5_a_lock_for_another_build_than_the_image_is_refused(self):
        lock = copy.deepcopy(self.lock)
        lock["versions"][0]["build_id"] = "1"
        lock["versions"][0]["manifest"]["url"] = framework_lock.MANIFEST_URL.format(
            build_id="1", target=lock["versions"][0]["target"])
        with self.assertRaisesRegex(framework_lock.LockError, "image is made from build"):
            framework_lock.check(lock, self.images)

    def test_nfr5_a_project_from_outside_aosp_is_refused(self):
        lock = copy.deepcopy(self.lock)
        lock["versions"][0]["projects"][0]["url"] = "https://github.com/someone/fork"
        with self.assertRaisesRegex(framework_lock.LockError, "not fetched from AOSP"):
            framework_lock.check(lock, self.images)

    def test_nfr5_the_lock_fetches_every_folder_the_patches_change(self):
        for version in self.lock["versions"]:
            projects = {p["path"]: p for p in version["projects"]}
            found = framework_lock.patch_files(version["android"])
            self.assertTrue(found, version["android"])
            for project, patch in found:
                cone = projects[project].get("cone")
                for path in patched_paths(patch):
                    if cone is not None:
                        self.assertTrue(any(path.startswith(c + "/") for c in cone),
                                        f"{version['android']} {project}: {path} is outside {cone}")

    def test_nfr5_both_versions_patch_renderengine_surfaceflinger_libegl_and_hwui(self):
        for android in ("android16", "android17"):
            changed = {(project, "/".join(path.split("/")[:2]))
                       for project, patch in framework_lock.patch_files(android)
                       for path in patched_paths(patch)}
            for wanted in [("frameworks/native", "libs/renderengine"),
                           ("frameworks/native", "services/surfaceflinger"),
                           ("frameworks/native", "opengl/libs"),
                           ("frameworks/base", "libs/hwui")]:
                self.assertIn(wanted, changed, android)

    def test_nfr5_each_patch_adds_the_tested_recovery_policy_unchanged(self):
        # The policy the renderers follow is the one test_device_loss_recovery.cpp tests.
        canonical = (HERE / "recovery/DeviceLossRecovery.h").read_text()
        added = 0
        for android in ("android16", "android17"):
            for project, patch in framework_lock.patch_files(android):
                text = patch.read_text()
                for match in re.finditer(r"^\+\+\+ b/(\S+DeviceLossRecovery\.h)\n@@ [^\n]+\n((?:\+[^\n]*\n)+)",
                                         text, re.M):
                    body = "".join(line[1:] + "\n" for line in match.group(2).splitlines())
                    self.assertEqual(body, canonical, f"{android} {project} {match.group(1)}")
                    added += 1
        self.assertEqual(added, 4)

    def test_nfr5_the_patches_carry_no_fatal_on_device_loss(self):
        # The aborts that ended SurfaceFlinger and the apps on a GPU reset are taken out: at the
        # start of a frame in RenderEngine and HWUI, and in each one's Vulkan device lost
        # callback (RenderEngine's crashes through LOG_THREAD_STATE_AND_CRASH on Android 17).
        crash = {"android16": "LOG_ALWAYS_FATAL", "android17": "LOG_THREAD_STATE_AND_CRASH"}
        for android in ("android16", "android17"):
            removed = {
                "frameworks/native": ['"Context is abandoned/device lost at start of %s"',
                                      f'{crash[android]}("%s", crashMsg.str().c_str());'],
                "frameworks/base": ['LOG_ALWAYS_FATAL("Lost GPU device unexpectedly");',
                                    'LOG_ALWAYS_FATAL("%s", crashMsg.str().c_str());'],
            }
            minus = {}
            for project, patch in framework_lock.patch_files(android):
                minus.setdefault(project, []).extend(
                    line for line in patch.read_text().splitlines() if line.startswith("-"))
            for project, wanted in removed.items():
                for text in wanted:
                    self.assertTrue(any(text in line for line in minus[project]),
                                    f"{android} {project}: {text}")

    def test_nfr5_the_build_version_changes_with_the_pins_and_the_patches(self):
        version = framework_lock.version_entry(self.lock, "android17")
        before = framework_lock.build_version(version)
        self.assertRegex(before, r"^17-[0-9a-f]{12}$")
        with tempfile.TemporaryDirectory() as tmp:
            patches = Path(tmp)
            (patches / "frameworks/base/android17").mkdir(parents=True)
            patch = patches / "frameworks/base/android17/0001-a.patch"
            patch.write_text("one\n")
            first = framework_lock.build_version(version, patches)
            patch.write_text("two\n")
            self.assertNotEqual(framework_lock.build_version(version, patches), first)
        moved = copy.deepcopy(version)
        moved["projects"][0]["commit"] = OTHER
        self.assertNotEqual(framework_lock.build_version(moved), before)


class ManifestTests(unittest.TestCase):
    def _version(self, manifest):
        return {
            "android": "android17", "android_major": 17, "build_id": "1",
            "manifest": {"url": "", "sha256": hashlib.sha256(manifest.encode()).hexdigest()},
            "projects": [
                {"path": "frameworks/base", "url": "https://android.googlesource.com/platform/frameworks/base",
                 "commit": COMMIT},
                {"path": "build/make", "url": "https://android.googlesource.com/platform/build", "commit": COMMIT},
            ],
        }

    def test_nfr5_the_lock_matches_the_build_manifest(self):
        manifest = (f'<manifest><project path="frameworks/base" name="platform/frameworks/base" revision="{COMMIT}"/>'
                    f'<project path="build/make" name="platform/build" revision="{COMMIT}"/></manifest>')
        self.assertEqual(framework_lock.manifest_problems(self._version(manifest), manifest.encode()), [])

    def test_nfr5_a_commit_the_build_was_not_made_from_is_found(self):
        manifest = (f'<manifest><project path="frameworks/base" name="platform/frameworks/base" revision="{OTHER}"/>'
                    f'</manifest>')
        problems = framework_lock.manifest_problems(self._version(manifest), manifest.encode())
        self.assertTrue(any("was made from fedcba987654" in p for p in problems), problems)
        self.assertTrue(any("build/make is not in build 1's manifest" in p for p in problems), problems)

    def test_nfr5_another_manifest_than_the_pinned_one_is_refused(self):
        manifest = "<manifest/>"
        version = self._version("<manifest></manifest>")
        self.assertIn("SHA-256 differs", framework_lock.manifest_problems(version, manifest.encode())[0])


class FetchTests(unittest.TestCase):
    def test_nfr5_the_fetch_reads_the_framework_lock_and_its_toolchains(self):
        version = fetch_source.load_version(framework_lock.LOCK, "android16")
        self.assertEqual(version["build_id"], "15581820")
        # The lock's own Clang and Rust, not what the tree's trunk_staging release would pick.
        self.assertEqual(fetch_source.toolchains(version, Path("/nonexistent")), (["clang-r563880c"], ["1.88.0"]))


class ApplyPatchesTests(unittest.TestCase):
    ORIGINAL = "first line\nsecond line\nthird line\n"

    def _patches(self, root, new_line="second line, patched"):
        patches = root / "patches"
        folder = patches / "frameworks/base/android17"
        folder.mkdir(parents=True)
        (folder / "0001-change.patch").write_text(
            "Subject: a change\n---\n"
            "diff --git a/libs/hwui/a.txt b/libs/hwui/a.txt\n"
            "--- a/libs/hwui/a.txt\n+++ b/libs/hwui/a.txt\n"
            f"@@ -1,3 +1,3 @@\n first line\n-second line\n+{new_line}\n third line\n")
        (folder / "0002-add.patch").write_text(
            "diff --git a/libs/hwui/b.txt b/libs/hwui/b.txt\nnew file mode 100644\n"
            "--- /dev/null\n+++ b/libs/hwui/b.txt\n@@ -0,0 +1 @@\n+added\n")
        return patches

    def _tree(self, root):
        tree = root / "tree"
        (tree / "frameworks/base/libs/hwui").mkdir(parents=True)
        (tree / "frameworks/base/libs/hwui/a.txt").write_text(self.ORIGINAL)
        return tree

    def test_nfr5_the_patches_apply_in_order_to_the_tree(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree = self._tree(root)
            done = apply_patches.apply_patches("android17", tree, patches=self._patches(root))
            self.assertEqual([name for _, name, _ in done], ["0001-change.patch", "0002-add.patch"])
            self.assertIn("second line, patched", (tree / "frameworks/base/libs/hwui/a.txt").read_text())
            self.assertEqual((tree / "frameworks/base/libs/hwui/b.txt").read_text(), "added\n")

    def test_nfr5_a_check_changes_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree = self._tree(root)
            apply_patches.apply_patches("android17", tree, check=True, patches=self._patches(root))
            self.assertEqual((tree / "frameworks/base/libs/hwui/a.txt").read_text(), self.ORIGINAL)
            self.assertFalse((tree / "frameworks/base/libs/hwui/b.txt").exists())

    def test_nfr5_a_patch_for_other_source_stops_the_build(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree = self._tree(root)
            (tree / "frameworks/base/libs/hwui/a.txt").write_text("first line\nsomething else\nthird line\n")
            with self.assertRaisesRegex(apply_patches.PatchError, "0001-change.patch does not apply"):
                apply_patches.apply_patches("android17", tree, patches=self._patches(root))
            self.assertFalse((tree / "frameworks/base/libs/hwui/b.txt").exists())

    def test_nfr5_a_tree_inside_another_repository_still_takes_the_patches(self):
        # Inside a repository git apply would skip the paths outside the folder it runs in and
        # still succeed; the tree a developer keeps under .scratch is inside DSA's.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            git("init", "-q", cwd=root)
            tree = self._tree(root)
            apply_patches.apply_patches("android17", tree, patches=self._patches(root))
            self.assertIn("second line, patched", (tree / "frameworks/base/libs/hwui/a.txt").read_text())

    def test_nfr5_a_missing_project_stops_the_build(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "tree").mkdir()
            with self.assertRaisesRegex(apply_patches.PatchError, "frameworks/base is not in"):
                apply_patches.apply_patches("android17", root / "tree", patches=self._patches(root))


class CollectTests(unittest.TestCase):
    @staticmethod
    def _elf(machine, body=b""):
        header = bytearray(64)
        header[:4] = b"\x7fELF"
        header[4] = 2
        header[5] = 1
        header[18:20] = machine.to_bytes(2, "little")
        return bytes(header) + body

    def _product(self, root, machine):
        product = root / "product"
        for installed in framework_lock.OUTPUTS.values():
            (product / installed).parent.mkdir(parents=True, exist_ok=True)
            (product / installed).write_bytes(self._elf(machine, installed.encode()))
        return product

    def test_nfr5_collect_takes_the_three_files_and_describes_them_for_the_image(self):
        version = framework_lock.version_entry(framework_lock.load(), "android17")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            description = collect.collect(version, "arm64", self._product(root, 183), out)
            written = json.loads((out / "framework.json").read_text())
            self.assertEqual(written, description)
            self.assertEqual(written["build_id"], "16373615")
            self.assertEqual(written["arch"], "arm64")
            self.assertEqual(written["version"], framework_lock.build_version(version))
            self.assertEqual(sorted(written["files"]), sorted(framework_lock.OUTPUTS))
            for name, entry in written["files"].items():
                data = (out / name).read_bytes()
                self.assertEqual(entry, {"size": len(data), "sha256": hashlib.sha256(data).hexdigest()})

    def test_nfr5_collect_refuses_a_file_built_for_another_architecture(self):
        version = framework_lock.version_entry(framework_lock.load(), "android16")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaisesRegex(SystemExit, "not x86_64"):
                collect.collect(version, "x86_64", self._product(root, 183), root / "out")


if __name__ == "__main__":
    unittest.main()
