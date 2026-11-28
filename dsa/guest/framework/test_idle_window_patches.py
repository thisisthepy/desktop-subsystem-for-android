"""Tests for the HWUI patches that let go of what a window keeps while it draws nothing (0014 and
0015, in libhwui).

DSA shows each task in its own desktop window, so a window the user can see but does not touch
stays on screen for a long time, and Android trims an app's GPU memory only once its UI is
hidden. After 30 seconds without a frame, the render thread frees Skia's GPU resources that
nothing holds and submits once more, waited on, so ANGLE frees what it deferred (0014). Each
window then also lets go of its GPU surface and keeps its native window, which disconnects its
buffer queue and frees every buffer in it but the one on screen; the next frame makes the
surface again (0015). Whether the patches apply to the pinned source, in order, is checked by
.github/scripts/framework/check-patches.sh in the framework build; these tests check what they
change."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = HERE / "patches/frameworks/base"
VERSIONS = ("android16", "android17")
COMMITS = {"android16": "45034f0663f960d9ee5fb0a101a4732b71f6e2f4",
           "android17": "94b4c163b7dfe5ce3607f7bb8456f9573f7de57d"}
CACHES = "0014"
SURFACES = "0015"
CACHE_MANAGER = "libs/hwui/renderthread/CacheManager.cpp"
CANVAS_CONTEXT = "libs/hwui/renderthread/CanvasContext.cpp"


def patch(android, number):
    found = list((BASE / android).glob(f"{number}-*.patch"))
    assert len(found) == 1, (android, number)
    return found[0].read_text(encoding="utf-8").replace("\r\n", "\n")


def lines(text, marker, path=None):
    """The lines of `path` (or every file) that start with `marker` ("+" added, "-" removed)."""
    found = []
    current = None
    for line in text.splitlines():
        if line.startswith("+++ b/"):
            current = line[len("+++ b/"):]
        elif line.startswith("--- a/"):
            continue
        elif line.startswith(marker) and path in (None, current):
            found.append(line[1:])
    return found


def added(text, path=None):
    return "\n".join(lines(text, "+", path))


def paths(text):
    return [l[len("+++ b/"):] for l in text.splitlines() if l.startswith("+++ b/")]


def function(joined, signature):
    """The body of the function that starts with `signature` among the added lines."""
    start = joined.index(signature)
    depth = 0
    for i in range(start, len(joined)):
        if joined[i] == "{":
            depth += 1
        elif joined[i] == "}":
            depth -= 1
            if depth == 0:
                return joined[start:i + 1]
    raise AssertionError(f"{signature} does not end")


def hunks(text):
    """Yields (old start, old count, new count, lines) for every hunk."""
    current = None
    for line in text.splitlines():
        if line.startswith("diff --git"):
            if current:
                yield current
            current = None
        elif line.startswith("@@"):
            if current:
                yield current
            old, new = line.split()[1:3]
            old_start, old_count = (int(x) for x in old[1:].split(","))
            _, new_count = (int(x) for x in new[1:].split(","))
            current = (old_start, old_count, new_count, [])
        elif current is not None:
            current[3].append(line)
    if current:
        yield current


class IdleWindowCacheTests(unittest.TestCase):
    def test_nfr1_an_idle_render_thread_frees_skias_gpu_resources_after_30_seconds(self):
        for android in VERSIONS:
            text = patch(android, CACHES)
            self.assertIn("static constexpr nsecs_t kIdleReleaseAfter = 30_s;", added(text), android)
            body = function(added(text, CACHE_MANAGER), "void CacheManager::releaseWhileIdle(nsecs_t idleFor) {")
            self.assertIn("mGrContext->freeGpuResources();", body, android)

    def test_nfr1_the_release_submits_once_more_and_waits_so_angle_frees_what_it_deferred(self):
        for android in VERSIONS:
            body = function(added(patch(android, CACHES), CACHE_MANAGER),
                            "void CacheManager::releaseWhileIdle(nsecs_t idleFor) {")
            self.assertLess(body.index("mGrContext->freeGpuResources();"),
                            body.index("mGrContext->flushAndSubmit(GrSyncCpu::kYes);"), android)

    def test_nfr1_every_frame_postpones_the_release_and_one_check_waits_at_a_time(self):
        for android in VERSIONS:
            text = added(patch(android, CACHES), CACHE_MANAGER)
            self.assertIn("    mIdleReleased = false;\n    scheduleIdleRelease(kIdleReleaseAfter);", text, android)
            schedule = function(text, "void CacheManager::scheduleIdleRelease(nsecs_t delay) {")
            self.assertIn("if (mIdleReleasePending) return;", schedule, android)
            due = function(text, "void CacheManager::onIdleReleaseDue() {")
            self.assertIn("scheduleIdleRelease(kIdleReleaseAfter - idleFor);", due, android)

    def test_nfr1_the_release_runs_once_a_quiet_stretch_and_not_on_a_lost_device(self):
        for android in VERSIONS:
            due = function(added(patch(android, CACHES), CACHE_MANAGER), "void CacheManager::onIdleReleaseDue() {")
            self.assertIn("if (!mGrContext || mGrContext->abandoned() || mIdleReleased) return;", due, android)
            self.assertLess(due.index("mIdleReleased = true;"), due.index("releaseWhileIdle(idleFor);"), android)

    def test_nfr1_the_release_logs_what_it_let_go_of(self):
        for android in VERSIONS:
            self.assertIn('ALOGI("No frame for %" PRId64 " s: let go of %zu KiB of GPU caches',
                          added(patch(android, CACHES)), android)

    def test_nfr1_only_the_cache_manager_changes_and_nothing_is_removed(self):
        for android in VERSIONS:
            text = patch(android, CACHES)
            self.assertEqual(paths(text), [CACHE_MANAGER, "libs/hwui/renderthread/CacheManager.h"], android)
            self.assertEqual(lines(text, "-"), [], android)


class IdleWindowSurfaceTests(unittest.TestCase):
    def test_nfr1_each_idle_window_lets_go_of_its_surface_before_the_caches_go(self):
        for android in VERSIONS:
            text = added(patch(android, SURFACES), CACHE_MANAGER)
            self.assertIn("if (context->releaseSurfaceWhileIdle()) windows++;", text, android)
            self.assertIn("let go of the surfaces of %zu windows", text, android)

    def test_nfr1_both_pipelines_destroy_the_surface_and_keep_the_window(self):
        for android in VERSIONS:
            gl = added(patch(android, SURFACES), "libs/hwui/pipeline/skia/SkiaOpenGLPipeline.cpp")
            self.assertIn("if (mHardwareBuffer || !mNativeWindow || mEglSurface == EGL_NO_SURFACE) {", gl, android)
            self.assertIn("mEglManager.destroySurface(mEglSurface);\n    mEglSurface = EGL_NO_SURFACE;", gl, android)
            self.assertNotIn("mNativeWindow = nullptr", gl, android)
            vk = added(patch(android, SURFACES), "libs/hwui/pipeline/skia/SkiaVulkanPipeline.cpp")
            self.assertIn("if (mHardwareBuffer || !mNativeWindow || !mVkSurface) {", vk, android)
            self.assertIn("vulkanManager().destroySurface(mVkSurface);\n    mVkSurface = nullptr;", vk, android)

    def test_nfr1_a_pipeline_without_such_a_surface_lets_go_of_nothing(self):
        for android in VERSIONS:
            self.assertIn("virtual bool releaseSurfaceWhileIdle() { return false; }",
                          added(patch(android, SURFACES), "libs/hwui/renderthread/IRenderPipeline.h"), android)

    def test_nfr1_the_next_frame_makes_the_surface_again_and_draws_all_of_the_window(self):
        for android in VERSIONS:
            text = added(patch(android, SURFACES), CANVAS_CONTEXT)
            # A frame the UI thread syncs calls makeCurrent() first; one the render thread draws
            # on its own (doFrame, prepareAndDraw) does it here.
            self.assertEqual(text.count("if (mSurfaceReleasedWhileIdle && !makeCurrent()) return;"), 2, android)
            release = function(text, "bool CanvasContext::releaseSurfaceWhileIdle() {")
            self.assertIn("mSurfaceReleasedWhileIdle = true;", release, android)
            self.assertIn("mHaveNewSurface = true;", release, android)
            self.assertIn("const bool remakesIdleSurface = std::exchange(mSurfaceReleasedWhileIdle, false);",
                          text, android)

    def test_nfr1_what_the_disconnect_forgot_is_set_on_the_window_again(self):
        for android in VERSIONS:
            text = added(patch(android, SURFACES), CANVAS_CONTEXT)
            self.assertEqual(text.count("setWindowOptions(mNativeSurface->getNativeWindow());"), 2, android)
            options = function(text, "void CanvasContext::setWindowOptions(ANativeWindow* window) {")
            self.assertIn("native_window_enable_frame_timestamps(window, true);", options, android)
            self.assertIn("native_window_set_scaling_mode(window, NATIVE_WINDOW_SCALING_MODE_FREEZE);",
                          options, android)
        # Android 17's setupPipelineSurface also turns producer throttling off; Android 16's does not.
        self.assertIn("native_window_set_producer_throttling_enabled(window, false);",
                      added(patch("android17", SURFACES), CANVAS_CONTEXT))
        self.assertNotIn("producer_throttling", patch("android16", SURFACES))


class IdleWindowPatchFormTests(unittest.TestCase):
    def test_nfr1_every_hunk_counts_its_lines(self):
        for android in VERSIONS:
            for number in (CACHES, SURFACES):
                for start, old_count, new_count, body in hunks(patch(android, number)):
                    for line in body:
                        self.assertIn(line[:1], (" ", "+", "-"), (android, number, start, line))
                    self.assertEqual(old_count, sum(1 for l in body if l[:1] in " -"), (android, number, start))
                    self.assertEqual(new_count, sum(1 for l in body if l[:1] in " +"), (android, number, start))

    def test_nfr1_the_patches_come_in_order_after_the_last_one(self):
        for android, commit in COMMITS.items():
            names = sorted(p.name[:4] for p in (BASE / android).glob("*.patch"))
            self.assertEqual(names, sorted(set(names)), android)
            self.assertIn(f"Applies to frameworks/base at {commit}", patch(android, CACHES), android)
            self.assertIn("after 0013.", patch(android, CACHES), android)
            self.assertIn(f"Applies to frameworks/base at {commit}", patch(android, SURFACES), android)
            self.assertIn("after 0014.", patch(android, SURFACES), android)

    def test_nfr1_only_libhwui_changes(self):
        for android in VERSIONS:
            for number in (CACHES, SURFACES):
                for path in paths(patch(android, number)):
                    self.assertTrue(path.startswith("libs/hwui/"), (android, number, path))

    def test_nfr1_no_em_dash(self):
        for android in VERSIONS:
            for number in (CACHES, SURFACES):
                self.assertNotIn(chr(0x2014), patch(android, number), (android, number))


if __name__ == "__main__":
    unittest.main()
