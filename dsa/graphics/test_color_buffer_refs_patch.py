"""Tests for the guest Vulkan driver keeping a gralloc buffer's host ColorBuffer while the process
has the buffer.

Without virtio-gpu the host frees a buffer's shared ColorBuffer as soon as no guest process has it
open, and a buffer only the GPU writes holds its pixels nowhere else. The driver closed its
process's reference when the last Vulkan object on the buffer went. hwui uploads a hardware bitmap
through an EGLImage it destroys right after the copy, and the render thread imports the buffer
again when it first draws the bitmap, so a bitmap decoded ahead of drawing came back as a new,
empty ColorBuffer: every thumbnail, ad and avatar in YouTube was an empty grey box.

Now the reference also lasts while the process still has one of the buffer's files open, asked of
every descriptor the process has (include/dsa_color_buffer_refs.h, tested by
tests/color_buffer_refs_test.cpp), and the ones the process let go of are closed at a submit or as
one of its Vulkan objects on a buffer goes, at most once a second.

SurfaceFlinger aborted on the owner's PC ("Failed to create a valid texture" while it imported an
app's new buffer after a resize, and a SIGSEGV in vkCmdPipelineBarrier under ANGLE), which
restarted the framework and ended every app. A reference could be closed while the process still
used the buffer: only the descriptor numbers seen at an import were asked, a holder was counted
after the lock that found the reference had been let go, and two threads could both open one
buffer and one close it again. These tests keep each of those shut."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH_DIR = HERE / "patches/guest/mesa"
PATCH = next(PATCH_DIR.glob("0013-*.patch"))
HEADER = HERE / "include/dsa_color_buffer_refs.h"
GRALLOC = "src/gfxstream/guest/android/GrallocMinigbm.cpp"
GRALLOC_HEADER = "src/gfxstream/guest/android/GrallocMinigbm.h"
INTERFACE = "src/gfxstream/guest/android/include/gfxstream/guest/GfxStreamGralloc.h"
TRACKER = "src/gfxstream/guest/vulkan_enc/ResourceTracker.cpp"


def file_part(patch, name):
    """The part of a patch that changes one file."""
    text = patch.read_text(encoding="utf-8").replace("\r\n", "\n")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            return part
    raise AssertionError(f"{patch.name} does not change {name}")


def file_hunks(patch, name):
    """The added and removed lines of one file in a patch."""
    lines = file_part(patch, name).splitlines()
    return ([l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")],
            [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")])


def after(patch, name):
    """The lines of one file's hunks as the patch leaves them: context and added lines."""
    lines = file_part(patch, name).splitlines()
    kept = [l[1:] for l in lines
            if (l.startswith(" ") or l.startswith("+")) and not l.startswith("+++")]
    return "\n".join(kept) + "\n"


def header():
    """The references' header, with the line ends a checkout on any system gives."""
    return HEADER.read_text(encoding="utf-8").replace("\r\n", "\n")


def function(text, signature, indent=""):
    """The body of the function that starts with `signature`, up to its closing brace at
    `indent`."""
    start = text.index(signature)
    end = text.index(f"\n{indent}}}\n", start)
    return text[start:end + len(indent) + 2]


class ColorBufferRefsPatchTests(unittest.TestCase):
    def test_pr3_the_last_vulkan_object_on_a_buffer_does_not_close_its_color_buffer(self):
        added, removed = file_hunks(PATCH, GRALLOC)
        text = "\n".join(added)
        # The old rule: the last holder erased the reference and closed it.
        self.assertIn("    mBufferIdToColorBuffer.erase(it);", removed)
        # Now the references decide, with what the process still has open.
        self.assertIn("mColorBufferRefs.drop(bufferId);", text)
        self.assertIn("        mColorBufferRefs.opened(bufferId, hostHandle, handle->data, fdCount);", text)
        self.assertIn("        mColorBufferRefs.hold(bufferId, hostHandle, handle->data, fdCount);", text)
        drop = function(header(), "    Closed drop(uint32_t bufferId) {", "    ")
        self.assertIn("if (it->second.holders > 0) return {};", drop)
        self.assertIn("if (!list_(&open) || stillOpen(it->second, open)) return {};", drop)

    def test_pr3_whether_the_process_has_a_buffer_is_asked_of_every_descriptor(self):
        # The numbers seen at an import are only a quick yes; a no is asked of every
        # descriptor the process has, so a buffer held through another handle stays.
        text = header()
        self.assertIn('DIR* dir = opendir("/proc/self/fd");', text)
        self.assertIn("ColorBufferRefs(IdentifyFile identify = identifyOpenFile,", text)
        self.assertIn("ListFiles list = listOpenFiles)", text)
        sweep = function(text, "    std::vector<Closed> sweep() {", "    ")
        self.assertIn("!stillOpenByNumber(ref)", sweep)
        self.assertIn("if (!list_(&open)) return closed;", sweep)
        self.assertIn("stillOpen(it->second, open)", sweep)
        # A buffer with no file known closes only with its last holder, never at a sweep.
        self.assertIn("ref.holders == 0 && !ref.files.empty()", sweep)
        # Files are kept by what they are; a newer number for the same file replaces the old.
        self.assertIn("if (old.id == file.id) {", text)

    def test_pr3_a_held_handle_is_found_or_opened_and_held_under_one_lock(self):
        text = after(PATCH, GRALLOC)
        hold = function(text, "uint32_t MinigbmGralloc::holdHostHandle(const native_handle_t* handle) {")
        self.assertIn("return sharedHostHandle(handle, /*hold=*/true);", hold)
        self.assertNotIn("mColorBufferMutex", hold)
        # getHostHandle's own start is outside the patch's hunks: its last line is in them.
        added, _ = file_hunks(PATCH, GRALLOC)
        self.assertIn("    return sharedHostHandle(handle, /*hold=*/false);", added)
        shared = function(text, "uint32_t MinigbmGralloc::sharedHostHandle(const native_handle_t* handle, bool hold) {")
        # One lock, taken before the lookup and kept to the hold.
        self.assertEqual(shared.count("std::lock_guard<std::mutex> lock(mColorBufferMutex);"), 1)
        lock = shared.index("std::lock_guard<std::mutex> lock(mColorBufferMutex);")
        self.assertLess(lock, shared.index("mColorBufferRefs.find(bufferId);"))
        self.assertLess(lock, shared.index("rcEnc->rcOpenColorBuffer2(rcEnc, hostHandle)"))
        self.assertLess(shared.index("mColorBufferRefs.opened("), shared.index("mColorBufferRefs.hold("))
        # No second open to close again: only one thread opens a buffer at a time.
        self.assertNotIn("rcEnc->rcCloseColorBuffer", shared)

    def test_pr3_a_close_is_sent_before_the_lock_lets_another_thread_open_the_buffer(self):
        text = after(PATCH, GRALLOC)
        for signature, scope in (
            ("void MinigbmGralloc::release(AHardwareBuffer* ahb) {", "\n            }\n"),
            ("void MinigbmGralloc::dropHostHandle(uint32_t hostHandle) {", "\n    }\n"),
            ("void MinigbmGralloc::closeReleasedBuffers() {", "\n    }\n"),
        ):
            body = function(text, signature)
            lock = body.index("std::lock_guard<std::mutex> lock(mColorBufferMutex);")
            close = body.index("closeLocked(closed);")
            forget = body.index("forgetUploads(closed);")
            # The close inside the lock's scope, what was uploaded forgotten after it.
            scope_end = body.index(scope, lock)
            self.assertLess(lock, close, signature)
            self.assertLess(close, scope_end, signature)
            self.assertLess(scope_end, forget, signature)
        close = function(text, "void MinigbmGralloc::closeLocked(const std::vector<dsa::ColorBufferRefs::Closed>& closed) {")
        self.assertIn("rcEnc->rcCloseColorBuffer(rcEnc, one.hostHandle);", close)
        self.assertNotIn("mUploadMutex", close)
        self.assertNotIn("closeHostHandle(", text)

    def test_nfr1_a_released_memory_lets_go_of_its_buffer_before_the_reference_is_dropped(self):
        added, removed = file_hunks(PATCH, GRALLOC)
        # Released first, so a buffer the memory held last closes at once, not a sweep later.
        release = added.index("        AHardwareBuffer_release(ahb);")
        drop = added.index(
            "                const dsa::ColorBufferRefs::Closed dropped = mColorBufferRefs.drop(bufferId);")
        self.assertLess(release, drop)
        self.assertIn("        AHardwareBuffer_release(ahb);", removed)

    def test_nfr1_the_references_a_process_let_go_of_close_at_a_submit(self):
        added, _ = file_hunks(PATCH, TRACKER)
        self.assertIn("    if (mGralloc) mGralloc->closeReleasedBuffers();", added)
        text = after(PATCH, GRALLOC)
        sweep = function(text, "void MinigbmGralloc::sweepLocked(std::vector<dsa::ColorBufferRefs::Closed>* closed) {")
        self.assertIn("if (!mColorBufferRefs.sweepDue(nowNs)) {", sweep)
        self.assertIn("for (const dsa::ColorBufferRefs::Closed& one : mColorBufferRefs.sweep()) {", sweep)
        released = function(text, "void MinigbmGralloc::closeReleasedBuffers() {")
        self.assertIn("sweepLocked(&closed);", released)
        self.assertIn("static constexpr uint64_t kSweepIntervalNs = 1000000000ULL;", header())

    def test_nfr2_a_process_that_stops_submitting_still_closes_what_it_let_go_of(self):
        # A window in the background submits nothing: the sweep also runs as its Vulkan objects
        # go (the memory that imported a buffer, a window image), after the reference that
        # object held is dropped, and its closes go with the drop's.
        text = after(PATCH, GRALLOC)
        for signature in (
            "void MinigbmGralloc::release(AHardwareBuffer* ahb) {",
            "void MinigbmGralloc::dropHostHandle(uint32_t hostHandle) {",
        ):
            body = function(text, signature)
            drop = body.index("mColorBufferRefs.drop(bufferId);")
            sweep = body.index("sweepLocked(&closed);")
            close = body.index("closeLocked(closed);")
            self.assertLess(drop, sweep, signature)
            self.assertLess(sweep, close, signature)

    def test_nfr1_every_gralloc_offers_the_sweep_and_only_minigbm_does_it(self):
        added, _ = file_hunks(PATCH, INTERFACE)
        self.assertIn("    virtual void closeReleasedBuffers() {}", added)
        added, _ = file_hunks(PATCH, GRALLOC_HEADER)
        self.assertIn("    void closeReleasedBuffers() override;", added)
        self.assertIn('#include "dsa_color_buffer_refs.h"', added)
        self.assertIn("    dsa::ColorBufferRefs mColorBufferRefs;", added)
        self.assertIn("    uint32_t sharedHostHandle(const native_handle_t* handle, bool hold);", added)

    def test_nfr1_a_buffer_is_kept_by_the_file_its_descriptor_names_not_by_its_number(self):
        text = header()
        self.assertIn("if (fstat(fd, &st) != 0) return false;", text)
        self.assertIn("if (identify_(file.fd, &now.device, &now.inode) && now == file.id) return true;",
                      text)
        self.assertIn("return a.device == b.device && a.inode == b.inode;", text)

    def test_nfr1_the_patch_comes_after_the_ones_it_builds_on(self):
        names = sorted(p.name for p in PATCH_DIR.glob("*.patch"))
        for before in ("0009-", "0012-"):
            self.assertGreater(names.index(PATCH.name),
                               names.index(next(n for n in names if n.startswith(before))))
        script = (HERE / "fetch.sh").read_text(encoding="utf-8")
        self.assertIn('for patch in "$patch_dir"/guest/mesa/*.patch; do', script)
        tests = (HERE / "tests/test.sh").read_text(encoding="utf-8")
        self.assertIn('"$here/color_buffer_refs_test.cpp" -o "$out/color_buffer_refs_test"', tests)
        self.assertIn('"$out/color_buffer_refs_test"', tests)


if __name__ == "__main__":
    unittest.main()
