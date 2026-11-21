"""Tests for the default apps' pins, patches and build records (apps.json, apps.py)."""

import importlib.util
import json
import re
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("apps", HERE / "apps.py")
apps = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(apps)

INPUTS = HERE.parents[1] / "dsa/guest/image/inputs.json"
SHA = re.compile(r"^[0-9a-f]{40}$")
# The AOSP package each default app takes, so it stands where the AOSP app stood.
AOSP_PACKAGES = {
    "deskclock": "com.android.deskclock",
    "gallery": "com.android.gallery3d",
    "camera": "com.android.camera2",
    "music": "com.android.music",
    "calculator": "com.android.calculator2",
    "recorder": "com.android.soundrecorder",
    "documentsui": "com.android.documentsui",
}
# The application ID each LineageOS app has upstream, which its first patch replaces.
UPSTREAM_IDS = {
    "gallery": "org.lineageos.glimpse",
    "camera": "org.lineageos.aperture",
    "music": "org.lineageos.twelve",
    "recorder": "org.lineageos.recorder",
}


def changed_files(patch):
    return re.findall(r"(?m)^\+\+\+ b/(\S+)$", patch.read_text())


class DefaultAppsTests(unittest.TestCase):
    def setUp(self):
        self.manifest = apps.load()

    def test_fr42_the_seven_apps_take_the_aosp_package_names(self):
        self.assertEqual({a["name"]: a["package"] for a in self.manifest["apps"]}, AOSP_PACKAGES)

    def test_fr42_every_lineageos_app_is_pinned_to_a_commit_of_its_branch(self):
        self.assertEqual(self.manifest["branch"], "lineage-24.0")
        for app in self.manifest["apps"]:
            if app["name"] == "documentsui":
                continue
            with self.subTest(app=app["name"]):
                self.assertRegex(app["url"], r"^https://github\.com/LineageOS/android_packages_apps_\w+$")
                self.assertRegex(app["commit"], SHA)
                self.assertEqual(app["build"]["kind"], "gradle")

    def test_fr42_documentsui_is_pinned_for_every_android_version_the_image_is_made_of(self):
        app = apps.app_entry("documentsui", self.manifest)
        self.assertEqual(app["url"], "https://android.googlesource.com/platform/packages/apps/DocumentsUI")
        self.assertEqual(app["build"]["kind"], "aosp")
        majors = {f"android{i['android_major']}" for i in json.loads(INPUTS.read_text())["images"]}
        self.assertEqual(set(app["commits"]), majors)
        for commit in app["commits"].values():
            self.assertRegex(commit, SHA)
        self.assertEqual(apps.pinned_commit(app, "android17"), app["commits"]["android17"])
        with self.assertRaises(apps.AppsError):
            apps.pinned_commit(app, "android15")

    def test_fr42_every_app_has_patches_that_stay_inside_it(self):
        for app in self.manifest["apps"]:
            with self.subTest(app=app["name"]):
                found = apps.patches(app["name"])
                self.assertTrue(found, f"{app['name']} has no patches")
                for patch in found:
                    files = changed_files(patch)
                    self.assertTrue(files, f"{patch.name} changes nothing")
                    for name in files:
                        self.assertFalse(name.startswith("/") or ".." in Path(name).parts, name)

    def test_fr42_the_first_patch_gives_a_lineageos_app_its_aosp_package_name(self):
        for name, upstream in UPSTREAM_IDS.items():
            with self.subTest(app=name):
                first = apps.patches(name)[0]
                self.assertEqual(first.name, "0001-use-the-aosp-package-name.patch")
                text = first.read_text()
                self.assertIn(f'-        applicationId = "{upstream}"', text)
                self.assertIn(f'+        applicationId = "{AOSP_PACKAGES[name]}"', text)
        # DeskClock and ExactCalculator already have the AOSP names upstream.
        for name in ("deskclock", "calculator"):
            self.assertNotIn("applicationId", "".join(p.read_text() for p in apps.patches(name)))

    def test_fr42_every_app_is_patched_to_follow_the_design(self):
        for app in self.manifest["apps"]:
            with self.subTest(app=app["name"]):
                design = [p for p in apps.patches(app["name"]) if p.name.endswith("-follow-the-dsa-design.patch")]
                self.assertEqual(len(design), 1)
                text = design[0].read_text()
                self.assertIn("@style/DsaShape.Medium", text)
                self.assertIn("@style/DsaCollapsingToolbar", text)

    def test_fr42_an_app_with_a_title_area_starts_it_folded_in_a_low_window(self):
        for name in ("deskclock", "gallery", "camera", "music", "recorder", "documentsui"):
            with self.subTest(app=name):
                text = "".join(p.read_text() for p in apps.patches(name))
                self.assertIn("io.github.thisisthepy.dsa.design.DsaDesign.", text)
        helper = (apps.DESIGN / "java/io/github/thisisthepy/dsa/design/DsaDesign.java").read_text()
        self.assertIn("LARGE_TITLE_MIN_HEIGHT_DP = 600", helper)
        self.assertIn("return heightDp >= LARGE_TITLE_MIN_HEIGHT_DP;", helper)

    def test_fr42_settings_lists_are_grouped_into_cards(self):
        for app in self.manifest["apps"]:
            with self.subTest(app=app["name"]):
                targets = [str(t.as_posix()) for _, t in apps.design_files(app)]
                java = app["build"]["java"]
                self.assertIn(f"{java}/io/github/thisisthepy/dsa/design/CardGroups.java", targets)
                self.assertIn(f"{app['build']['res']}/values/dsa_design.xml", targets)
                has_preference_cards = f"{java}/io/github/thisisthepy/dsa/design/PreferenceCards.java" in targets
                self.assertEqual(has_preference_cards, app["build"]["preference"])
                if app["build"]["preference"]:
                    text = "".join(p.read_text() for p in apps.patches(app["name"]))
                    self.assertIn("PreferenceCards.attach(", text)

    def test_fr42_the_record_names_the_pin_and_every_patch_and_design_file(self):
        app = apps.app_entry("camera", self.manifest)
        record = apps.provenance(app)
        self.assertEqual(record["package"], "com.android.camera2")
        self.assertEqual(record["commit"], app["commit"])
        self.assertEqual(set(record["patches"]), {p.name for p in apps.patches("camera")})
        self.assertEqual(len(record["design"]), len(apps.design_files(app)))
        with tempfile.TemporaryDirectory() as tmp:
            apk = Path(tmp) / "app-release-unsigned.apk"
            apk.write_bytes(b"PK")
            out = Path(tmp) / "out"
            target = apps.write_record(app, apk, out)
            self.assertEqual(target.name, "DsaCamera.apk")
            written = json.loads((out / "camera.json").read_text())
            self.assertEqual(written["apk"], "DsaCamera.apk")
            self.assertEqual(written["image"], app["image"])

    def test_fr42_documentsui_gets_its_material3_flag_turned_on(self):
        app = apps.app_entry("documentsui", self.manifest)
        with tempfile.TemporaryDirectory() as tmp:
            folder = Path(tmp) / "build/release/aconfig/cp2a/com.android.documentsui.flags"
            with self.assertRaises(apps.AppsError):
                apps.set_flags(app, tmp, "cp2a")
            folder.mkdir(parents=True)
            (folder / "Android.bp").write_text("aconfig_values {}\n")
            written = apps.set_flags(app, tmp, "cp2a")
            self.assertEqual([p.name for p in written], ["use_material3_flag_values.textproto"])
            text = written[0].read_text()
            self.assertIn('name: "use_material3"', text)
            self.assertIn("state: ENABLED", text)

    def test_fr42_patch_tree_refuses_a_tree_at_another_commit(self):
        with tempfile.TemporaryDirectory() as tmp:
            lock = Path(tmp) / "services-lock.json"
            lock.write_text(json.dumps({"versions": [{"projects": [
                {"path": "packages/apps/DocumentsUI", "commit": "0" * 40}]}]}))
            with self.assertRaises(apps.AppsError):
                apps.patch_tree("documentsui", tmp, "android17", "cp2a", lock)

    def test_fr42_gradle_and_tree_builds_are_not_mixed_up(self):
        with self.assertRaises(apps.AppsError):
            apps.fetch("documentsui")
        with self.assertRaises(apps.AppsError):
            apps.patch_tree("music", "/nowhere", "android17", "cp2a")

    def test_fr42_each_app_has_its_own_place_in_the_image(self):
        places = [a["image"] for a in self.manifest["apps"]]
        self.assertEqual(len(places), len(set(places)))
        for app in self.manifest["apps"]:
            folder = Path(app["image"]).parent.name
            for replaced in app["replaces"]:
                self.assertNotEqual(folder, replaced)


if __name__ == "__main__":
    unittest.main()
