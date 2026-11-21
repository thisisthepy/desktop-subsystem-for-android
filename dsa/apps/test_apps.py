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
        # The default apps; the system screens have tests of their own (SystemScreensTests).
        self.manifest = apps.load()
        self.manifest["apps"] = [a for a in self.manifest["apps"] if a.get("role") != "system"]

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


# The image's own build manifests pin these; DocumentsUI's pins come from the same manifests.
FRAMEWORKS_BASE = {"android16": "45034f0663f960d9ee5fb0a101a4732b71f6e2f4",
                   "android17": "94b4c163b7dfe5ce3607f7bb8456f9573f7de57d"}


class SystemScreensTests(unittest.TestCase):
    def setUp(self):
        self.manifest = apps.load()
        self.screens = {a["name"]: a for a in self.manifest["apps"] if a.get("role") == "system"}

    def test_fr34_every_system_screen_is_built_in_the_tree_at_the_images_pins(self):
        self.assertEqual(set(self.screens), {"settings", "permissioncontroller", "packageinstaller", "intentresolver"})
        majors = {f"android{i['android_major']}" for i in json.loads(INPUTS.read_text())["images"]}
        for name, app in self.screens.items():
            with self.subTest(app=name):
                self.assertEqual(app["build"]["kind"], "aosp")
                self.assertEqual(set(app["commits"]), majors)
                for commit in app["commits"].values():
                    self.assertRegex(commit, SHA)
                self.assertEqual(app["certificate"], "platform")
                self.assertEqual(app["replaces"], [])
                self.assertTrue(app["build"]["tree_path"].startswith(app["build"]["project"]))
                # Its install path in the tree, or for an app inside an APEX its APK target.
                self.assertEqual(("installed" in app["build"]), ("apex" not in app["build"]))
        self.assertEqual(self.screens["packageinstaller"]["commits"], FRAMEWORKS_BASE)
        lib = apps.app_entry("settingslib", self.manifest)
        self.assertTrue(apps.is_library(lib))
        self.assertEqual(lib["commits"], FRAMEWORKS_BASE)

    def test_fr34_every_system_screen_is_patched_to_follow_the_design(self):
        for name in ("settingslib", *self.screens):
            with self.subTest(app=name):
                design = [p for p in apps.patches(name) if p.name.endswith("-follow-the-dsa-design.patch")]
                self.assertEqual(len(design), 1)
                for changed in changed_files(design[0]):
                    self.assertFalse(changed.startswith("/") or ".." in Path(changed).parts, changed)
        lib = "".join(p.read_text() for p in apps.patches("settingslib"))
        # The large title opens expanded in a tall window and folded in a low one, sub-pages too.
        self.assertIn("DsaDesign.showLargeTitle(mAppBarLayout)", lib)
        self.assertIn("DsaDesign\n+                                            .largeTitleStartsExpanded(activity), true)", lib)
        self.assertIn("-                            appBarLayout.post(() -> appBarLayout.setExpanded(false, true));", lib)
        # Bold, variable-weight titles that shrink into the toolbar.
        self.assertIn("'wght' 700", lib)
        self.assertIn('+        <item name="titleCollapseMode">scale</item>', lib)
        settings = "".join(p.read_text() for p in apps.patches("settings"))
        self.assertIn("DsaDesign\n+                .largeTitleStartsExpanded(newConfig.screenHeightDp)", settings)
        self.assertIn("+            recreate();", settings)
        installer = "".join(p.read_text() for p in apps.patches("packageinstaller"))
        self.assertIn("fun isMaterialDesignEnabled(context: Context): Boolean {\n+", installer)
        self.assertIn("+        return true", installer)
        permissions = "".join(p.read_text() for p in apps.patches("permissioncontroller"))
        self.assertIn('+        <item name="android:textStyle">bold</item>', permissions)
        self.assertNotIn("+        <item name=\"android:fontFamily\">google-sans", permissions)
        self.assertIn("TextAppearance.Dsa.ChooserTitle", "".join(p.read_text() for p in apps.patches("intentresolver")))

    def test_fr34_only_settingslib_carries_the_design_helper(self):
        # Settings and PermissionController take DsaDesign from SettingsLib; a second copy of the
        # class in the same APK would not build.
        lib = apps.app_entry("settingslib", self.manifest)
        self.assertEqual([str(t.as_posix()) for _, t in apps.design_files(lib)],
                         ["CollapsingToolbarBaseActivity/src/io/github/thisisthepy/dsa/design/DsaDesign.java"])
        for name, app in self.screens.items():
            with self.subTest(app=name):
                self.assertEqual(apps.design_files(app), [])
        self.assertEqual(self.screens["settings"]["libraries"], ["settingslib"])
        self.assertEqual(self.screens["permissioncontroller"]["libraries"], ["settingslib"])

    def test_fr34_settingslib_is_laid_over_the_tree_once_before_the_apps(self):
        order = apps.tree_order(apps=self.manifest)
        self.assertEqual(order[0], "settingslib")
        self.assertEqual(order.count("settingslib"), 1)
        self.assertEqual(set(order[1:]), {"documentsui", *self.screens})

    def test_fr34_the_record_names_settingslib_too(self):
        record = apps.provenance(self.screens["settings"], "android17")
        self.assertEqual(record["commit"], self.screens["settings"]["commits"]["android17"])
        self.assertEqual(record["libraries"]["settingslib"]["commit"], FRAMEWORKS_BASE["android17"])
        self.assertEqual(set(record["libraries"]["settingslib"]["patches"]),
                         {p.name for p in apps.patches("settingslib")})

    def test_fr34_patch_tree_checks_the_project_a_folder_is_in(self):
        with tempfile.TemporaryDirectory() as tmp:
            lock = Path(tmp) / "services-lock.json"
            lock.write_text(json.dumps({"versions": [{"projects": [
                {"path": "frameworks/base", "commit": "0" * 40}]}]}))
            with self.assertRaisesRegex(apps.AppsError, "frameworks/base at 0+"):
                apps.patch_tree("packageinstaller", tmp, "android17", "cp2a", lock)
            with self.assertRaisesRegex(apps.AppsError, "frameworks/base at 0+"):
                apps.patch_tree("settingslib", tmp, "android17", "cp2a", lock)

    def test_fr34_the_permission_controller_apk_is_found_among_the_trees_targets(self):
        targets = [
            "out/soong/.intermediates/packages/modules/Permission/PermissionController/PermissionController/"
            "android_common_apex36/PermissionController.apk: phony",
            "out/soong/.intermediates/packages/modules/Permission/PermissionController/PermissionController/"
            "android_common_apex36/aligned/PermissionController.apk: phony",
            "out/target/common/obj/foo.jar: phony",
        ]
        self.assertEqual(apps.apk_target("permissioncontroller", targets, self.manifest),
                         targets[0].split(": ")[0])
        with self.assertRaises(apps.AppsError):
            apps.apk_target("permissioncontroller", targets[2:], self.manifest)
        # The module's Android.bp names the platform too (apex_available), so a tree whose APEX
        # variant is named otherwise still has the platform variant, built from the same sources.
        base = "out/soong/.intermediates/packages/modules/Permission/PermissionController/PermissionController/"
        platform = [base + "android_common/PermissionController.apk: phony", targets[2]]
        self.assertEqual(apps.apk_target("permissioncontroller", platform, self.manifest),
                         base + "android_common/PermissionController.apk")
        named = [base + "android_common_com.android.permission/PermissionController.apk: phony", *platform]
        self.assertEqual(apps.apk_target("permissioncontroller", named, self.manifest),
                         base + "android_common_com.android.permission/PermissionController.apk")
        # The APEX variant comes first when the tree has both.
        self.assertEqual(apps.apk_target("permissioncontroller", [targets[0], *platform], self.manifest),
                         targets[0].split(": ")[0])

    def test_fr34_the_system_screens_build_on_a_runner_of_their_own(self):
        self.assertEqual(apps.tree_apps("services", self.manifest), ["documentsui"])
        self.assertEqual(set(apps.tree_apps("screens", self.manifest)), set(self.screens))
        self.assertEqual(apps.tree_order("screens", self.manifest)[0], "settingslib")
        self.assertNotIn("settingslib", apps.tree_order("services", self.manifest))
        workflow = (HERE.parents[1] / ".github/workflows/framework.yml").read_text()
        self.assertIn("part: [services, screens]", workflow)
        self.assertIn("DSA_TREE_PART: ${{ matrix.part }}", workflow)
        self.assertIn("default-app-system-screens-${{ matrix.android }}", workflow)
        script = (HERE.parents[1] / ".github/scripts/framework/build-services.sh").read_text()
        self.assertIn('[[ "$tree_part" == screens ]] || outputs=', script)
        for image in ("release-image.yml", "test-image.yml"):
            text = (HERE.parents[1] / ".github/workflows" / image).read_text()
            self.assertIn("download-documentsui.sh ${{ matrix.android }} .scratch/default-apps system-screens", text)

    def test_fr34_settings_lists_are_cards_with_room_between_groups(self):
        lib = "".join(p.read_text() for p in apps.patches("settingslib"))
        # A card surface that stands clear of the window, in light and dark.
        self.assertIn("+    <color name=\"settingslib_dsa_card\">@color/settingslib_materialColorSurfaceContainerLowest</color>", lib)
        self.assertIn("b/SettingsTheme/res/values-night-v36/settingslib_dsa_cards.xml", lib)
        self.assertIn("+    <dimen name=\"settingslib_preference_corner_radius\">26dp</dimen>", lib)
        self.assertIn("+        <item name=\"android:paddingTop\">@dimen/settingslib_dsa_group_gap</item>", lib)
        for card in ("", "_top", "_center", "_bottom"):
            self.assertIn(f"b/SettingsTheme/res/drawable-v36/settingslib_round_background{card}.xml", lib)
        # Emphasized group titles in the accent colour.
        self.assertIn("+        <item name=\"android:textColor\">@color/settingslib_materialColorPrimary</item>", lib)

    def test_fr34_pressed_rows_morph_their_shape_and_spring(self):
        lib = "".join(p.read_text() for p in apps.patches("settingslib"))
        self.assertIn("+            <item android:state_pressed=\"true\">", lib)
        self.assertIn("android:enterFadeDuration=\"@integer/settingslib_dsa_morph_ms\"", lib)
        self.assertIn("R.animator.settingslib_dsa_press_spring", lib)
        self.assertIn("b/SettingsTheme/res/interpolator-v36/settingslib_dsa_spring.xml", lib)
        self.assertIn("+<pathInterpolator", lib)

    def test_fr34_the_permission_dialog_buttons_are_full_width_rounded_tonal_buttons(self):
        text = "".join(p.read_text() for p in apps.patches("permissioncontroller"))
        self.assertEqual(text.count("+        <item name=\"android:background\">@drawable/dsa_grant_button</item>"), 3)
        self.assertIn("b/res/drawable-v31/dsa_grant_button.xml", text)
        self.assertIn("b/res/drawable-night-v31/dsa_grant_button.xml", text)
        self.assertIn("+            <corners android:radius=\"28dp\" />", text)
        self.assertIn("@android:color/system_accent2_100", text)
        self.assertIn("@android:color/system_accent2_700", text)


if __name__ == "__main__":
    unittest.main()
