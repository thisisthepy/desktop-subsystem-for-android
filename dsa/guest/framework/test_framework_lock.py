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
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import framework_lock  # noqa: E402

REPO = HERE.parents[2]
STAND_INS = REPO / ".github/scripts/framework/stand-ins"


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

    def test_nfr5_the_sdk_project_gives_only_its_flag_lists_under_a_stand_in_build_file(self):
        # The real prebuilts/sdk/Android.bp declares the prebuilt SDKs, whose API files the tree
        # does not have: the first Framework build run's analysis stopped on all four builds
        # with 'module "sdk": no api file found under "prebuilts/sdk"'. aconfig still needs
        # //prebuilts/sdk:finalized-api-flags, so build.sh puts a stand-in in its place.
        for version in self.lock["versions"]:
            sdk = next(p for p in version["projects"] if p["path"] == "prebuilts/sdk")
            self.assertEqual(sdk.get("patterns"), ["/NOTICE", "/*/finalized-flags.txt"], version["android"])
            self.assertNotIn("cone", sdk)
        stand_in = (STAND_INS / "common/prebuilts/sdk/Android.bp").read_text()
        code = re.sub(r"//[^\n]*", "", stand_in)
        self.assertNotIn("prebuilt_apis", code)
        self.assertRegex(code, r'filegroup \{\s*name: "finalized-api-flags",\s*srcs: \[\s*"\*/finalized-flags.txt"')
        self.assertRegex(code, r'genrule \{\s*name: "latest-finalized-flags"')
        self.assertRegex(code, r'license \{\s*name: "prebuilts_sdk_license"')

    def test_nfr5_python_tools_find_the_host_libsqlite_soong_adds_to_them(self):
        # Every python_binary_host depends on libsqlite, which no project in the tree defines;
        # the translator build's seventh run failed on it. aprotoc, the other module Soong adds,
        # is external/protobuf's own, so a stand-in for it would clash with the real one.
        hosttools = (REPO / ".github/scripts/framework/device/hosttools/Android.bp").read_text()
        self.assertRegex(hosttools, r'cc_library_host_shared \{\s*name: "libsqlite"')
        self.assertNotIn('name: "aprotoc"', hosttools)
        for version in self.lock["versions"]:
            self.assertIn("external/protobuf", [p["path"] for p in version["projects"]])
        build = (REPO / ".github/scripts/framework/build.sh").read_text()
        self.assertIn('cp -r "$here/device/hosttools" "$device/"', build)
        # With external/sqlite in the tree (libandroid_runtime needs it) its libsqlite is the
        # real one, and the stand-in would define the module twice.
        self.assertIn('if [[ ! -e "$top/external/sqlite/dist/Android.bp" ]]; then', build)

    def test_nfr5_modules_soong_analyses_find_their_sources_and_licenses(self):
        # Errors that missing dependencies do not excuse stopped the second and third Framework
        # build runs: a package whose default license is in a folder the cone left out, and an
        # aidl_interface whose frozen version (aidl_api/<name>/<version>) was not fetched.
        for version in self.lock["versions"]:
            projects = {p["path"]: p for p in version["projects"]}
            self.assertIn("adobe-hdr-gain-map-license", projects["external/libultrahdr"]["cone"])
            self.assertIn("services/mediaresourcemanager/aidl_api", projects["frameworks/av"]["cone"])

    def test_nfr5_host_tools_soong_runs_by_path_are_in_the_tree(self):
        # Soong names some host tools by their install path instead of as a dependency, so a
        # missing one is not a missing dependency but a file ninja cannot make: the fourth run
        # stopped on out/host/linux-x86/bin/protoc-gen-rust (16) and protoc-gen-rs (17), the
        # protobuf plugin aconfig's Rust protos are generated with. x86_64 assembly (.asm, in
        # libjpeg-turbo) is assembled with prebuilts/misc's yasm in the same way.
        plugin_crates = ["errno", "fastrand", "protobuf-codegen", "protobuf-parse", "rustix",
                         "tempfile", "which"]
        for version in self.lock["versions"]:
            projects = {p["path"]: p for p in version["projects"]}
            crates = projects["external/rust/android-crates-io"]["cone"]
            for crate in plugin_crates + (["getrandom"] if version["android"] == "android17" else []):
                self.assertIn("crates/" + crate, crates, version["android"])
            self.assertEqual(projects["prebuilts/misc"]["cone"], ["linux-x86/yasm"], version["android"])

    def test_nfr5_framework_build_files_over_sources_the_tree_lacks_are_stood_in(self):
        # frameworks/base's build files declare the framework's Java, AIDL and proto modules
        # over folders the tree does not fetch; the second and third runs stopped on an
        # aidl_interface whose srcs matched nothing and on a gensrcs with no input (a panic).
        # The lock leaves those files out and build.sh puts DSA's cut of each in its place.
        for version in self.lock["versions"]:
            base = next(p for p in version["projects"] if p["path"] == "frameworks/base")
            self.assertEqual(base.get("exclude"), [
                "Android.bp", "api/Android.bp", "core/java/Android.bp", "libs/hwui/Android.bp",
                "media/Android.bp", "media/jni/Android.bp", "media/jni/audioeffect/Android.bp",
                "media/jni/soundpool/Android.bp", "media/jni/soundpool/tests/Android.bp",
                "services/Android.bp", "services/core/Android.bp"],
                version["android"])
        root = (STAND_INS / "common/frameworks/base/Android.bp").read_text()
        self.assertRegex(root, r'license \{\s*name: "frameworks_base_license"')
        self.assertRegex(root, r'build = \[\s*"AconfigFlags.bp",\s*\]')
        self.assertNotIn('"ProtoLibraries.bp"', root)
        for android in ("android16", "android17"):
            core = (STAND_INS / android / "frameworks/base/core/java/Android.bp").read_text()
            types = set(re.findall(r"^([a-z_]+) \{", core, re.M))
            self.assertFalse({t for t in types if t.startswith("java_")}, android)
            self.assertNotIn("gensrcs", types)
            for name in ("libpowermanager_aidl", "android.os.hintmanager_aidl", "HardwareBuffer_aidl"):
                self.assertIn(f'name: "{name}"', core, android)
            self.assertNotIn('name: "android.security.rkp_aidl"', core)
            hwui = (STAND_INS / android / "frameworks/base/libs/hwui/Android.bp").read_text()
            self.assertRegex(hwui, r'cc_library \{\s*name: "libhwui"')
            self.assertNotIn("java_sdk_library", hwui)

    def test_nfr5_every_stand_in_replaces_a_file_the_lock_leaves_out(self):
        # build.sh stops when the checkout brings the file a stand-in replaces, so each stand-in
        # must name a file its version's lock excludes or does not fetch.
        for version in self.lock["versions"]:
            projects = sorted(version["projects"], key=lambda p: -len(p["path"]))
            for root in (STAND_INS / "common", STAND_INS / version["android"]):
                for file in root.rglob("*"):
                    if not file.is_file():
                        continue
                    rel = file.relative_to(root).as_posix()
                    self.assertTrue(file.read_text().startswith("// Stands in for " + rel), rel)
                    project = next(p for p in projects if rel.startswith(p["path"] + "/"))
                    inside = rel[len(project["path"]) + 1:]
                    left_out = inside in project.get("exclude", []) or (
                        "patterns" in project and "/" + inside not in project["patterns"])
                    self.assertTrue(left_out, f"{version['android']} {rel}")
        build = (REPO / ".github/scripts/framework/build.sh").read_text()
        self.assertIn('for stand_ins in "$here/stand-ins/common" "$here/stand-ins/$android"; do', build)
        self.assertIn("the lock must leave it out for its stand-in", build)

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
                # A patch to Java sources only is for the services build, whose tree takes every
                # project whole (ServicesTests).
                if framework_lock.java_only(patch):
                    continue
                cone = projects[project].get("cone")
                for path in patched_paths(patch):
                    if cone is not None:
                        self.assertTrue(any(path.startswith(c + "/") for c in cone),
                                        f"{version['android']} {project}: {path} is outside {cone}")

    def test_nfr5_frameworks_base_patches_change_only_sources_of_files_the_image_takes(self):
        # The image takes a few of the framework build's files and keeps the published build's
        # copy of every other one. A patch to frameworks/base outside the folders those files
        # are built from changes nothing the image ships, or worse ships half of a change:
        # libandroid_runtime once registered a native for a Process method that the published
        # framework.jar does not declare, and zygote aborted at every start.
        taken = set(framework_lock.OUTPUTS) | set(framework_lock.JAVA_OUTPUTS)
        self.assertTrue(set(framework_lock.FRAMEWORKS_BASE_SOURCES) <= taken,
                        set(framework_lock.FRAMEWORKS_BASE_SOURCES) - taken)
        folders = [folder for sources in framework_lock.FRAMEWORKS_BASE_SOURCES.values()
                   for folder in sources]
        for android in ("android16", "android17"):
            found = [patch for project, patch in framework_lock.patch_files(android)
                     if project == "frameworks/base"]
            self.assertTrue(found, android)
            for patch in found:
                for path in framework_lock.patch_paths(patch):
                    self.assertTrue(any(path.startswith(folder + "/") for folder in folders),
                                    f"{android} {patch.name} changes {path}, which no file the "
                                    f"image takes is built from (only {folders} are)")

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

    def test_nfr1_both_versions_free_the_client_target_when_nothing_is_composed_into_it(self):
        # The client target is the display's size (63 MiB at 2880x2880). FramebufferSurface
        # gives its buffers back after a while without client composition, and RenderSurface
        # drops its textures of them, or the memory would stay held.
        for android in ("android16", "android17"):
            added = {}
            for project, patch in framework_lock.patch_files(android):
                if project != "frameworks/native":
                    continue
                path = None
                for line in patch.read_text().splitlines():
                    if line.startswith("+++ b/"):
                        path = line[len("+++ b/"):]
                    elif line.startswith("+") and path:
                        added.setdefault(path, []).append(line[1:])
            def adds(path, text):
                return any(text in line for line in added.get(path, []))
            fbs = "services/surfaceflinger/DisplayHardware/FramebufferSurface.cpp"
            self.assertTrue(adds("services/surfaceflinger/CompositionEngine/include/compositionengine/"
                                 "DisplaySurface.h", "virtual bool takeBuffersReleased()"), android)
            self.assertTrue(adds("services/surfaceflinger/CompositionEngine/src/RenderSurface.cpp",
                                 "mTextureCache.clear();"), android)
            self.assertTrue(adds(fbs, "discardFreeBuffers("), android)
            self.assertTrue(adds(fbs, "kReleaseIdleBuffersAfter"), android)
            self.assertTrue(adds("services/surfaceflinger/DisplayHardware/FramebufferSurface.h",
                                 "kReleaseIdleBuffersAfter = 1'000'000'000"), android)

    def test_nfr1_android17_frees_the_client_target_whichever_framebuffer_surface_it_picks(self):
        # Android 17 picks FramebufferSurface or LegacyFramebufferSurface by the
        # wb_framebuffersurface2 flag, and the build DSA ships takes the legacy one. A release
        # only in FramebufferSurface never ran there: the client target stayed held, and its
        # "buffers are freed" line never appeared.
        added = {}
        for project, patch in framework_lock.patch_files("android17"):
            if project != "frameworks/native":
                continue
            path = None
            for line in patch.read_text().splitlines():
                if line.startswith("+++ b/"):
                    path = line[len("+++ b/"):]
                elif line.startswith("+") and path:
                    added.setdefault(path, []).append(line[1:])

        def adds(path, text):
            return any(text in line for line in added.get(path, []))

        for name in ("FramebufferSurface", "LegacyFramebufferSurface"):
            source = f"services/surfaceflinger/DisplayHardware/{name}.cpp"
            header = f"services/surfaceflinger/DisplayHardware/{name}.h"
            self.assertTrue(adds(header, "bool takeBuffersReleased() override;"), name)
            self.assertTrue(adds(header, "kReleaseIdleBuffersAfter = 1'000'000'000"), name)
            self.assertTrue(adds(source, f"bool {name}::takeBuffersReleased()"), name)
            self.assertTrue(adds(source, "discardFreeBuffers("), name)
            self.assertTrue(adds(source, "its buffers are freed"), name)
        # The legacy class tells HWC about each slot's buffer only once; after the discard the
        # slots hold new buffers, which must go to HWC whole.
        self.assertTrue(adds("services/surfaceflinger/DisplayHardware/LegacyFramebufferSurface.cpp",
                             "id = UINT64_MAX;"))

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


check_ninja_inputs = load_module("check_ninja_inputs", REPO / ".github/scripts/translator/check-ninja-inputs.py")

# What `ninja -t graph out/lib.so` prints for: lib.so <- link(a.o, b.o); a.o <- cc(a.c);
# b.o <- cc(gen/b.c, missing.h); gen/b.c <- gen() with no inputs.
GRAPH = """digraph ninja {
rankdir="LR"
node [fontsize=10, shape=box, height=0.25]
edge [fontsize=10]
"0x1" [label="out/lib.so"]
"0x2" [label="link", shape=ellipse]
"0x2" -> "0x1"
"0x3" -> "0x2" [arrowhead=none]
"0x4" -> "0x2" [arrowhead=none]
"0x3" [label="out/a.o"]
"0x5" [label="src/a.c"]
"0x5" -> "0x3" [label=" cc"]
"0x4" [label="out/b.o"]
"0x6" [label="cc", shape=ellipse]
"0x6" -> "0x4"
"0x7" -> "0x6" [arrowhead=none]
"0x8" -> "0x6" [arrowhead=none]
"0x7" [label="out/gen/b.c"]
"0x9" [label="gen", shape=ellipse]
"0x9" -> "0x7"
"0x8" [label="src/missing.h"]
}
"""


class NinjaInputsTests(unittest.TestCase):
    def test_nfr5_every_input_no_rule_makes_is_found(self):
        self.assertEqual(check_ninja_inputs.leaves(GRAPH.splitlines()), ["src/a.c", "src/missing.h"])

    def test_nfr5_every_missing_input_is_listed_at_once(self):
        # Ninja stops at the first missing input; the fifth run stopped on boringssl's
        # inject_hash.go, so each run showed one file. The check lists all of them.
        with tempfile.TemporaryDirectory() as top:
            Path(top, "src").mkdir()
            Path(top, "src/a.c").write_text("")
            leaves = check_ninja_inputs.leaves(GRAPH.splitlines())
            self.assertEqual(check_ninja_inputs.missing(top, leaves), ["src/missing.h"])

    @unittest.skipIf(os.name == "nt", "links need privileges on Windows")
    def test_nfr5_a_link_to_a_folder_the_tree_leaves_out_is_missing(self):
        # The sixth run's android17 builds stopped on a link in frameworks/base/core/jni whose
        # target, in media/jni, the cone did not take.
        with tempfile.TemporaryDirectory() as top:
            Path(top, "src").mkdir()
            Path(top, "src/a.c").write_text("")
            os.symlink("../gone/missing.h", Path(top, "src/missing.h"))
            leaves = check_ninja_inputs.leaves(GRAPH.splitlines())
            self.assertEqual(check_ninja_inputs.missing(top, leaves), ["src/missing.h"])

    def test_nfr5_both_builds_check_the_inputs_before_building(self):
        for build in (".github/scripts/framework/build.sh", ".github/scripts/translator/build.sh"):
            text = (REPO / build).read_text()
            analysis = text.index("m --skip-ninja")
            check = text.index("check-ninja-inputs.py")
            real = text.index('m -k -j"$(nproc)"')
            self.assertLess(analysis, check, build)
            self.assertLess(check, real, build)
            self.assertIn('--file "$top/out/combined-$product.ninja"', text, build)

    def test_nfr5_the_headers_the_compiled_libraries_include_are_fetched(self):
        # The sixth run compiled for the first time and stopped on headers the cones left
        # out: Soong's global include folders (system/core/include and
        # hardware/libhardware/include, both links into folders the tree has), links in
        # frameworks/av/include/media and frameworks/base/core/jni into folders it did not
        # have, and headers beside the sources of pdfium, XMP, llvm-libc, zlib, cxx,
        # arm-optimized-routines and crabbyavif. stats-log-api-gen, which SurfaceFlinger's
        # and libhwui's statslog sources are generated with, is frameworks/proto_logging's.
        lock = framework_lock.load()
        wanted = {
            "system/core": ["include"],
            "hardware/libhardware": ["include"],
            "frameworks/av": ["media/libaudioprocessing/include", "media/libeffects/factory/include",
                              "media/libmediaplayerservice/include"],
            "frameworks/base": ["media/jni"],
            "external/pdfium": ["build", "core"],
            "external/llvm-libc": ["hdr", "include", "src"],
            "external/XMP-Toolkit-SDK": ["XMPCommon", "build", "third-party"],
        }
        for version in lock["versions"]:
            projects = {p["path"]: p for p in version["projects"]}
            for path, folders in wanted.items():
                for folder in folders:
                    self.assertIn(folder, projects[path]["cone"], f"{version['android']} {path}")
            for whole in ("external/zlib", "external/rust/cxx", "external/arm-optimized-routines",
                          "external/rust/crabbyavif", "frameworks/proto_logging"):
                self.assertNotIn("cone", projects[whole], whole)
                self.assertNotIn("patterns", projects[whole], whole)

    def test_nfr5_bindgen_finds_the_clang_soong_gives_it(self):
        # Rust bindgen runs with LIBCLANG_PATH set to its own Clang (bindgenClangVersion in
        # build/soong/rust/bindgen.go), not the C compiler's; the seventh run's bindgen rules
        # all failed with "Unable to find libclang".
        bindgen_clang = {"android16": "clang-r563880", "android17": "clang-r584948"}
        for version in framework_lock.load()["versions"]:
            clang = next(p for p in version["projects"] if p["path"] == "prebuilts/clang/host/linux-x86")
            for folder in ("bin", "lib"):
                self.assertIn(f"{bindgen_clang[version['android']]}/{folder}", clang["cone"], version["android"])

    def test_nfr5_the_seventh_runs_headers_are_fetched(self):
        # Skia's SkSL and third-party headers, upb's port headers and the Rust kernel headers
        # protoc's Rust generator includes (17), and hardware_legacy/power.h, which
        # PowerHalLoader includes through Soong's global hardware/libhardware_legacy/include.
        for version in framework_lock.load()["versions"]:
            projects = {p["path"]: p for p in version["projects"]}
            for folder in ("include/sksl", "include/third_party"):
                self.assertIn(folder, projects["external/skia"]["cone"], version["android"])
            if version["android"] == "android17":
                for folder in ("upb/port", "rust/cpp_kernel"):
                    self.assertIn(folder, projects["external/protobuf"]["cone"])
            legacy = projects["hardware/libhardware_legacy"]
            self.assertNotIn("cone", legacy)
            self.assertNotIn("patterns", legacy)

    def test_fr32_what_libandroid_runtime_links_is_fetched(self):
        # Framework build 43377022889 passed the analysis and the input check, then stopped in
        # ninja on libraries libandroid_runtime links: headers beside sources the cones left out
        # (libnetd_client's and libandroid_net's NetdClient.h, libmeminfo's include, perfetto's
        # JNIHelp.h, libbpf's libelf.h, contexthub_data_flow's chre/platform/atomic_ref.h), a
        # HIDL interface's dependency (camera.device@3.2 on camera.common@1.0), a project
        # libtflite_static includes by path (external/gemmlowp), and aconfig declarations
        # whose flag files were not fetched ("production library contains no production
        # flags", or the file named as missing). libtflite_static's own sources, which that
        # run never reached, include TensorFlow's core, TSL and third_party headers.
        both = {
            "external/elfutils": ["bionic-fixup", "include", "lib", "libelf"],
            "external/perfetto": ["src/android_sdk/nativehelper"],
            "external/tensorflow": ["tensorflow/core/kernels", "tensorflow/core/lib/bfloat16",
                                    "tensorflow/core/lib/random", "tensorflow/core/platform",
                                    "tensorflow/core/public", "tensorflow/lite",
                                    "tensorflow/tsl/platform", "third_party/eigen3",
                                    "third_party/fft2d"],
            "frameworks/base": ["core/java/android/database/sqlite", "core/java/android/permission",
                                "core/java/android/window/flags"],
            "hardware/interfaces": ["camera/common/1.0", "camera/device/3.2"],
            "system/memory/libmeminfo": ["include"],
            "system/netd": ["client", "include"],
        }
        only = {
            "android16": {},
            "android17": {
                "frameworks/base": ["core/java/android/tracing"],
                "system/chre": ["platform/include", "platform/shared/public_std_atomic_ref",
                                "variant/googletest/include", "variant/include"],
            },
        }
        for version in framework_lock.load()["versions"]:
            projects = {p["path"]: p for p in version["projects"]}
            for wanted in (both, only[version["android"]]):
                for path, folders in wanted.items():
                    for folder in folders:
                        self.assertIn(folder, projects[path]["cone"], f"{version['android']} {path}")
            gemmlowp = projects["external/gemmlowp"]
            self.assertNotIn("cone", gemmlowp)
            self.assertNotIn("patterns", gemmlowp)

    def test_nfr5_boringssls_go_build_tools_are_fetched(self):
        # bssl_inject_hash and the Go packages it uses, which the fifth run stopped on.
        lock = framework_lock.load()
        for version in lock["versions"]:
            cone = next(p for p in version["projects"] if p["path"] == "external/boringssl")["cone"]
            for folder in ("src/util/ar", "src/util/fipstools/fipscommon", "src/util/fipstools/inject_hash"):
                self.assertIn(folder, cone, version["android"])


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

    def test_nfr5_a_check_takes_a_patch_on_lines_an_earlier_patch_wrote(self):
        # A build applies the patches in order, so a later patch may change what an earlier one
        # wrote; checking each against the bare source would fail a series a build takes.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree = self._tree(root)
            patches = self._patches(root)
            (patches / "frameworks/base/android17/0003-on-top.patch").write_text(
                "diff --git a/libs/hwui/a.txt b/libs/hwui/a.txt\n"
                "--- a/libs/hwui/a.txt\n+++ b/libs/hwui/a.txt\n"
                "@@ -1,3 +1,3 @@\n first line\n-second line, patched\n+second line, patched again\n third line\n")
            done = apply_patches.apply_patches("android17", tree, check=True, patches=patches)
            self.assertEqual(len(done), 3)
            self.assertEqual((tree / "frameworks/base/libs/hwui/a.txt").read_text(), self.ORIGINAL)
            self.assertFalse((tree / "frameworks/base/libs/hwui/b.txt").exists())

    def test_nfr5_a_check_that_stops_leaves_the_tree_as_it_was(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree = self._tree(root)
            patches = self._patches(root)
            (patches / "frameworks/base/android17/0003-stale.patch").write_text(
                "diff --git a/libs/hwui/a.txt b/libs/hwui/a.txt\n"
                "--- a/libs/hwui/a.txt\n+++ b/libs/hwui/a.txt\n"
                "@@ -1,3 +1,3 @@\n first line\n-no such line\n+other\n third line\n")
            with self.assertRaisesRegex(apply_patches.PatchError, "0003-stale.patch does not apply"):
                apply_patches.apply_patches("android17", tree, check=True, patches=patches)
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
        for installed in {**framework_lock.OUTPUTS, **framework_lock.KEYMINT_OUTPUTS}.values():
            (product / installed).parent.mkdir(parents=True, exist_ok=True)
            (product / installed).write_bytes(self._elf(machine, installed.encode()))
        return product

    def _tree(self, root):
        top = root / "tree"
        for path in framework_lock.KEYMINT_SOURCES.values():
            (top / path).parent.mkdir(parents=True, exist_ok=True)
            (top / path).write_text(f"<manifest>{path}</manifest>\n")
        return top

    def test_nfr5_collect_takes_the_three_files_and_describes_them_for_the_image(self):
        version = framework_lock.version_entry(framework_lock.load(), "android17")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            description = collect.collect(version, "arm64", self._product(root, 183), out, top=self._tree(root))
            written = json.loads((out / "framework.json").read_text())
            self.assertEqual(written, description)
            self.assertEqual(written["build_id"], "16373615")
            self.assertEqual(written["arch"], "arm64")
            self.assertEqual(written["version"], framework_lock.build_version(version))
            self.assertEqual(sorted(written["files"]), sorted(list(framework_lock.OUTPUTS) + framework_lock.KEYMINT_FILES))
            for name, entry in written["files"].items():
                data = (out / name).read_bytes()
                self.assertEqual(entry, {"size": len(data), "sha256": hashlib.sha256(data).hexdigest()})

    def test_nfr5_collect_refuses_a_file_built_for_another_architecture(self):
        version = framework_lock.version_entry(framework_lock.load(), "android16")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with self.assertRaisesRegex(SystemExit, "not x86_64"):
                collect.collect(version, "x86_64", self._product(root, 183), root / "out", top=self._tree(root))

    def test_fr36_collect_takes_the_keymint_from_the_build_and_its_fragments_from_the_tree(self):
        version = framework_lock.version_entry(framework_lock.load(), "android16")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            out = root / "out"
            top = self._tree(root)
            description = collect.collect(version, "x86_64", self._product(root, 62), out, top=top)
            for name, path in framework_lock.KEYMINT_SOURCES.items():
                self.assertEqual((out / name).read_bytes(), (top / path).read_bytes())
            self.assertIn(framework_lock.KEYMINT_BINARY, description["files"])
            self.assertIn("VENDOR/lib64/libssl.so", description["files"])
            # Without the tree there are no fragments, and without them no image can declare it.
            with self.assertRaisesRegex(SystemExit, "give --top"):
                collect.collect(version, "x86_64", self._product(root, 62), root / "out2")
            (top / framework_lock.KEYMINT_SOURCES[
                "VENDOR/etc/vintf/manifest/android.hardware.security.secureclock-service.xml"]).unlink()
            with self.assertRaisesRegex(SystemExit, "secureclock-service.xml"):
                collect.collect(version, "x86_64", self._product(root, 62), root / "out3", top=top)
            # A build that did not make the KeyMint is not collected.
            (root / "product" / framework_lock.KEYMINT_OUTPUTS[framework_lock.KEYMINT_BINARY]).unlink()
            with self.assertRaisesRegex(SystemExit, "did not make vendor/bin/hw/dsa-keymint"):
                collect.collect(version, "x86_64", root / "product", root / "out4", top=self._tree(root))


INSETS_POLICY = "services/core/java/com/android/server/wm/InsetsPolicy.java"


def added_lines(patch):
    return [line[1:] for line in Path(patch).read_text().splitlines()
            if line.startswith("+") and not line.startswith("+++")]


def removed_lines(patch):
    return [line[1:] for line in Path(patch).read_text().splitlines()
            if line.startswith("-") and not line.startswith("---")]


def write_java_outputs(folder):
    """Leaves in `folder` every jar the services build makes (framework_lock.JAVA_OUTPUTS), each a
    jar with a dex file of its own, under its target_files name."""
    for i, name in enumerate(framework_lock.JAVA_OUTPUTS):
        jar = Path(folder) / name
        jar.parent.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(jar, "w", zipfile.ZIP_DEFLATED) as z:
            z.writestr("classes.dex", b"dex\n035\0" + bytes([i]) * 100)


class ServicesTests(unittest.TestCase):
    """services.jar, built from the published build's source with DSA's patches to the window
    manager (a dialog gets no caption bar inset), the package manager, the WebView service, the
    display manager (Google Play is shown a phone's density) and the activity manager (Google
    Play is given the computer's installed memory as advertisedMem)."""

    def test_fr32_google_play_is_shown_a_phones_density_by_the_display_manager(self):
        for android in ("android16", "android17"):
            patches = self._patches_to(
                android, "services/core/java/com/android/server/display/DisplayManagerService.java")
            self.assertEqual(len(patches), 1, android)
            self.assertTrue(framework_lock.java_only(patches[0]), android)
            self.assertIn("                return getDisplayInfoInternal(displayId, callingUid);",
                          removed_lines(patches[0]), android)
            self.assertIn(f"Applies to frameworks/base at {self._commit(android)}", patches[0].read_text())

    def _patches_to(self, android, path):
        return [patch for project, patch in framework_lock.patch_files(android)
                if project == "frameworks/base" and path in framework_lock.patch_paths(patch)]

    def test_fr22_a_dialog_gets_no_caption_inset(self):
        for android in ("android16", "android17"):
            patches = self._patches_to(android, INSETS_POLICY)
            self.assertEqual(len(patches), 1, android)
            added = "\n".join(added_lines(patches[0]))
            # A window smaller than its parent, or one laid out below the band, loses the caption
            # bar source; the task's full-size edge-to-edge window keeps it.
            self.assertIn("if (!attrs.isFullscreen() || (attrs.getFitInsetsTypes()", added, android)
            self.assertIn("& (Type.captionBar() | Type.statusBars())) != 0) {", added, android)
            self.assertIn("                    state.removeSourceAt(i);", patches[0].read_text(), android)
            # Not behind a flag the published build turns on.
            self.assertNotIn("relativeInsets()", added, android)
            self.assertIn(f"Applies to frameworks/base at {self._commit(android)}", patches[0].read_text())

    def test_fr22_a_freeform_window_keeps_its_status_bar_inset_in_the_source(self):
        # The dex edit of the published jar (SERVICES_INSETS_CODE), ported to the source.
        for android in ("android16", "android17"):
            patch = self._patches_to(android, INSETS_POLICY)[0]
            self.assertIn("            int types = WindowInsets.Type.captionBar();", removed_lines(patch), android)
            self.assertIn("            int types = WindowInsets.Type.systemBars();", added_lines(patch), android)

    def test_fr22_the_other_services_dex_edits_are_ported_to_the_source(self):
        for android in ("android16", "android17"):
            abi = self._patches_to(android, "services/core/java/com/android/server/pm/PackageAbiHelperImpl.java")
            self.assertEqual(len(abi), 1, android)
            self.assertIn("        return Build.SUPPORTED_64_BIT_ABIS;", added_lines(abi[0]))
            webview = self._patches_to(android, "services/core/java/com/android/server/webkit/SystemImpl.java")
            self.assertEqual(len(webview), 1, android)
            self.assertIn("        WebViewZygote.getProcess();", removed_lines(webview[0]))
            self.assertFalse(any("getProcess()" in line and not line.strip().startswith("//")
                                 for line in added_lines(webview[0])))

    def _commit(self, android):
        version = framework_lock.version_entry(framework_lock.load(), android)
        return next(p["commit"] for p in version["projects"] if p["path"] == "frameworks/base")

    def test_fr22_the_native_build_leaves_the_java_patches_to_the_services_build(self):
        for android in ("android16", "android17"):
            java = [p for _, p in framework_lock.patch_files(android) if framework_lock.java_only(p)]
            self.assertEqual(len(java), 7, android)
            native = [p for _, p in framework_lock.patch_files(android) if not framework_lock.java_only(p)]
            self.assertTrue(native, android)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            folder = root / "patches/frameworks/base/android17"
            folder.mkdir(parents=True)
            (folder / "0001-native.patch").write_text(
                "diff --git a/libs/hwui/a.txt b/libs/hwui/a.txt\n--- a/libs/hwui/a.txt\n+++ b/libs/hwui/a.txt\n"
                "@@ -1 +1 @@\n-one\n+two\n")
            (folder / "0002-java.patch").write_text(
                "diff --git a/services/A.java b/services/A.java\n--- a/services/A.java\n+++ b/services/A.java\n"
                "@@ -1 +1 @@\n-class A {}\n+final class A {}\n")
            tree = root / "tree"
            (tree / "frameworks/base/libs/hwui").mkdir(parents=True)
            (tree / "frameworks/base/libs/hwui/a.txt").write_text("one\n")
            done = apply_patches.apply_patches("android17", tree, patches=root / "patches", native=True)
            self.assertEqual([name for _, name, _ in done], ["0001-native.patch"])
            # The services tree has the file and takes both.
            (tree / "frameworks/base/libs/hwui/a.txt").write_text("one\n")
            (tree / "frameworks/base/services").mkdir()
            (tree / "frameworks/base/services/A.java").write_text("class A {}\n")
            done = apply_patches.apply_patches("android17", tree, patches=root / "patches")
            self.assertEqual([name for _, name, _ in done], ["0001-native.patch", "0002-java.patch"])

    def _manifest(self):
        return (
            "<manifest>"
            f'<project path="build/make" name="platform/build" revision="{COMMIT}" groups="pdk">'
            '<linkfile src="core" dest="build/core"/><copyfile src="root.mk" dest="Makefile"/></project>'
            f'<project path="frameworks/base" name="platform/frameworks/base" revision="{COMMIT}"/>'
            f'<project path="prebuilts/clang/host/linux-x86" name="platform/prebuilts/clang/host/linux-x86" '
            f'revision="{COMMIT}" groups="pdk,linux"/>'
            f'<project path="prebuilts/clang/host/darwin-x86" name="platform/prebuilts/clang/host/darwin-x86" '
            f'revision="{COMMIT}" groups="pdk,darwin"/>'
            f'<project path="device/google/unsynced" name="device/google/unsynced" revision="{COMMIT}" '
            'groups="device,notdefault"/>'
            "</manifest>"
        ).encode()

    def test_fr22_the_services_tree_is_every_project_the_manifest_syncs_by_default(self):
        manifest = self._manifest().replace(
            b"</manifest>",
            f'<project path="prebuilts/vndk/v34" name="platform/prebuilts/vndk/v34" revision="{COMMIT}"/>'
            "</manifest>".encode())
        version = {
            "android": "android17", "android_major": 17, "build_id": "1", "release": "cp2a",
            "manifest": {"url": "", "sha256": hashlib.sha256(manifest).hexdigest()},
            "projects": [{"path": "prebuilts/clang/host/linux-x86", "url": GOOGLESOURCE_CLANG,
                          "commit": COMMIT, "cone": ["soong", "{clang}"]}],
        }
        tree = framework_lock.services_lock(version, manifest)
        self.assertEqual(tree["kind"], framework_lock.KIND)
        (entry,) = tree["versions"]
        self.assertEqual(entry["release"], "cp2a")
        projects = {p["path"]: p for p in entry["projects"]}
        self.assertEqual(sorted(projects), ["build/make", "frameworks/base", "prebuilts/clang/host/linux-x86"])
        self.assertNotIn("cone", projects["frameworks/base"])
        self.assertEqual(projects["prebuilts/clang/host/linux-x86"]["cone"], ["soong", "{clang}"])
        self.assertEqual(projects["build/make"]["linkfiles"], [{"src": "core", "dest": "build/core"}])
        self.assertEqual(projects["build/make"]["copyfiles"], [{"src": "root.mk", "dest": "Makefile"}])
        self.assertEqual(projects["frameworks/base"]["url"],
                         "https://android.googlesource.com/platform/frameworks/base")
        with self.assertRaisesRegex(framework_lock.LockError, "SHA-256 differs"):
            framework_lock.services_lock(version, manifest + b" ")

    def test_fr22_the_services_tree_takes_android_17s_rust_toolchain_in_part(self):
        # Android 17 keeps its Rust toolchains in prebuilts/rust-toolchain/linux-x86, which the
        # native lock takes in part, and only build files in prebuilts/rust, which it does not name.
        manifest = (
            "<manifest>"
            f'<project path="frameworks/base" name="platform/frameworks/base" revision="{COMMIT}"/>'
            f'<project path="prebuilts/rust" name="platform/prebuilts/rust" revision="{COMMIT}"/>'
            f'<project path="prebuilts/rust-toolchain/linux-x86" '
            f'name="platform/prebuilts/rust-toolchain/linux-x86" revision="{COMMIT}"/>'
            "</manifest>"
        ).encode()
        version = {
            "android": "android17", "android_major": 17, "build_id": "1", "release": "cp2a",
            "manifest": {"url": "", "sha256": hashlib.sha256(manifest).hexdigest()},
            "projects": [{"path": "prebuilts/rust-toolchain/linux-x86", "url": GOOGLESOURCE_CLANG,
                          "commit": COMMIT, "cone": ["{rust}"]}],
        }
        (entry,) = framework_lock.services_lock(version, manifest)["versions"]
        projects = {p["path"]: p for p in entry["projects"]}
        self.assertEqual(projects["prebuilts/rust-toolchain/linux-x86"]["cone"], ["{rust}"])
        self.assertNotIn("cone", projects["prebuilts/rust"])

    def test_fr22_the_services_build_takes_the_published_product_and_no_dexpreopt(self):
        script = (REPO / ".github/scripts/framework/build-services.sh").read_text()
        self.assertIn('lunch "$product-$release-userdebug"', script)
        self.assertIn("export WITH_DEXPREOPT=false", script)
        self.assertNotIn("--native", script)
        self.assertIn("--native", (REPO / ".github/scripts/framework/build.sh").read_text())
        self.assertEqual(framework_lock.JAVA_OUTPUTS,
                         {"SYSTEM/framework/services.jar": "system/framework/services.jar",
                          "SYSTEM/framework/framework.jar": "system/framework/framework.jar"})

    def test_fr22_the_services_tree_leaves_out_what_only_native_builds_read(self):
        # Run 43616908714 ran out of disk: the whole tree was 80 GB of the runner's 145.
        manifest = (
            "<manifest>"
            f'<project path="frameworks/base" name="platform/frameworks/base" revision="{COMMIT}"/>'
            f'<project path="prebuilts/abi-dumps/platform" name="platform/prebuilts/abi-dumps/platform" revision="{COMMIT}"/>'
            f'<project path="prebuilts/vndk/v33" name="platform/prebuilts/vndk/v33" revision="{COMMIT}"/>'
            f'<project path="prebuilts/qemu-kernel" name="platform/prebuilts/qemu-kernel" revision="{COMMIT}"/>'
            f'<project path="prebuilts/android-emulator" name="platform/prebuilts/android-emulator" revision="{COMMIT}"/>'
            f'<project path="prebuilts/sdk" name="platform/prebuilts/sdk" revision="{COMMIT}"/>'
            f'<project path="prebuilts/vndkfoo" name="platform/prebuilts/vndkfoo" revision="{COMMIT}"/>'
            "</manifest>"
        ).encode()
        version = {
            "android": "android16", "android_major": 16, "build_id": "1", "release": "bp4a",
            "manifest": {"url": "", "sha256": hashlib.sha256(manifest).hexdigest()}, "projects": [],
        }
        (entry,) = framework_lock.services_lock(version, manifest)["versions"]
        # The Java build's own prebuilts stay; a path is left out only below a skipped folder.
        self.assertEqual([p["path"] for p in entry["projects"]],
                         ["frameworks/base", "prebuilts/sdk", "prebuilts/vndkfoo"])
        for path in framework_lock.REQUIRED:
            self.assertFalse(framework_lock.services_skipped(path), path)

    def test_fr22_the_services_build_makes_the_dex_jar_not_the_install_path(self):
        # Installing services.jar also built its JNI, native and Rust dependencies (12163 steps,
        # rustc out of disk in run 43616908714). The build makes the file the install copies.
        dex = "out/soong/.intermediates/frameworks/base/services/services/android_common/dex/services.jar"
        query = (
            "out/target/product/vsoc_x86_64_only/system/framework/services.jar:\n"
            "  input: g.android.Cp\n"
            f"    {dex}\n"
            "    | out/host/linux-x86/bin/cp_helper\n"
            "    || out/target/product/vsoc_x86_64_only/system/lib64/libandroid_servers.so\n"
            "  validations:\n"
            "    out/soong/.intermediates/check.txt\n"
            "  outputs:\n"
            "    out/target/product/vsoc_x86_64_only/system/framework/services.jar.installed\n")
        self.assertEqual(framework_lock.install_source(query), dex)
        with self.assertRaisesRegex(framework_lock.LockError, "not a copy"):
            framework_lock.install_source(query.replace("g.android.Cp", "g.java.dexpreopt"))
        with self.assertRaisesRegex(framework_lock.LockError, "not of one jar"):
            framework_lock.install_source(query.replace(f"    {dex}\n", f"    {dex}\n    a.jar\n"))
        with self.assertRaisesRegex(framework_lock.LockError, "no rule"):
            framework_lock.install_source("services.jar:\n  outputs:\n")
        script = (REPO / ".github/scripts/framework/build-services.sh").read_text()
        self.assertIn('build "the analysis of the tree" nothing', script)
        self.assertIn('-t query "$product_out/$installed"', script)
        self.assertIn("framework_lock.py\" install-source", script)
        self.assertIn('build "the services.jar build" "${jars[@]}"', script)
        self.assertIn('cp "${jars[$i]}" "$out/${names[$i]}"', script)
        self.assertNotIn('m -j"$(nproc)" "${targets[@]}"', script)
        # The analysis swap goes back to the disk before the Java build.
        self.assertLess(script.index('build "the analysis of the tree"'), script.index('sudo swapoff "$DSA_ANALYSIS_SWAP"'))
        self.assertLess(script.index('sudo rm -f "$DSA_ANALYSIS_SWAP"'), script.index('build "the services.jar build"'))

    def test_fr42_the_services_tree_builds_the_patched_documentsui_apk(self):
        # DocumentsUI builds only in an Android tree; the services build's tree is the published
        # build's own, so it is patched there before the analysis and its APK built after
        # services.jar.
        apk = "out/soong/.intermediates/packages/apps/DocumentsUI/DocumentsUI/android_common/DocumentsUI.apk"
        query = (
            "out/target/product/vsoc_x86_64_only/system/priv-app/DocumentsUI/DocumentsUI.apk:\n"
            "  input: g.android.Cp\n"
            f"    {apk}\n"
            "    | out/host/linux-x86/bin/cp_helper\n"
            "  outputs:\n"
            "    out/target/product/vsoc_x86_64_only/system/priv-app/DocumentsUI/DocumentsUI.apk.installed\n")
        self.assertEqual(framework_lock.install_source(query, ".apk"), apk)
        with self.assertRaisesRegex(framework_lock.LockError, "not of one APK"):
            framework_lock.install_source(query.replace(apk, apk + ".zip"), ".apk")
        with self.assertRaisesRegex(framework_lock.LockError, "not of one jar"):
            framework_lock.install_source(query)
        script = (REPO / ".github/scripts/framework/build-services.sh").read_text()
        patched = script.index('apps.py" patch-tree documentsui')
        self.assertLess(patched, script.index('build "the analysis of the tree" nothing'))
        self.assertIn('install-source --apk', script)
        self.assertLess(script.index('build "the services.jar build"'), script.index('build "the DocumentsUI build"'))
        self.assertIn('apps.py" record documentsui', script)
        workflow = (REPO / ".github/workflows/framework.yml").read_text()
        self.assertIn("name: default-app-documentsui-${{ matrix.android }}-${{ steps.pins.outputs.version }}", workflow)

    def test_fr22_a_services_build_failure_says_whether_disk_memory_or_time(self):
        script = (REPO / ".github/scripts/framework/build-services.sh").read_text()
        for cause in ("ran out of time", "ran out of disk", "ran out of memory"):
            self.assertIn(f"$what {cause}", script)
        self.assertIn('timeout --signal=TERM --kill-after=120 "$left"', script)
        self.assertIn('minutes="${DSA_BUILD_MINUTES:-280}"', script)
        workflow = (REPO / ".github/workflows/framework.yml").read_text()
        job = workflow[workflow.index("  services:\n"):workflow.index("  framework:\n")]
        step = job[job.index("- name: Build services.jar"):job.index("- name: Keep services.jar")]
        step_limit = int(re.search(r"timeout-minutes: (\d+)", step).group(1))
        budget = int(re.search(r"DSA_BUILD_MINUTES: (\d+)", step).group(1))
        job_limit = int(re.search(r"timeout-minutes: (\d+)", job).group(1))
        # The script stops itself before the step's limit, and the step before the job's (360
        # minutes on a standard runner), leaving room for the fetch.
        self.assertLess(budget + 10, step_limit)
        self.assertLessEqual(step_limit + 45, job_limit)
        self.assertLessEqual(job_limit, 360)

    def test_fr22_the_services_job_fits_a_standard_runner(self):
        workflow = (REPO / ".github/workflows/framework.yml").read_text()
        job = workflow[workflow.index("  services:\n"):workflow.index("  framework:\n")]
        self.assertIn("runs-on: ubuntu-24.04\n", job)
        self.assertIn(".github/scripts/translator/free-disk.sh dsa-services --deep", job)
        self.assertIn('echo "DSA_ANALYSIS_SWAP=$swap_dir/dsa-swap-services" >> "$GITHUB_ENV"', job)
        self.assertEqual(job.count("df -h / /mnt"), 3)
        free_disk = (REPO / ".github/scripts/translator/free-disk.sh").read_text()
        self.assertIn('if [[ "$deep" == --deep ]]; then', free_disk)
        self.assertIn("/usr/lib/llvm-*", free_disk)
        # The other builds keep the plain cleanup.
        self.assertIn("run: .github/scripts/translator/free-disk.sh dsa-framework\n",
                      workflow[workflow.index("  native:\n"):workflow.index("  services:\n")])
        self.assertNotIn("--deep", workflow[workflow.index("  framework:\n"):])

    def test_fr22_the_services_build_leaves_the_runner_room(self):
        # android17's services job lost its runner 50 minutes into the build (run 43637660922).
        script = (REPO / ".github/scripts/framework/build-services.sh").read_text()
        self.assertIn('jobs="${DSA_SERVICES_JOBS:-2}"', script)
        self.assertIn('--make-mode --skip-soong-tests -j"$jobs"', script)
        self.assertNotIn('-j"$(nproc)"', script)
        self.assertIn("export NINJA_HIGHMEM_NUM_JOBS=1", script)
        self.assertIn("export GOMEMLIMIT=", script)
        self.assertIn("choom -n 1000 -- nice -n 19 ionice -c 2 -n 7", script)
        workflow = (REPO / ".github/workflows/framework.yml").read_text()
        job = workflow[workflow.index("  services:\n"):workflow.index("  framework:\n")]
        self.assertIn("DSA_SERVICES_JOBS: 2", job)
        guard = job[job.index("- name: Keep the runner responsive under memory pressure"):
                    job.index("- name: Install the build tools")]
        self.assertIn("/sys/module/zswap/parameters", guard)
        self.assertIn('echo -1000 | sudo tee "/proc/$pid/oom_score_adj"', guard)
        # Before the fetch and the build, which it protects the runner from.
        self.assertLess(job.index("Keep the runner responsive"), job.index("- name: Build services.jar"))

    def test_fr22_the_services_build_runs_in_a_memory_cgroup_of_its_own(self):
        # android17 lost its runner 58 to 62 minutes into the build in about half its attempts
        # (runs 43763452008, 43815264913 and 44797323515). Its analysis holds 19 to 20 GiB, and
        # while it ran in the runner's own cgroup the runner had 0.5 GiB available and waited
        # for memory up to 96 % of the time (run 44677247350).
        workflow = (REPO / ".github/workflows/framework.yml").read_text()
        job = workflow[workflow.index("  services:\n"):workflow.index("  framework:\n")]
        guard = job[job.index("- name: Keep the runner responsive under memory pressure"):
                    job.index("- name: Install the build tools")]
        self.assertIn("cgroup=/sys/fs/cgroup/dsa-build", guard)
        self.assertIn('echo "$high" | sudo tee "$cgroup/memory.high"', guard)
        self.assertIn('echo "DSA_BUILD_CGROUP=$cgroup" >> "$GITHUB_ENV"', guard)
        # What the build may hold is the runner's memory less what the runner keeps: 2.5 GiB.
        self.assertIn("high=$(( (total_kib - 2621440) * 1024 ))", guard)
        # Held back, not killed: no memory.max, so the build waits and its own bounds stop it.
        self.assertNotIn('"$cgroup/memory.max"', guard)
        # A runner without the memory controller fails here, not 60 minutes into the build.
        self.assertIn("this runner has no cgroup v2 memory controller", guard)
        script = (REPO / ".github/scripts/framework/build-services.sh").read_text()
        mover = script[script.index("in_build_cgroup() {"):script.index("\nbuild() {")]
        # The subshell's own process, read outside the pipeline whose sides are processes of
        # their own, joins the cgroup and becomes the build, so everything the build starts is
        # in it, and the script and its per-minute report are not.
        self.assertIn('self=$BASHPID\n    echo "$self" | sudo tee "$DSA_BUILD_CGROUP/cgroup.procs"', mover)
        self.assertIn('exec "$@"', mover)
        self.assertNotIn("in_build_cgroup watch_room", script)
        self.assertEqual(script.count("in_build_cgroup timeout"), 1)
        # The report says what the build holds of its share and whether the runner waits.
        self.assertIn('"$DSA_BUILD_CGROUP/memory.current"', script)
        self.assertIn('"$runner_cgroup/memory.pressure"', script)
        self.assertIn("$(cgroup_line || true)", script)

    def test_fr22_the_services_analysis_is_not_incremental(self):
        # android17's analysis never finished on a standard runner (run 43644988414): its
        # release configuration turns Soong's incremental analysis on, which keeps every
        # module's providers and ninja statements a second time for a later analysis this build
        # never runs.
        script = (REPO / ".github/scripts/framework/build-services.sh").read_text()
        self.assertIn("export SOONG_INCREMENTAL_ANALYSIS=false", script)
        # Before the build system reads it, and so for the analysis and the build alike: a
        # second run with another setting would analyse the tree again.
        self.assertLess(script.index("export SOONG_INCREMENTAL_ANALYSIS=false"), script.index('lunch "$product'))
        self.assertEqual(script.count("export SOONG_INCREMENTAL_ANALYSIS="), 1)
        self.assertIn('grep -q -- "--incremental-build-actions" "$bootstrap"', script)

    SOONG_GO = (
        "package build\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\n"
        "func (pb PrimaryBuilderFactory) primaryBuilderInvocation(config Config) bootstrap.PrimaryBuilderInvocation {\n"
        "\tcommonArgs := make([]string, 0, 0)\n"
        "\tinvocationEnv := make(map[string]string)\n"
        "\tif pb.debugPort != \"\" {\n"
        "\t\tinvocationEnv[\"GODEBUG\"] = \"asyncpreemptoff=1\"\n"
        "\t}\n"
        "\tif profileCpu := os.Getenv(\"SOONG_PROFILE_CPU\"); profileCpu != \"\" {\n"
        "\t}\n"
        "\treturn bootstrap.PrimaryBuilderInvocation{Env: invocationEnv}\n"
        "}\n"
    )

    def test_fr22_the_go_memory_limit_reaches_the_analysis(self):
        # soong_ui starts soong_build with `env -i $env`, so the exported GOMEMLIMIT stopped at
        # soong_ui; the failed run's log shows the command with nothing between `env -i` and
        # the builder.
        changed = framework_lock.soong_memory_limit(self.SOONG_GO)
        lines = changed.splitlines()
        made = lines.index("\tinvocationEnv := make(map[string]string)")
        self.assertEqual(lines[made + 2:made + 5], [
            '\tif limit := os.Getenv("GOMEMLIMIT"); limit != "" {',
            '\t\tinvocationEnv["GOMEMLIMIT"] = limit',
            "\t}",
        ])
        # Nothing else changes, and nothing is lost.
        self.assertEqual(changed.replace(framework_lock.SOONG_MEMORY_LIMIT, ""), self.SOONG_GO)
        # One variable only: the rule writes the map in Go's map order, and a command that
        # differs between the analysis and the build makes ninja analyse the tree twice.
        self.assertEqual(framework_lock.SOONG_MEMORY_LIMIT.count("invocationEnv["), 1)
        # Applying it twice changes nothing more.
        self.assertEqual(framework_lock.soong_memory_limit(changed), changed)
        # A source this was not written against stops the build rather than run without a limit.
        with self.assertRaisesRegex(framework_lock.LockError, "GOMEMLIMIT cannot be handed on"):
            framework_lock.soong_memory_limit(self.SOONG_GO.replace("invocationEnv := make", "env := make"))
        with self.assertRaisesRegex(framework_lock.LockError, "GOMEMLIMIT cannot be handed on"):
            framework_lock.soong_memory_limit(self.SOONG_GO.replace('\t"os"\n', ""))
        with self.assertRaisesRegex(framework_lock.LockError, "GOMEMLIMIT cannot be handed on"):
            framework_lock.soong_memory_limit(self.SOONG_GO + framework_lock.SOONG_INVOCATION_ENV)

    def test_fr22_the_services_build_hands_the_memory_limit_on_before_it_starts(self):
        script = (REPO / ".github/scripts/framework/build-services.sh").read_text()
        change = 'framework_lock.py" soong-memory-limit "$top/' + framework_lock.SOONG_UI_SOURCE + '"'
        self.assertIn(change, script)
        # soong_ui is compiled from the tree when the first build step starts.
        self.assertLess(script.index(change), script.index('build "the analysis of the tree"'))
        self.assertIn('grep -q "GOMEMLIMIT=$GOMEMLIMIT" "$bootstrap"', script)
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "soong.go"
            source.write_text(self.SOONG_GO)
            run = [sys.executable, str(HERE / "framework_lock.py"), "soong-memory-limit", str(source)]
            subprocess.run(run, check=True, capture_output=True)
            self.assertIn(framework_lock.SOONG_MEMORY_LIMIT, source.read_text())
            source.write_text("package build\n")
            failed = subprocess.run(run, capture_output=True, text=True)
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("GOMEMLIMIT cannot be handed on", failed.stderr)
            self.assertEqual(source.read_text(), "package build\n")

    def test_fr22_the_services_build_says_what_it_has_of_the_runner_every_minute(self):
        # The run that lost its runner printed nothing for 58 minutes, so nothing said whether
        # the memory or the disk had run out.
        script = (REPO / ".github/scripts/framework/build-services.sh").read_text()
        for source in ("/proc/meminfo", '"/proc/pressure/$what"', 'df -BM --output=avail "$top"',
                       "ps -eo rss=,pid=,comm= --sort=-rss", "VmSwap"):
            self.assertIn(source, script)
        self.assertIn('local every="${DSA_ROOM_SECONDS:-60}"', script)
        # It starts before the analysis and ends with the script, however that ends.
        self.assertLess(script.index("watch_room &\n"), script.index('build "the analysis of the tree"'))
        self.assertIn("trap 'kill \"$watcher\" 2>/dev/null || true' EXIT", script)
        # It runs outside the build's lowest priority, which is given to soong_ui alone.
        self.assertNotIn("nice -n 19 watch_room", script)

    def test_fr22_an_analysis_that_does_not_fit_is_stopped_while_the_runner_answers(self):
        script = (REPO / ".github/scripts/framework/build-services.sh").read_text()
        self.assertIn('analysis_minutes="${DSA_ANALYSIS_MINUTES:-90}"', script)
        self.assertIn('step_minutes="$analysis_minutes"\nbuild "the analysis of the tree" nothing\nstep_minutes=0\n',
                      script)
        self.assertIn("$what ran out of time: it was stopped after $stopped_after", script)
        workflow = (REPO / ".github/workflows/framework.yml").read_text()
        job = workflow[workflow.index("  services:\n"):workflow.index("  framework:\n")]
        bound = int(re.search(r"DSA_ANALYSIS_MINUTES: (\d+)", job).group(1))
        budget = int(re.search(r"DSA_BUILD_MINUTES: (\d+)", job).group(1))
        # Several times android17's 19 to 23 minutes, since the build's cgroup gives the analysis
        # less memory than it had and keeps the runner answering however long it takes. And
        # with the longest Java build after it so far (92 minutes, run 43815264913), within the
        # time the job gives the whole build.
        self.assertGreaterEqual(bound, 60)
        self.assertLessEqual(bound + 92, budget)
        # The runner answers only because the build runs in its own cgroup.
        self.assertIn('in_build_cgroup timeout --signal=TERM --kill-after=120 "$left"', script)

    def test_fr22_the_services_tree_takes_one_version_of_the_musl_rust_toolchain(self):
        # 6 GB for three versions, of the 17 GB the fetch leaves android17 (run 43644988414).
        musl = "prebuilts/rust-toolchain/linux-musl-x86"
        manifest = (
            "<manifest>"
            f'<project path="frameworks/base" name="platform/frameworks/base" revision="{COMMIT}"/>'
            f'<project path="prebuilts/rust-toolchain/linux-x86" '
            f'name="platform/prebuilts/rust-toolchain/linux-x86" revision="{COMMIT}"/>'
            f'<project path="{musl}" name="platform/{musl}" revision="{COMMIT}"/>'
            "</manifest>"
        ).encode()
        version = {
            "android": "android17", "android_major": 17, "build_id": "1", "release": "cp2a",
            "manifest": {"url": "", "sha256": hashlib.sha256(manifest).hexdigest()},
            "projects": [{"path": "prebuilts/rust-toolchain/linux-x86", "url": GOOGLESOURCE_CLANG,
                          "commit": COMMIT, "cone": ["{rust}"]}],
        }
        (entry,) = framework_lock.services_lock(version, manifest)["versions"]
        projects = {p["path"]: p for p in entry["projects"]}
        # Still fetched, with its build file (a cone keeps the files of the project's root), so
        # the analysis reads what it read before.
        self.assertEqual(projects[musl]["cone"], ["{rust}"])
        self.assertEqual(projects[musl]["url"], "https://android.googlesource.com/platform/" + musl)
        self.assertFalse(framework_lock.services_skipped(musl))
        # Without a native lock that says which part, it is taken whole, as before.
        version["projects"] = []
        (entry,) = framework_lock.services_lock(version, manifest)["versions"]
        self.assertNotIn("cone", {p["path"]: p for p in entry["projects"]}[musl])
        # The real lock says which part for Android 17, and Android 16's manifest has no such
        # project, so its tree is the one that already builds.
        lock = framework_lock.load()
        native = {p["path"]: p for p in framework_lock.version_entry(lock, "android17")["projects"]}
        self.assertEqual(native[framework_lock.SERVICES_TREE_CONES_LIKE[musl]]["cone"], ["{rust}"])

    def test_fr22_the_framework_artifact_carries_services_jar_from_the_services_build(self):
        workflow = (REPO / ".github/workflows/framework.yml").read_text()
        self.assertIn("build-services.sh", workflow)
        self.assertIn("framework_lock.py services-lock", workflow)
        native = workflow[workflow.index("  native:\n"):workflow.index("  services:\n")]
        pack = workflow[workflow.index("  framework:\n"):]
        # The native builds start with the services builds, not after them.
        self.assertIn("    needs: versions\n", native)
        self.assertNotIn("--services", native)
        self.assertIn("name: framework-native-${{ matrix.android }}-${{ matrix.arch }}-${{ steps.pins.outputs.version }}",
                      native)
        # Only the job after both keeps the artifact the image release takes.
        self.assertNotIn("name: framework-${{ matrix.android }}", native)
        self.assertIn("needs: [versions, native, services]", pack)
        self.assertIn("if: ${{ !cancelled() && needs.versions.result == 'success' }}", pack)
        self.assertIn('--add-services "$RUNNER_TEMP/services" --out "$out"', pack)
        self.assertLess(pack.index("- name: Check the native build kept its files"),
                        pack.index("- name: Keep the framework files"))

    def test_fr22_services_jar_added_later_makes_the_same_folder(self):
        version = framework_lock.version_entry(framework_lock.load(), "android17")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            collect_tests = CollectTests()
            services = root / "services"
            write_java_outputs(services)
            product, top = collect_tests._product(root, 62), collect_tests._tree(root)
            sources, patches = {"projects": 1}, {"patches": 2}
            collect.collect(version, "x86_64", product, root / "once", sources, patches, top, services)
            collect.collect(version, "x86_64", product, root / "later", sources, patches, top)
            description = collect.add_services(root / "later", services)
            for name in framework_lock.JAVA_OUTPUTS:
                self.assertIn(name, description["files"])
            for name in [collect.MANIFEST, *framework_lock.OUTPUTS, *framework_lock.JAVA_OUTPUTS]:
                self.assertEqual((root / "later" / name).read_bytes(), (root / "once" / name).read_bytes(), name)
            with self.assertRaisesRegex(SystemExit, "already holds"):
                collect.add_services(root / "later", services)
            with self.assertRaisesRegex(SystemExit, "has no framework.json"):
                collect.add_services(root / "empty", services)

    def test_fr22_collect_takes_services_jar_from_the_services_build(self):
        version = framework_lock.version_entry(framework_lock.load(), "android17")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            collect_tests = CollectTests()
            services = root / "services"
            write_java_outputs(services)
            jar = services / framework_lock.SERVICES_JAR
            description = collect.collect(version, "x86_64", collect_tests._product(root, 62), root / "out",
                                          top=collect_tests._tree(root), services=services)
            for name in framework_lock.JAVA_OUTPUTS:
                self.assertIn(name, description["files"])
                self.assertEqual((root / "out" / name).read_bytes(), (services / name).read_bytes(), name)
            # Without the services build's folder there is no services.jar, as before.
            description = collect.collect(version, "x86_64", root / "product", root / "out2", top=root / "tree")
            self.assertNotIn(framework_lock.SERVICES_JAR, description["files"])
            self.assertNotIn(framework_lock.FRAMEWORK_JAR, description["files"])
            jar.write_bytes(b"not a jar")
            with self.assertRaisesRegex(SystemExit, "not a jar with dex code"):
                collect.collect(version, "x86_64", root / "product", root / "out3", top=root / "tree",
                                services=services)

    def test_fr4_framework_jar_comes_with_services_jar_or_not_at_all(self):
        # The window manager in services.jar calls framework.jar's new code, so a services build
        # that left one without the other is refused rather than packed.
        version = framework_lock.version_entry(framework_lock.load(), "android17")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            collect_tests = CollectTests()
            services = root / "services"
            write_java_outputs(services)
            (services / framework_lock.FRAMEWORK_JAR).unlink()
            with self.assertRaisesRegex(SystemExit, "the services build left no SYSTEM/framework/framework.jar"):
                collect.collect(version, "x86_64", collect_tests._product(root, 62), root / "out",
                                top=collect_tests._tree(root), services=services)


GOOGLESOURCE_CLANG = "https://android.googlesource.com/platform/prebuilts/clang/host/linux-x86"


class KeyMintLockTests(unittest.TestCase):
    """What the Framework build needs to build DSA's KeyMint, and the patches that make it take
    the root key of key blobs from the host."""

    HOST_KEY = "security/keymint/aidl/default/ta/host_key.rs"

    def _patch(self, project, android):
        found = [patch for p, patch in framework_lock.patch_files(android) if p == project]
        self.assertEqual(len(found), 1, f"{android} {project}")
        return found[0].read_text()

    def test_fr36_the_build_makes_the_keymint_the_image_takes(self):
        outputs = subprocess.run([sys.executable, "-I", str(HERE / "framework_lock.py"), "outputs"],
                                 capture_output=True, text=True, check=True).stdout.split()
        self.assertEqual(outputs, list(framework_lock.OUTPUTS.values()) + list(framework_lock.KEYMINT_OUTPUTS.values()))
        self.assertIn("vendor/bin/hw/dsa-keymint", outputs)
        # The image's vendor partition has no libssl, which the Rust openssl crate links.
        self.assertIn("vendor/lib64/libssl.so", outputs)
        for board in ("x86_64", "arm64"):
            config = (REPO / ".github/scripts/framework/device" / board / "BoardConfig.mk").read_text()
            self.assertRegex(config, r"(?m)^TARGET_COPY_OUT_VENDOR := vendor$", board)

    def test_fr36_the_lock_fetches_what_the_keymint_is_built_from(self):
        crates = ["ciborium", "ciborium-io", "ciborium-ll", "coset", "der", "der_derive", "const-oid", "spki",
                  "pkcs1", "pkcs8", "sec1", "x509-cert", "flagset", "zeroize", "zeroize_derive", "enumn", "hex",
                  "half", "foreign-types", "foreign-types-shared", "openssl-macros", "android_logger", "log",
                  "libc", "nix", "regex", "zerocopy", "downcast-rs", "thiserror",
                  # The AIDL Rust backend gives every generated interface crate libasync_trait,
                  # for its async traits. Without it the first KeyMint Framework build stopped on
                  # the KeyMint, RKP, secure clock and shared secret interface crates.
                  "async-trait"]
        for version in framework_lock.load()["versions"]:
            projects = {p["path"]: p for p in version["projects"]}
            for whole in ("system/keymint", "external/rust/crates/openssl"):
                self.assertNotIn("cone", projects[whole], f"{version['android']} {whole}")
                self.assertNotIn("patterns", projects[whole], f"{version['android']} {whole}")
            for folder in ("security/keymint/aidl", "security/rkp/aidl", "security/secureclock/aidl",
                           "security/sharedsecret/aidl"):
                self.assertIn(folder, projects["hardware/interfaces"]["cone"], version["android"])
            self.assertIn("src/rust/bssl-sys", projects["external/boringssl"]["cone"], version["android"])
            self.assertIn("libs/binder", projects["frameworks/native"]["cone"], version["android"])
            for crate in crates:
                self.assertIn("crates/" + crate, projects["external/rust/android-crates-io"]["cone"],
                              f"{version['android']} {crate}")

    def test_fr36_both_versions_take_the_root_key_from_the_host(self):
        sources = set()
        for android in ("android16", "android17"):
            patch = self._patch("hardware/interfaces", android)
            match = re.search(r"^\+\+\+ b/" + re.escape(self.HOST_KEY) + r"\n@@ [^\n]+\n((?:\+[^\n]*\n)+)", patch, re.M)
            self.assertIsNotNone(match, android)
            source = "".join(line[1:] + "\n" for line in match.group(1).splitlines())
            sources.add(source)
            self.assertIn('pub const HOST_KEY_FILE: &str = "/dev/dsa_keymint/root_key";', source)
            self.assertIn('pub const HOST_KEK_CONTEXT: &[u8] = b"dsa-host-root-key-v1";', source)
            self.assertIn("Duration::from_secs(30)", source)
            self.assertIn("fn kek_context_is_outdated", source)
            # The TA is built with it, and the image's binary is built on its own, with its Rust
            # crates linked in; the APEX the tree cannot build goes.
            self.assertIn("+        Box::new(host_key::HostKeys::new());", patch)
            self.assertRegex(patch, r'\+rust_binary \{\n\+    name: "dsa-keymint",\n\+    relative_install_path: "hw",'
                                    r'\n\+    vendor: true,')
            binary = patch[patch.index('+    name: "dsa-keymint",'):]
            self.assertIn("+    prefer_rlib: true,", binary[:binary.index("+}")])
            self.assertIn('-    name: "com.android.hardware.keymint.rust_nonsecure",', patch)
        # One implementation for both versions.
        self.assertEqual(len(sources), 1)

    def test_fr36_android16_gets_the_ta_hook_android17_has(self):
        # Android 17's system/keymint asks the device whether a blob's KEK context is outdated
        # and answers KEY_REQUIRES_UPGRADE for it; Android 16's does not, so DSA's patch brings
        # the same code there.
        patch = self._patch("system/keymint", "android16")
        for text in ("+    fn kek_context_is_outdated(&self, _kek_context: &[u8]) -> Result<bool, Error> {",
                     '+            return Err(km_err!(KeyRequiresUpgrade, "KEK context outdated, request upgrade"));',
                     "+                Ok(result) => (result.keyblob, result.kek_context_is_outdated),"):
            self.assertIn(text, patch)
        self.assertFalse([p for p, _ in framework_lock.patch_files("android17") if p == "system/keymint"])


class WorkflowTests(unittest.TestCase):
    """How the Framework build keeps its compiler caches and how the image release finds its
    files: the build is two hours cold and twenty minutes warm, and a development build waits
    on it."""

    def setUp(self):
        self.framework = (REPO / ".github/workflows/framework.yml").read_text(encoding="utf-8")
        self.release = (REPO / ".github/workflows/release-image.yml").read_text(encoding="utf-8")
        self.build = (REPO / ".github/scripts/framework/build.sh").read_text(encoding="utf-8")

    def _step(self, text, name):
        start = text.index(f"- name: {name}\n")
        end = text.find("\n      - ", start + 1)
        return text[start:end if end != -1 else len(text)]

    def test_nfr5_a_development_build_waits_only_for_the_job_it_takes_files_from(self):
        find = self._step(self.release, "Find the Framework build run with the pinned files")
        dev = find[find.index('if [[ "$DEV_BUILD" == true ]]'):find.index("wanted=()")]
        # The newest supported Android's files of the architecture asked for, from the job
        # framework.yml names so.
        self.assertIn('android="android$(jq \'.supported_majors | max\' dsa/android-versions.json)"', dev)
        self.assertIn('name="framework-$android-$DEV_ARCH-', dev)
        # The job that keeps the artifact once services.jar has joined the native files.
        self.assertIn('job="Pack the framework files (ubuntu-24.04, $android, $DEV_ARCH)"', dev)
        pack = self.framework[self.framework.index("  framework:\n"):]
        self.assertIn("name: Pack the framework files (ubuntu-24.04, ${{ matrix.android }}, ${{ matrix.arch }})",
                      pack)
        self.assertIn("name: framework-${{ matrix.android }}-${{ matrix.arch }}-${{ steps.pins.outputs.version }}",
                      pack)
        # Its job's conclusion, not the run's: the run may still be going or have failed.
        self.assertIn('[[ "$conclusion" == success ]] || continue', dev)
        self.assertNotIn("--status success", dev)
        # A release still takes every version and architecture from one passing run.
        release = find[find.index("wanted=()"):]
        self.assertIn("--status success", release)
        self.assertIn("for arch in x86_64 arm64; do", release)

    def test_nfr5_the_image_downloads_the_framework_while_its_run_still_goes(self):
        download = self._step(self.release, "Download the patched framework")
        self.assertNotIn('gh run download "', download)
        self.assertIn("/actions/artifacts/$id/zip", download)

    def test_nfr5_the_newest_android_and_x86_64_build_first(self):
        self.assertIn("sort_by(ltrimstr(\"android\") | tonumber) | reverse", self.framework)
        self.assertIn("arches='[\"x86_64\",\"arm64\"]'", self.framework)

    def test_nfr5_the_compiler_cache_is_kept_as_an_artifact_and_restored_before_the_build(self):
        steps = [line.strip()[len("- name: "):] for line in self.framework.splitlines()
                 if line.strip().startswith("- name: ")]
        order = ["Restore the compiler cache", "Build SurfaceFlinger, libhwui, libEGL and the KeyMint",
                 "Pack the compiler cache", "Keep the compiler cache", "Delete the compiler caches it replaces"]
        self.assertEqual([s for s in steps if s in order], order)
        keep = self._step(self.framework, "Keep the compiler cache")
        self.assertIn("uses: actions/upload-artifact@v4", keep)
        self.assertIn("name: framework-ccache-${{ matrix.android }}-${{ matrix.arch }}\n", keep)
        self.assertIn("compression-level: 0", keep)
        # Nothing is saved to the Actions cache any more, where GitHub evicted it.
        self.assertNotIn("actions/cache/save", self.framework)
        # An empty cache from a build that never ran must not replace a full one.
        self.assertIn("steps.build.outcome", self._step(self.framework, "Pack the compiler cache"))

    def test_nfr5_the_build_reports_its_compiler_cache_hits(self):
        self.assertIn("ccache --zero-stats", self.build)
        self.assertIn("ccache --show-stats --verbose", self.build)
        self.assertIn("-o max_size=3G", self.build)


@unittest.skipIf(sys.platform == "win32", "runs the shell script with a fake gh and curl")
class CcacheArtifactTests(unittest.TestCase):
    SCRIPT = REPO / ".github/scripts/framework/ccache-artifact.sh"

    def _run(self, root, listing, *args, branch="feat/x"):
        root = Path(root)
        bin_dir = root / "bin"
        bin_dir.mkdir(exist_ok=True)
        (root / "listing.tsv").write_text(listing)
        log = root / "calls.log"
        # gh prints the listing as the script's own jq would; curl hands out a zip holding
        # ccache.tar with one file, named after the artifact it was asked for.
        (bin_dir / "gh").write_text(
            "#!/bin/sh\n"
            f'echo "gh $*" >> "{log}"\n'
            'case "$*" in *DELETE*) exit 0 ;; esac\n'
            f'cat "{root}/listing.tsv"\n')
        (bin_dir / "curl").write_text(
            "#!/bin/sh\n"
            f'echo "curl $*" >> "{log}"\n'
            'out=""; prev=""; url=""\n'
            'for a in "$@"; do [ "$prev" = "-o" ] && out="$a"; prev="$a"; url="$a"; done\n'
            'id="$(echo "$url" | sed -E \'s#.*/artifacts/([0-9]+)/zip#\\1#\')"\n'
            f'w="{root}/pack-$id"; mkdir -p "$w/c"; echo "$id" > "$w/c/from"\n'
            'tar -C "$w/c" -cf "$w/ccache.tar" . && (cd "$w" && zip -q "$out" ccache.tar)\n')
        for tool in ("gh", "curl"):
            (bin_dir / tool).chmod(0o755)
        env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}", GH_TOKEN="t",
                   GITHUB_REPOSITORY="o/r", GITHUB_REF_NAME=branch, GITHUB_OUTPUT=str(root / "out"))
        env.pop("GITHUB_HEAD_REF", None)
        result = subprocess.run(["bash", str(self.SCRIPT), *args], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = log.read_text() if log.exists() else ""
        out = (root / "out").read_text() if (root / "out").exists() else ""
        return calls, out

    LISTING = ("30\t300\tdevelop\t2026-11-14T09:00:00Z\n"
               "20\t200\tfeat/x\t2026-11-13T09:00:00Z\n"
               "10\t100\tdevelop\t2026-11-12T09:00:00Z\n")

    @unittest.skipUnless(subprocess.run(["sh", "-c", "command -v zip unzip"], capture_output=True).returncode == 0,
                         "needs zip and unzip")
    def test_nfr5_a_branch_restores_its_own_cache_before_develops(self):
        with tempfile.TemporaryDirectory() as root:
            cache = Path(root, "ccache")
            _, out = self._run(root, self.LISTING, "restore", "framework-ccache-android17-x86_64",
                               str(cache), str(Path(root, "dl")))
            self.assertIn("found=true", out)
            self.assertEqual((cache / "from").read_text().strip(), "20")

    @unittest.skipUnless(subprocess.run(["sh", "-c", "command -v zip unzip"], capture_output=True).returncode == 0,
                         "needs zip and unzip")
    def test_nfr5_develop_restores_its_newest_cache_whatever_lock_made_it(self):
        with tempfile.TemporaryDirectory() as root:
            cache = Path(root, "ccache")
            _, out = self._run(root, self.LISTING, "restore", "framework-ccache-android17-x86_64",
                               str(cache), str(Path(root, "dl")), branch="develop")
            self.assertIn("found=true", out)
            self.assertEqual((cache / "from").read_text().strip(), "30")

    def test_nfr5_no_cache_to_restore_is_a_cold_build_not_a_failure(self):
        with tempfile.TemporaryDirectory() as root:
            calls, out = self._run(root, "", "restore", "framework-ccache-android17-x86_64",
                                   str(Path(root, "ccache")), str(Path(root, "dl")))
            self.assertIn("found=false", out)
            self.assertNotIn("curl", calls)

    def test_nfr5_only_develops_older_caches_are_deleted(self):
        with tempfile.TemporaryDirectory() as root:
            calls, _ = self._run(root, self.LISTING, "prune", "framework-ccache-android17-x86_64", "30")
            deleted = [line for line in calls.splitlines() if "DELETE" in line]
            self.assertEqual(deleted, ["gh api -X DELETE repos/o/r/actions/artifacts/10"])


if __name__ == "__main__":
    unittest.main()
