"""Tests for the SurfaceFlinger patch that lets go of finished GPU work once RenderEngine is idle
(Android 16 0008, Android 17 0009).

A texture RenderEngine imported for a buffer lives until Skia sees that the GPU work which used
it finished, which it learns when it submits again. DSA's hardware composer composes every layer
on the host, so RenderEngine draws rarely, and the buffers of a layer, a closed app or a
screenshot stayed imported and held by SurfaceFlinger. The RenderEngine thread now waits a
second after each stretch of work and, if nothing came, checks finished work and submits once
more, waited on. Whether the patch applies to the pinned source, in order, is checked by
.github/scripts/framework/check-patches.sh in the framework build; these tests check what it
changes."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
NATIVE = HERE / "patches/frameworks/native"
COMMITS = {"android16": "fcbde2bcff56ca1d7bec9cabf8bdca5e288bf103",
           "android17": "ae266dcb706d083868578cfedce381ef44488a07"}
# For each Android: this patch, and the one before it.
NUMBERS = {"android16": ("0008", "0007"), "android17": ("0009", "0008")}
THREAD = "libs/renderengine/threaded/RenderEngineThreaded.cpp"
ENGINE = "libs/renderengine/skia/SkiaRenderEngine.cpp"
CONTEXTS = {
    "libs/renderengine/skia/compat/GaneshGpuContext.cpp": (
        "mGrContext->checkAsyncWorkCompletion();\n    mGrContext->flushAndSubmit(GrSyncCpu::kYes);"),
    "libs/renderengine/skia/compat/GraphiteGpuContext.cpp": (
        "mContext->checkAsyncWorkCompletion();\n    mContext->submit(skgpu::graphite::SyncToCpu::kYes);"),
}


def patch(android):
    found = list((NATIVE / android).glob(f"{NUMBERS[android][0]}-*.patch"))
    assert len(found) == 1, android
    return found[0].read_text(encoding="utf-8").replace("\r\n", "\n")


def lines(text, marker, path=None):
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


def function(joined, signature):
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


class IdleRenderEngineTests(unittest.TestCase):
    def test_nfr1_the_thread_waits_a_second_after_its_work_then_lets_go_of_finished_work(self):
        for android in COMMITS:
            text = patch(android)
            self.assertIn("static constexpr std::chrono::milliseconds kReleaseIdleGpuWorkAfter{1000};",
                          added(text), android)
            loop = added(text, THREAD)
            self.assertIn("if (!mCondition.wait_for(lock, kReleaseIdleGpuWorkAfter, hasWork)) {", loop, android)
            self.assertIn("mRenderEngine->releaseIdleGpuResources();", loop, android)

    def test_nfr1_an_idle_thread_releases_once_then_sleeps_until_new_work(self):
        for android in COMMITS:
            loop = added(patch(android), THREAD)
            # Set by work, cleared by the release; without it the thread waits as before.
            self.assertIn("            idleReleaseDue = true;", loop, android)
            self.assertIn("                idleReleaseDue = false;", loop, android)
            self.assertIn("if (idleReleaseDue && !hasWork()) {", loop, android)
            self.assertIn("        mCondition.wait(lock, hasWork);", loop, android)
            # The release runs without the queue's lock, so SurfaceFlinger never waits on it.
            self.assertLess(loop.index("lock.unlock();"), loop.index("mRenderEngine->releaseIdleGpuResources();"),
                            android)

    def test_nfr1_the_engine_checks_finished_work_and_submits_once_more_waited_on(self):
        for android in COMMITS:
            text = patch(android)
            for path, body in CONTEXTS.items():
                self.assertIn(body, added(text, path), (android, path))
            self.assertIn("virtual void releaseFinishedWork() = 0;", added(text), android)

    def test_nfr1_the_release_leaves_a_lost_device_alone_and_draws_nothing(self):
        for android in COMMITS:
            body = function(added(patch(android), ENGINE), "void SkiaRenderEngine::releaseIdleGpuResources() {")
            self.assertIn("std::lock_guard<std::mutex> lock(mRenderingMutex);", body, android)
            self.assertIn("if (mDeviceLossPending || context == nullptr || context->isAbandonedOrDeviceLost()) {",
                          body, android)
            self.assertLess(body.index("context->releaseFinishedWork();"),
                            body.rindex("mTextureCleanupMgr.cleanup();"), android)
            self.assertNotIn("drawLayers", body, android)

    def test_nfr1_the_release_logs_what_it_let_go_of(self):
        for android in COMMITS:
            body = function(added(patch(android), ENGINE), "void SkiaRenderEngine::releaseIdleGpuResources() {")
            self.assertIn('ALOGI("RenderEngine idle: finished GPU work let go of %d textures', body, android)
            self.assertIn("if (after < before) {", body, android)
            self.assertIn("static int liveCount() { return sLiveCount.load(); }", added(patch(android)), android)
            self.assertIn("~AutoBackendTexture() { sLiveCount--; }", added(patch(android)), android)
            self.assertIn("    sLiveCount++;", added(patch(android)), android)

    def test_nfr1_an_engine_without_the_release_does_nothing(self):
        for android in COMMITS:
            self.assertIn("virtual void releaseIdleGpuResources() {}",
                          added(patch(android), "libs/renderengine/include/renderengine/RenderEngine.h"), android)

    def test_nfr1_every_hunk_counts_its_lines(self):
        for android in COMMITS:
            for start, old_count, new_count, body in hunks(patch(android)):
                for line in body:
                    self.assertIn(line[:1], (" ", "+", "-"), (android, start, line))
                self.assertEqual(old_count, sum(1 for l in body if l[:1] in " -"), (android, start))
                self.assertEqual(new_count, sum(1 for l in body if l[:1] in " +"), (android, start))

    def test_nfr1_the_patch_comes_after_the_last_one_and_names_its_commit(self):
        for android, commit in COMMITS.items():
            number, before = NUMBERS[android]
            names = sorted(p.name[:4] for p in (NATIVE / android).glob("*.patch"))
            self.assertEqual(names, sorted(set(names)), android)
            self.assertEqual(names.index(number), names.index(before) + 1, android)
            self.assertIn(f"Applies to frameworks/native at {commit}", patch(android), android)
            self.assertIn(f"after {before}.", patch(android), android)

    def test_nfr1_no_em_dash(self):
        for android in COMMITS:
            self.assertNotIn(chr(0x2014), patch(android), android)


if __name__ == "__main__":
    unittest.main()
