"""Tests for the framework patch that rebuilds the views of an activity of a task marked for forced
system scaling at its new density, without a relaunch (0010, in framework.jar and services.jar).

0009 gives such a task a new density without relaunching its activity; 0010 makes the window
manager say so to the app's process with each configuration change, and the app's process then
converts the pixel sizes its views took at the old density (android.view.DsaDensityRescale).
Whether the patch applies to the pinned source, after 0009, is checked by
.github/scripts/framework/check-patches.sh in the framework build; these tests check what it
decides and that it changes nothing for any other task."""

import math
import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = HERE / "patches/frameworks/base"
VERSIONS = ("android16", "android17")
COMMITS = {"android16": "45034f0663f960d9ee5fb0a101a4732b71f6e2f4",
           "android17": "94b4c163b7dfe5ce3607f7bb8456f9573f7de57d"}
CORE = "core/java/android"
DRAWABLE = "graphics/java/android/graphics/drawable"
RESCALE = f"{CORE}/view/DsaDensityRescale.java"
ITEM = f"{CORE}/app/servertransaction/ActivityConfigurationChangeItem.java"
FILES = [
    f"{CORE}/app/ActivityThread.java",
    ITEM,
    RESCALE,
    f"{CORE}/view/View.java",
    f"{CORE}/view/ViewGroup.java",
    f"{CORE}/view/ViewRootImpl.java",
    f"{CORE}/widget/ImageView.java",
    f"{CORE}/widget/TextView.java",
    f"{DRAWABLE}/AnimatedVectorDrawable.java",
    f"{DRAWABLE}/BitmapDrawable.java",
    f"{DRAWABLE}/Drawable.java",
    f"{DRAWABLE}/DrawableContainer.java",
    f"{DRAWABLE}/DrawableWrapper.java",
    f"{DRAWABLE}/GradientDrawable.java",
    f"{DRAWABLE}/LayerDrawable.java",
    f"{DRAWABLE}/NinePatchDrawable.java",
    f"{DRAWABLE}/VectorDrawable.java",
    "services/core/java/com/android/server/wm/ActivityRecord.java",
]

# android.content.pm.ActivityInfo's values.
CONFIG_ORIENTATION = 0x0080
CONFIG_SCREEN_SIZE = 0x0400
CONFIG_DENSITY = 0x1000


def patch(android):
    found = list((BASE / android).glob("0010-*.patch"))
    assert len(found) == 1, android
    return found[0].read_text(encoding="utf-8").replace("\r\n", "\n")


def added(text, path=None):
    lines = []
    current = None
    for line in text.splitlines():
        if line.startswith("+++ "):
            current = line[len("+++ b/"):] if line.startswith("+++ b/") else None
        elif line.startswith("+") and path in (None, current):
            lines.append(line[1:])
    return "\n".join(lines) + "\n"


def method(source, signature):
    """The body of the method `signature` names in `source`, up to its closing brace at the
    indentation it was declared at."""
    at = source.find(signature)
    assert at >= 0, signature
    indent = source.rfind("\n", 0, at) + 1
    margin = source[indent:at]
    margin = margin[:len(margin) - len(margin.lstrip())]
    end = source.find("\n" + margin + "}\n", at)
    assert end >= 0, signature
    return source[at:end + len(margin) + 2]


def rescales_views(joined, forced, handled):
    """ActivityRecord.dsaRescalesViews(forced, handledChanges), run as the patch writes it."""
    body = method(joined, "static boolean dsaRescalesViews(boolean forced, int handledChanges) {")
    expected = ("static boolean dsaRescalesViews(boolean forced, int handledChanges) {\n"
                "        return forced && (handledChanges & ActivityInfo.CONFIG_DENSITY) == 0;\n"
                "    }")
    assert body == expected, body
    return forced and handled & CONFIG_DENSITY == 0


def scale(joined, px, from_dpi, to_dpi):
    """DsaDensityRescale.scale(int, int, int), run as the patch writes it."""
    body = method(joined, "public static int scale(int px, int from, int to) {")
    expected = ("public static int scale(int px, int from, int to) {\n"
                "        if (px == 0 || from == to || from <= 0 || to <= 0) {\n"
                "            return px;\n"
                "        }\n"
                "        final float scaled = px * (float) to / from;\n"
                "        final int rounded = Math.round(scaled);\n"
                "        // As Resources rounds a dimension: a size that was not zero stays at least one pixel.\n"
                "        if (rounded == 0) {\n"
                "            return px > 0 ? 1 : -1;\n"
                "        }\n"
                "        return rounded;\n"
                "    }")
    assert body == expected, body
    if px == 0 or from_dpi == to_dpi or from_dpi <= 0 or to_dpi <= 0:
        return px
    # Java's Math.round: the floor of x + 0.5.
    rounded = math.floor(px * to_dpi / from_dpi + 0.5)
    if rounded == 0:
        return 1 if px > 0 else -1
    return rounded


class ForcedDensityViewsPatchTests(unittest.TestCase):
    def test_fr4_the_views_patch_changes_framework_jar_and_the_window_manager(self):
        # The app's side (framework.jar: the activity thread, the views, the drawables) and the
        # window manager's (services.jar), in one patch, since services.jar calls the item's new
        # constructor and the image takes both jars from the same build.
        for android in VERSIONS:
            headers = [l for l in patch(android).splitlines() if l.startswith(("--- ", "+++ "))]
            expected = []
            for path in FILES:
                expected += ["--- /dev/null" if path == RESCALE else f"--- a/{path}", f"+++ b/{path}"]
            self.assertEqual(headers, expected, android)
            self.assertIn("new file mode 100644\n--- /dev/null\n+++ b/" + RESCALE, patch(android), android)

    def test_fr4_only_a_marked_task_that_does_not_handle_density_has_its_views_rescaled(self):
        for android in VERSIONS:
            joined = added(patch(android), FILES[-1])
            self.assertTrue(rescales_views(joined, True, 0), android)
            self.assertTrue(rescales_views(joined, True, CONFIG_ORIENTATION | CONFIG_SCREEN_SIZE), android)
            # An app that handles density redraws as it decides, as in App mode.
            self.assertFalse(rescales_views(joined, True, CONFIG_DENSITY), android)
            # Nothing changes for a task the user did not set to forced system scaling.
            self.assertFalse(rescales_views(joined, False, 0), android)
            self.assertIn("config, activityWindowInfo, getDisplayId(), dsaRescalesViews());", joined, android)
            self.assertIn("return dsaRescalesViews(dsaForcedDensity(), info.getRealConfigChanged());",
                          joined, android)

    def test_fr4_the_mark_travels_with_every_configuration_change_item(self):
        for android in VERSIONS:
            joined = added(patch(android), ITEM)
            # Every other caller keeps the old constructor, which never rescales.
            self.assertIn("this(activityToken, config, activityWindowInfo, displayId, false);", joined, android)
            # Written and read in the same place of the parcel.
            self.assertIn("dest.writeBoolean(mDsaRescaleViews);", joined, android)
            self.assertIn("mDsaRescaleViews = in.readBoolean();", joined, android)
            text = patch(android)
            self.assertLess(text.index("dest.writeInt(mDisplayId);\n+        dest.writeBoolean(mDsaRescaleViews);"),
                            text.index("mDisplayId = in.readInt();\n+        mDsaRescaleViews = in.readBoolean();"))
            # Set on the record before the client handles the item, for whichever item it keeps.
            self.assertIn("r.mDsaRescaleViews = mDsaRescaleViews;", joined, android)
            self.assertIn("&& mDsaRescaleViews == other.mDsaRescaleViews;", joined, android)

    def test_fr4_the_views_are_rescaled_after_the_resources_and_before_the_activity_hears(self):
        for android in VERSIONS:
            text = patch(android)
            hunk = text[text.index("+++ b/core/java/android/app/ActivityThread.java"):text.index("diff --git a/" + ITEM)]
            condition = ("+        if (r.mDsaRescaleViews && (diff & ActivityInfo.CONFIG_DENSITY) != 0\n"
                         "+                && (handledConfigChanges & ActivityInfo.CONFIG_DENSITY) == 0) {\n"
                         "+            android.view.DsaDensityRescale.rescaleWindowsOf(activityToken);\n"
                         "+        }\n")
            self.assertIn(condition, hunk, android)
            # Right after the activity's resources take the new density, so the views read it,
            # and before onConfigurationChanged, so what the app sets there is not scaled twice.
            after = " mResourcesManager.updateResourcesForActivity(activityToken, finalOverrideConfig, displayId);\n"
            self.assertLess(hunk.index(after), hunk.index(condition), android)
            self.assertIn("public boolean mDsaRescaleViews;", added(hunk), android)

    def test_fr4_nothing_changes_in_a_process_before_its_first_forced_change(self):
        for android in VERSIONS:
            joined = added(patch(android), RESCALE)
            # Set in one place only: when the window manager's change reaches the activity.
            self.assertEqual(joined.count("sActive = true;"), 1, android)
            self.assertIn("public static void rescaleWindowsOf(IBinder token) {\n        sActive = true;", joined)
            self.assertIn("if (sActive && view != null) {", joined, android)
            view = added(patch(android), f"{CORE}/view/View.java")
            self.assertIn("    public final void dsaRescaleIfActive() {\n"
                          "        if (DsaDensityRescale.sActive) {\n"
                          "            dsaRescaleIfStale();", view, android)
            # Every hook goes through the check, in View and its subclasses: only the check
            # itself and the conversion of a window's tree call dsaRescaleIfStale.
            self.assertEqual(view.count("dsaRescaleIfStale();"), 1, android)
            for path in (f"{CORE}/widget/TextView.java", f"{CORE}/widget/ImageView.java"):
                self.assertNotIn("dsaRescaleIfStale();", added(patch(android), path), (android, path))
            self.assertIn("        view.dsaRescaleIfStale();", joined, android)
            for path in (f"{CORE}/view/ViewGroup.java", f"{CORE}/view/ViewRootImpl.java"):
                calls = re.findall(r"DsaDensityRescale\.(\w+)\(", added(patch(android), path))
                self.assertTrue(calls, (android, path))
                self.assertTrue(set(calls) <= {"rescaleTreeIfActive", "scale", "densityOf"},
                                (android, path, calls))

    def test_fr4_a_view_records_its_density_and_is_converted_once(self):
        for android in VERSIONS:
            view = added(patch(android), f"{CORE}/view/View.java")
            self.assertIn("mDsaDensityDpi = mResources != null ? mResources.getDisplayMetrics().densityDpi : 0;",
                          view, android)
            body = method(view, "public final void dsaRescaleIfStale() {")
            # The new density is recorded before anything is converted, so what the conversion
            # calls back into finds the view converted already.
            self.assertLess(body.index("mDsaDensityDpi = to;"), body.index("onDsaRescale(from, to);"), android)
            self.assertIn("if (to == from || to == 0) {", body, android)
            group = added(patch(android), f"{CORE}/view/ViewGroup.java")
            rescale = method(group, "public final void dsaRescale(int fromDpi, int toDpi) {")
            self.assertIn("final int from = mDsaDensityDpi != 0 ? mDsaDensityDpi : fromDpi;", rescale, android)
            self.assertLess(rescale.index("mDsaDensityDpi = toDpi;"), rescale.index("onDsaRescale(from, toDpi);"))

    def test_fr4_the_conversion_rounds_as_resources_round_a_dimension(self):
        for android in VERSIONS:
            joined = added(patch(android), RESCALE)
            # An app bar of 48 dp, from a 150% monitor (196 dpi) to a 200% one (262 dpi).
            self.assertEqual(scale(joined, 59, 196, 262), 79, android)
            self.assertEqual(scale(joined, 79, 262, 196), 59, android)
            self.assertEqual(scale(joined, 0, 196, 262), 0, android)
            # A hairline stays a pixel; a negative margin stays negative.
            self.assertEqual(scale(joined, 1, 262, 130), 1, android)
            self.assertEqual(scale(joined, -1, 262, 130), -1, android)
            self.assertEqual(scale(joined, -24, 196, 262), -32, android)
            # Nothing is known of a density of 0, and nothing changes at the same density.
            self.assertEqual(scale(joined, 59, 0, 262), 59, android)
            self.assertEqual(scale(joined, 59, 196, 196), 59, android)

    def test_fr4_the_window_manager_keeps_the_size_of_a_window(self):
        # A window's own parameters are the window manager's, which keeps the window's size on a
        # move (the host sizes it); only what is inside it is converted.
        for android in VERSIONS:
            view = added(patch(android), f"{CORE}/view/View.java")
            self.assertIn("!(mLayoutParams instanceof WindowManager.LayoutParams)", view, android)
            rescale = added(patch(android), RESCALE)
            self.assertIn("if (root.mThread != Thread.currentThread()) {", rescale, android)

    def test_fr4_layout_parameters_padding_text_and_images_are_converted(self):
        for android in VERSIONS:
            group = added(patch(android), f"{CORE}/view/ViewGroup.java")
            for field in ("width", "height"):
                self.assertIn(f"            if ({field} > 0) {{\n                {field} = DsaDensityRescale.scale({field}, fromDpi, toDpi);",
                              group, (android, field))
            for field in ("leftMargin", "topMargin", "rightMargin", "bottomMargin"):
                self.assertIn(f"{field} = DsaDensityRescale.scale({field}, fromDpi, toDpi);", group, (android, field))
            self.assertIn("if (startMargin != DEFAULT_MARGIN_RELATIVE) {", group, android)
            view = added(patch(android), f"{CORE}/view/View.java")
            for wanted in ("internalSetPadding(DsaDensityRescale.scale(mUserPaddingLeft, fromDpi, toDpi),",
                           "mMinWidth = DsaDensityRescale.scale(mMinWidth, fromDpi, toDpi);",
                           "mMinHeight = DsaDensityRescale.scale(mMinHeight, fromDpi, toDpi);",
                           "mBackground.dsaRetargetDensity(fromDpi, toDpi);",
                           "mForegroundInfo.mDrawable.dsaRetargetDensity(fromDpi, toDpi);"):
                self.assertIn(wanted, view, (android, wanted))
            text = added(patch(android), f"{CORE}/widget/TextView.java")
            for wanted in ("setRawTextSize(DsaDensityRescale.scale(mTextPaint.getTextSize(), fromDpi, toDpi),",
                           "mSpacingAdd = DsaDensityRescale.scale(mSpacingAdd, fromDpi, toDpi);",
                           "dr.mDrawablePadding = DsaDensityRescale.scale(dr.mDrawablePadding, fromDpi, toDpi);",
                           "mAutoSizeTextSizesInPx = cleanupAutoSizePresetSizes(sizes);"):
                self.assertIn(wanted, text, (android, wanted))
            # The spacing first, so that a line height in sp, worked out again from the new text
            # size, is not scaled on top.
            self.assertLess(text.index("mSpacingAdd = DsaDensityRescale.scale"), text.index("setRawTextSize("))
            image = added(patch(android), f"{CORE}/widget/ImageView.java")
            self.assertIn("mDrawable.dsaRetargetDensity(fromDpi, toDpi);", image, android)
            self.assertIn("mDrawableWidth = mDrawable.getIntrinsicWidth();", image, android)
            # Each subclass converts its own sizes on top of the view's.
            for path in (f"{CORE}/widget/TextView.java", f"{CORE}/widget/ImageView.java"):
                body = method(added(patch(android), path), "protected void onDsaRescale(int fromDpi, int toDpi) {")
                self.assertIn("super.onDsaRescale(fromDpi, toDpi);", body.splitlines()[1], (android, path))

    def test_fr4_a_size_the_app_sets_at_the_new_density_is_not_converted_again(self):
        # A view kept aside (a list item for reuse) is converted when the app first touches one of
        # its sizes through the framework, before the app's own value goes in.
        # Each hook as the patch adds it: the check first, then the method's own first line.
        hooks = [
            ("     public ViewGroup.LayoutParams getLayoutParams() {\n"
             "+        // DSA: converted before the app changes them at the new density (DsaDensityRescale).\n"
             "+        dsaRescaleIfActive();\n"
             "         return mLayoutParams;\n"),
            ("+        dsaRescaleIfActive();\n"
             "+        dsaAdoptLayoutParams(params);\n"
             "         mLayoutParams = params;\n"),
            ("     public void setPadding(int left, int top, int right, int bottom) {\n"
             "+        dsaRescaleIfActive();\n"),
            ("     public void setPaddingRelative(int start, int top, int end, int bottom) {\n"
             "+        dsaRescaleIfActive();\n"),
            ("     public void setMinimumHeight(int minHeight) {\n"
             "+        dsaRescaleIfActive();\n"),
            ("     public void setMinimumWidth(int minWidth) {\n"
             "+        dsaRescaleIfActive();\n"),
            ("     public void setTextSize(int unit, float size) {\n"
             "+        // DSA: converted first, so that a size the app sets at the new density stays as set.\n"
             "+        dsaRescaleIfActive();\n"),
            ("     private void updateDrawable(Drawable d) {\n"
             "+        // DSA: converted first, so that the drawable set now keeps its own density.\n"
             "+        dsaRescaleIfActive();\n"),
        ]
        for android in VERSIONS:
            text = patch(android)
            for hook in hooks:
                self.assertIn(hook, text, (android, hook))
            group = added(text, f"{CORE}/view/ViewGroup.java")
            # Parameters made for a view without a density take the view's.
            self.assertEqual(group.count("child.dsaAdoptLayoutParams(params);"), 2, android)
            # A view joins a parent or a window at the new density.
            self.assertEqual(group.count("DsaDensityRescale.rescaleTreeIfActive(child);"), 2, android)
            self.assertIn("DsaDensityRescale.rescaleTreeIfActive(view);", added(text, f"{CORE}/view/ViewRootImpl.java"))

    def test_fr4_a_drawable_made_at_another_density_is_left_as_its_maker_set_it(self):
        guards = {
            "BitmapDrawable.java": "if (mTargetDensity == fromDpi) {",
            "NinePatchDrawable.java": "if (mTargetDensity == fromDpi) {",
            "VectorDrawable.java": "if (mTargetDensity == fromDpi && toDpi > 0) {",
            "GradientDrawable.java": "if (state.mDensity == fromDpi) {",
            "LayerDrawable.java": "final boolean own = state.mDensity == fromDpi;",
            "DrawableWrapper.java": "if (mState != null && mState.mDensity == fromDpi) {",
            "DrawableContainer.java": "if (state.mDensity == fromDpi) {",
        }
        for android in VERSIONS:
            base = added(patch(android), f"{DRAWABLE}/Drawable.java")
            self.assertIn("    public void dsaRetargetDensity(int fromDpi, int toDpi) {\n    }", base, android)
            for name, guard in guards.items():
                body = method(added(patch(android), f"{DRAWABLE}/{name}"),
                              "public void dsaRetargetDensity(int fromDpi, int toDpi) {")
                self.assertIn(guard, body, (android, name))
            # In place, so a running animation keeps running and the app's reference stays good.
            for name in guards:
                self.assertNotIn("newDrawable", added(patch(android), f"{DRAWABLE}/{name}"), (android, name))

    def test_fr4_every_hunk_line_starts_with_a_diff_marker(self):
        for android in VERSIONS:
            in_hunk = False
            for line in patch(android).splitlines():
                if line.startswith("@@"):
                    in_hunk = True
                elif line.startswith("diff --git"):
                    in_hunk = False
                elif in_hunk:
                    self.assertIn(line[:1], (" ", "+", "-"), (android, line))

    def test_fr4_the_patch_names_the_commit_it_was_made_for(self):
        for android, commit in COMMITS.items():
            self.assertIn(f"Applies to frameworks/base at {commit}", patch(android), android)
            # After 0009, whose dsaForcedDensity it calls.
            self.assertIn("after 0009", patch(android), android)

    def test_fr4_both_versions_add_the_same_rescaling_class(self):
        self.assertEqual(added(patch("android16"), RESCALE), added(patch("android17"), RESCALE))


if __name__ == "__main__":
    unittest.main()
