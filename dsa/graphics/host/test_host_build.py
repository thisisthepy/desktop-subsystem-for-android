"""Tests for the standalone host gfxstream renderer build scripts and CMake glue."""

import unittest
from pathlib import Path


HERE = Path(__file__).resolve().parent
BUILD_SCRIPT = HERE / "build.ps1"
FETCH_SCRIPT = HERE / "fetch.ps1"
CMAKE_FILE = HERE / "CMakeLists.txt"


class HostGraphicsBuildTests(unittest.TestCase):
    def test_pr3_host_build_script_invokes_cmake_for_gfxstream_backend(self):
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        self.assertIn("Ninja", script)
        self.assertIn("clang-cl", script)
        self.assertIn("gfxstream_backend", script)
        self.assertIn("dist_host_gfxstream", script)

    def test_pr3_host_build_keeps_the_renderers_symbols_apart_from_the_dll(self):
        # A crash in the renderer or in the GPU driver it calls is only readable with them.
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        # A compile-time PDB without a compiler cache, debug information in each object
        # under one; the linker writes the PDB from either.
        self.assertIn('{ "Embedded" } else { "ProgramDatabase" }', script)
        self.assertIn("CMAKE_MSVC_DEBUG_INFORMATION_FORMAT=$DebugInformationFormat", script)
        self.assertIn("/DEBUG", script)
        self.assertIn("symbols/gfxstream_backend.pdb", script)

    def test_pr3_host_cmake_glue_enforces_vulkan_and_standalone_aemu(self):
        cmake = CMAKE_FILE.read_text(encoding="utf-8")
        self.assertIn("VK_USE_PLATFORM_WIN32_KHR", cmake)
        self.assertIn("AEMU_COMMON_BUILD_CONFIG", cmake)
        self.assertIn("add_library(flatbuffers INTERFACE)", cmake)
        self.assertIn("add_library(libdrm_headers INTERFACE)", cmake)
        self.assertIn("add_library(gfxstream_egl_headers INTERFACE)", cmake)
        self.assertIn("emugl_config_stub.cpp", cmake)
        self.assertIn("add_custom_target(dist_host_gfxstream", cmake)

    def test_pr3_host_fetch_script_pins_upstream_and_dependencies(self):
        script = FETCH_SCRIPT.read_text(encoding="utf-8")
        self.assertIn("d047a57228332d995d36600792fa9ccc26cf8ae6", script)
        self.assertIn("3ff20aed307f55bc4ae865373d07bf76c9efa43c", script)
        self.assertIn("https://android.googlesource.com/platform/external/angle", script)

    def test_fr3_host_fetch_applies_the_patch_that_lends_out_the_vulkan_queue(self):
        script = FETCH_SCRIPT.read_text(encoding="utf-8")
        self.assertIn('"0004-*.patch"', script)
        # The script is in dsa/graphics/host, three levels below the repository root.
        self.assertIn('"$PSScriptRoot/../../.."', script)
        self.assertIn("throw", script)
        patch = (HERE.parent / "patches" / "0004-expose-the-vulkan-queue-to-the-host-backend.patch")
        text = patch.read_text(encoding="utf-8")
        self.assertIn("host/vulkan/VkCommonOperations.h", text)
        self.assertIn("getQueueLock", text)

    def test_pr3_host_renderer_can_be_asked_for_the_gpu_that_drives_the_display(self):
        script = FETCH_SCRIPT.read_text(encoding="utf-8")
        self.assertIn('"0007-*.patch"', script)
        patch = (HERE.parent / "patches" / "0007-select-the-gpu-the-embedder-asks-for.patch")
        text = patch.read_text(encoding="utf-8")
        self.assertIn("host/vulkan/VkCommonOperations.cpp", text)
        self.assertIn("idProps.deviceLUID, preference.luid, VK_LUID_SIZE", text)
        self.assertIn("void setGpuPreference(const GpuPreference& preference)", text)
        header = (HERE / "dsa_gfx_backend.h").read_text(encoding="utf-8")
        self.assertIn("dsa_gfx_select_gpu", header)
        self.assertIn("dsa_gfx_get_gpu_info", header)
        source = (HERE / "dsa_gfx_backend.cpp").read_text(encoding="utf-8")
        self.assertIn("gfxstream::vk::setGpuPreference(preference)", source)
        self.assertIn("gfxstream::vk::gpuPreferenceMatched()", source)

    def test_fr4_destroying_a_color_buffer_never_waits_for_the_gpu_queue(self):
        # A resize makes the guest destroy and create ColorBuffers for every size step. The
        # destroy runs under the renderer's FrameBuffer lock, which every creation needs, so
        # a destroy that waits for the queue to go idle stalls the guest's allocator.
        script = FETCH_SCRIPT.read_text(encoding="utf-8")
        self.assertIn('"0009-*.patch"', script)
        patch = (HERE.parent / "patches" /
                 "0009-free-vulkan-resources-without-waiting-for-the-queue.patch")
        text = patch.read_text(encoding="utf-8")
        self.assertIn("host/vulkan/VkCommonOperations.cpp", text)
        removed = [line for line in text.splitlines() if line.startswith("-")]
        added = [line for line in text.splitlines() if line.startswith("+")]
        # Both waits leave the teardown paths; the only wait left is the fallback when no
        # fence can be submitted.
        self.assertEqual(sum("vkQueueWaitIdle" in line for line in removed), 2)
        self.assertEqual(sum("vkQueueWaitIdle" in line for line in added), 1)
        self.assertIn("+        result = vk->vkQueueSubmit(mQueue, 0, nullptr, resource.fence);",
                      text)
        self.assertIn("vkGetFenceStatus(mDevice, it->fence)", text)
        # Nothing retired outlives the device.
        self.assertIn("+    freeRetiredLocked(/*wait=*/true);", text)

    def test_fr4_an_image_release_is_answered_once_the_gpu_has_finished(self):
        # The guest has no fences from the host, so the answer to a release is all that says the
        # image is ready. Upstream answers once the release is queued, the guest then composes
        # the buffer before the app's drawing has run, and a buffer new to a resize shows black.
        script = FETCH_SCRIPT.read_text(encoding="utf-8")
        self.assertIn('"0010-*.patch"', script)
        patch = (HERE.parent / "patches" /
                 "0010-answer-an-image-release-once-the-gpu-has-finished.patch")
        lines = patch.read_text(encoding="utf-8").splitlines()
        added = "\n".join(line for line in lines if line.startswith("+"))

        # The entry point the guest's waiting call reaches waits, and only after a release that
        # was queued; the guest's other, fenced entry point calls the implementation directly.
        self.assertIn("mImpl->waitForImageRelease(image);", added)
        self.assertIn("if (result == VK_SUCCESS) {", added)
        self.assertNotIn("AsyncGOOGLE", patch.read_text(encoding="utf-8"))

        # The wait is on what the release's own fence completes, and outside the decoder's lock:
        # the lock's scope closes before it, so another app's commands are not held up.
        wait = added[added.index("void waitForImageRelease"):]
        self.assertIn("latestRelease()", wait)
        self.assertLess(wait.index("std::lock_guard<std::mutex> lock(mMutex);"),
                        wait.index("release->wait();"))
        between = wait[wait.index("latestRelease()"):wait.index("release->wait();")]
        self.assertIn("+        }", between)

    def test_fr3_each_display_is_copied_into_its_window_on_the_gpu(self):
        header = (HERE / "dsa_gfx_backend.h").read_text(encoding="utf-8")
        self.assertIn("dsa_gfx_set_display_callbacks2", header)
        self.assertIn("dsa_gfx_get_adapter_luid", header)
        source = (HERE / "dsa_gfx_display.cpp").read_text(encoding="utf-8")
        # The frame goes into the window's Direct3D texture without a trip through the CPU.
        self.assertIn("VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT", source)
        self.assertIn("vkCmdBlitImage", source)
        # On Windows, that is: the Linux and macOS section reads a frame back only where the
        # desktop takes no GPU buffers.
        windows = source[source.index("#ifdef _WIN32"):source.index("#else /* !_WIN32 */")]
        self.assertNotIn("readColorBuffer", windows)
        self.assertIn("setDisplayColorBuffer", source)
        self.assertIn("dsa_gfx_display.cpp", CMAKE_FILE.read_text(encoding="utf-8"))

    def test_nfr2_a_display_that_lost_its_window_lets_its_texture_go_at_once(self):
        source = (HERE / "dsa_gfx_display.cpp").read_text(encoding="utf-8")
        windows = source[source.index("#ifdef _WIN32"):source.index("#else /* !_WIN32 */")]
        # Windows has a real forget, not a stub: the import is destroyed on the next pass.
        self.assertNotIn("dsa_gfx_forget_display(uint32_t) {}", windows)
        self.assertIn("dsa_gfx_forget_display(uint32_t display_id)", windows)
        self.assertIn("presenter->forget.empty()", windows)
        self.assertIn("presenter->targets.erase(it);", windows)

    def test_nfr2_the_renderer_says_what_its_vulkan_memory_holds(self):
        header = (HERE / "dsa_gfx_backend.h").read_text(encoding="utf-8")
        self.assertIn("dsa_gfx_get_memory_stats(dsa_gfx_memory_stats* stats)", header)
        source = (HERE / "dsa_gfx_memory.cpp").read_text(encoding="utf-8")
        # Every allocation and free of the renderer's device dispatch is counted by kind.
        self.assertIn("vk->vkAllocateMemory = counted_allocate;", source)
        self.assertIn("vk->vkFreeMemory = counted_free;", source)
        self.assertIn("VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT", source)
        backend = (HERE / "dsa_gfx_backend.cpp").read_text(encoding="utf-8")
        self.assertIn("dsa_gfx_memory_install();", backend)
        self.assertIn("dsa_gfx_memory_uninstall();", backend)
        self.assertEqual(backend.count("dsa_gfx_memory_process_ended();"), 2)
        self.assertIn("dsa_gfx_memory.cpp", CMAKE_FILE.read_text(encoding="utf-8"))

    def test_fr4_only_the_region_a_frame_fills_is_copied(self):
        source = (HERE / "dsa_gfx_display.cpp").read_text(encoding="utf-8")
        # The guest's buffers keep their size while a window is resized; the display's size
        # says how much of the buffer the frame fills, and only that is copied and shown.
        self.assertIn("operations.getDisplayPose", source)
        # In bands of rows of the region, none past the frame (dsa_copy_bands.h).
        self.assertIn("dsa::copy_bands(content_width, content_height);", source)
        self.assertIn("region.srcOffsets[1] = {copy_width, bottom, 1};", source)
        self.assertIn("region.dstOffsets[1] = {copy_width, bottom, 1};", source)
        self.assertIn(
            "presenter.frame_cb(presenter.context, display_id, content_width, content_height);",
            source,
        )

    def test_fr27_host_renderer_builds_for_arm64_windows(self):
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        # The platform picks the Visual Studio environment and the compiler's target, and
        # an ARM64 build on an x64 machine is a cross build.
        self.assertIn('[ValidateSet("x64", "ARM64")]', script)
        self.assertIn("vcvarsarm64.bat", script)
        self.assertIn("vcvarsamd64_arm64.bat", script)
        self.assertIn("aarch64-pc-windows-msvc", script)
        fetch = FETCH_SCRIPT.read_text(encoding="utf-8")
        self.assertIn('"0011-*.patch"', fetch)
        self.assertIn('Join-Path $PatchDir "aemu"', fetch)
        aemu = (HERE.parent / "patches" / "aemu" /
                "0001-yield-without-the-x86-pause-instruction.patch").read_text(encoding="utf-8")
        self.assertIn("-    _mm_pause();", aemu)
        self.assertIn("+    YieldProcessor();", aemu)
        gfx = (HERE.parent / "patches" /
               "0011-build-and-spin-on-arm64-windows.patch").read_text(encoding="utf-8")
        self.assertIn("+                        __builtin_arm_yield();", gfx)
        self.assertIn('+        setIcdPaths("lvp_icd.aarch64.json");', gfx)


    def test_pr3_renderer_frees_only_what_a_failed_vulkan_start_made(self):
        # Without a usable Vulkan driver the emulation is dropped half built, and its
        # destructor used to call through the device dispatch it never got.
        for script in (FETCH_SCRIPT, HERE / "fetch.sh"):
            self.assertIn("0012", script.read_text(encoding="utf-8"))
        patch = (HERE.parent / "patches" /
                 "0012-survive-a-host-without-a-vulkan-driver.patch").read_text(encoding="utf-8")
        self.assertIn("+    if (mDvk && mDevice != VK_NULL_HANDLE) {", patch)
        self.assertIn("+    if (mGvk && mInstance != VK_NULL_HANDLE) {", patch)

    def test_nfr5_finished_vulkan_garbage_is_destroyed_not_leaked(self):
        # The v0.1.9 renderer log said "had a waitable pending for -21043 milliseconds.
        # Leaking object." for fences whose work had finished: garbage is looked at only when
        # the device is used, and an entry older than five seconds was leaked whatever its
        # state, its age printed with the sign turned round.
        for script in (FETCH_SCRIPT, HERE / "fetch.sh"):
            self.assertIn("0016", script.read_text(encoding="utf-8"))
        patch = (HERE.parent / "patches" /
                 "0016-destroy-vulkan-garbage-whose-work-finished.patch").read_text(encoding="utf-8")
        self.assertIn("+            if (pendingGarbage.timepoint < old && !IsDone(pendingGarbage.waitable)) {", patch)
        self.assertIn("+                    now - pendingGarbage.timepoint);", patch)

    def test_nfr5_a_shared_color_buffer_opened_before_it_is_made_is_not_an_error(self):
        # Every guest process opens a gralloc buffer's shared ColorBuffer (top bit set) and
        # creates it when the open fails, so the v0.1.9 log was full of "openColorBuffer cb
        # handle -2147483645 not found" errors that were not errors.
        for script in (FETCH_SCRIPT, HERE / "fetch.sh"):
            self.assertIn("0017", script.read_text(encoding="utf-8"))
        patch = (HERE.parent / "patches" /
                 "0017-open-a-shared-color-buffer-without-an-error-when-it-is-new.patch").read_text(encoding="utf-8")
        self.assertIn("+            if (!(p_colorbuffer & 0x80000000u)) {", patch)
        header = (HERE.parent / "include" / "dsa_shared_color_buffer.h").read_text(encoding="utf-8")
        self.assertIn("#define DSA_SHARED_COLOR_BUFFER_BIT 0x80000000u", header)

    def test_nfr5_a_release_fence_the_gpu_has_not_signaled_is_never_used_again(self):
        # v0.2.1 on an Intel Arc GPU: "VkAndroidNativeBuffer.cpp:893: Timeout when waiting for
        # the Qsri fence." The timed-out fence went back to the pool still pending, and the
        # next release reset it and submitted with it again.
        for script in (FETCH_SCRIPT, HERE / "fetch.sh"):
            self.assertIn("0018", script.read_text(encoding="utf-8"))
        patch = (HERE.parent / "patches" /
                 "0018-leave-a-release-fence-the-gpu-has-not-signaled.patch").read_text(encoding="utf-8")
        self.assertIn("+                mQsriWaitFencePool->abandonFence(qsriFence);", patch)
        self.assertIn("+                emu->suspectDeviceLost();", patch)
        self.assertIn("-                VK_ANB_ERR(\"Timeout when waiting for the Qsri fence.\");", patch)

    def test_pr4_a_yuv_buffer_a_guest_cpu_writes_gets_a_yuv_colorbuffer(self):
        # Image test run 42724126311: every decoder's pictures showed green on screen, since
        # a YUV gralloc buffer had an RGBA ColorBuffer nothing wrote.
        for script in (FETCH_SCRIPT, HERE / "fetch.sh"):
            self.assertIn("0019", script.read_text(encoding="utf-8"))
        patch = next((HERE.parent / "patches").glob("0019-*.patch")).read_text(encoding="utf-8")
        self.assertIn("+bool FrameBuffer::updateColorBufferFromDsaPackedYuv(", patch)
        self.assertIn("+bool VkEmulation::supportsYuvColorBuffer(", patch)

    def test_nfr5_a_frame_copy_into_a_task_window_never_waits_without_an_end(self):
        # A copy the GPU did not finish in a second was waited for with UINT64_MAX, so a hung
        # GPU held the renderer's device, and every task window's frames, until Windows reset
        # it.
        display = (HERE / "dsa_gfx_display.cpp").read_text(encoding="utf-8")
        self.assertNotIn("UINT64_MAX", display)
        # Each band of a copy is waited for on its own: on Windows in the frame copy, on Linux
        # and macOS in the one copy the dmabuf and IOSurface paths share.
        self.assertEqual(display.count("finish_copy(*emu, presenter.fence, &took_ns)"), 0)
        self.assertEqual(display.count("finish_copy(*emu, presenter.fence, &band_ns)"), 1)
        self.assertEqual(display.count("finish_copy(emu, presenter.fence, &band_ns)"), 1)
        tests = (HERE.parent / "tests" / "test.sh").read_text(encoding="utf-8")
        self.assertIn("copy_wait_test.cpp", tests)

    def test_nfr5_no_copy_into_a_task_window_is_one_gpu_job_past_two_megapixels(self):
        # The owner's GPU was reset on 2026-11-12 just after a window was maximized to
        # 2880x1800 on a 200% monitor and back. Each band is a submission of its own, waited
        # for before the next, and the wait ends the moment the device is found lost.
        display = (HERE / "dsa_gfx_display.cpp").read_text(encoding="utf-8")
        windows = display[display.index("#ifdef _WIN32"):display.index("#else /* !_WIN32 */")]
        self.assertIn('#include "dsa_copy_bands.h"', display)
        self.assertIn("for (size_t band = 0; band < bands.size(); ++band) {", windows)
        loop = windows[windows.index("for (size_t band = 0;"):]
        self.assertLess(loop.index("vkQueueSubmit"), loop.index("finish_copy("))
        self.assertIn("[&] { return !emu.deviceUsable(); }, timeout_ns);", display)
        cmake = CMAKE_FILE.read_text(encoding="utf-8")
        self.assertIn('target_include_directories(gfxstream_backend PRIVATE "${CMAKE_CURRENT_SOURCE_DIR}/../include")', cmake)
        tests = (HERE.parent / "tests" / "test.sh").read_text(encoding="utf-8")
        self.assertIn("copy_bands_test.cpp", tests)

    def test_nfr5_linux_and_macos_copies_into_a_task_window_are_cut_into_bounded_jobs(self):
        # The dmabuf (Linux) and IOSurface (macOS) copies were one GPU job each, the size of
        # the window, as the Windows one was before. They now go through one banded copy, each
        # band submitted and waited for before the next, with the same 2 Mpixel limit.
        display = (HERE / "dsa_gfx_display.cpp").read_text(encoding="utf-8")
        unix = display[display.index("#else /* !_WIN32 */"):]
        banded = unix[unix.index("bool copy_in_bands("):unix.index("bool present_dmabuf(")]
        self.assertIn("dsa::copy_bands(width, height);", banded)
        loop = banded[banded.index("for (size_t band = 0; band < bands.size(); ++band) {"):]
        self.assertLess(loop.index("vkQueueSubmit"), loop.index("finish_copy("))
        # Each band covers its own rows of the frame, in the copy and in the blit.
        self.assertIn("region.extent = {width, rows, 1};", banded)
        self.assertIn("region.srcOffset = {0, top, 0};", banded)
        self.assertIn("region.srcOffsets[0] = {0, top, 0};", banded)
        # The images are taken over in the first band and handed back in the last.
        self.assertLess(banded.index("if (first) {"), banded.index("if (last) {"))
        # Neither path submits a copy of its own any more.
        dmabuf = unix[unix.index("bool present_dmabuf("):unix.index("#ifdef __APPLE__\n\nvoid release_import")]
        self.assertNotIn("vkQueueSubmit", dmabuf)
        self.assertIn("copy_in_bands(*emu, presenter, display_id, source->image, slot.image", dmabuf)
        iosurface = unix[unix.index("bool present_iosurface("):unix.index("#endif /* __APPLE__ */")]
        self.assertNotIn("vkQueueSubmit", iosurface)
        self.assertIn("copy_in_bands(*emu, presenter, display_id, source->image, target->image", iosurface)

    def test_pr3_renderer_on_macos_finds_moltenvk_without_being_told(self):
        # stream_renderer_init clears ANDROID_EMU_VK_ICD, and without portability enumeration
        # the loader hides MoltenVK: creating the instance failed with
        # VK_ERROR_INCOMPATIBLE_DRIVER on every Mac.
        for script in (FETCH_SCRIPT, HERE / "fetch.sh"):
            self.assertIn("0013", script.read_text(encoding="utf-8"))
        patch = (HERE.parent / "patches" /
                 "0013-use-moltenvk-when-the-host-vulkan-is-asked-for.patch").read_text(encoding="utf-8")
        self.assertIn('+        (vulkanIcd == "moltenvk") || (vulkanIcd.empty() && moltenVKSupported);', patch)
        self.assertIn('-    const bool moltenVKEnabled = (vulkanIcd == "moltenvk");', patch)

    def test_pr3_renderer_fails_rather_than_crashes_when_no_graphics_api_starts(self):
        # A guest that asks for Vulkan only gets no GL emulation, and when Vulkan then failed
        # the GL compositor was taken from the GL emulation that was never made (exit 139).
        for script in (FETCH_SCRIPT, HERE / "fetch.sh"):
            self.assertIn("0014", script.read_text(encoding="utf-8"))
        patch = (HERE.parent / "patches" /
                 "0014-fail-the-start-when-no-graphics-api-starts.patch").read_text(encoding="utf-8")
        self.assertIn("+    if (!fb->m_emulationVk && !fb->m_emulationGl) {", patch)
        self.assertIn("+        return false;", patch)

    def test_nfr5_renderer_survives_a_lost_device_and_a_bad_guest_command(self):
        # A GPU reset made every guest call end the window manager through on_DeviceLost, and
        # one guest's bad command did through the decoders' aborts.
        for script in (FETCH_SCRIPT, HERE / "fetch.sh"):
            self.assertIn("0015", script.read_text(encoding="utf-8"))
        patch = (HERE.parent / "patches" /
                 "0015-survive-a-lost-device-and-a-fatal-guest-error.patch").read_text(encoding="utf-8")
        self.assertIn("-        GFXSTREAM_ABORT(FatalError(VK_ERROR_DEVICE_LOST));", patch)
        self.assertIn("+    } catch (const emugl::GuestFatalError& error) {", patch)
        self.assertIn("+bool VkEmulation::remakeDeviceLocked() {", patch)
        self.assertNotIn("+            VK_CHECK(", patch)
        aemu = (HERE.parent / "patches" / "aemu" /
                "0002-end-a-guest-stream-rather-than-the-host-on-a-fatal-error.patch").read_text(encoding="utf-8")
        self.assertIn("+        throw GuestFatalError(mOss.str());", aemu)
        self.assertIn('+    OutputLog(stderr, \'F\', mFile, mLine, 0, "%s", mOss.str().c_str());', aemu)

    def test_nfr5_every_guest_channel_has_an_event_callback_for_its_stop(self):
        # The render thread stops a guest's channel from the host after a fatal command, and
        # the channel calls its event callback on a stop: an empty one threw
        # std::bad_function_call out of the render thread and ended the process.
        backend = (HERE / "dsa_gfx_backend.cpp").read_text(encoding="utf-8")
        self.assertEqual(backend.count("createRenderChannel("), 1)
        self.assertIn("channel->setEventCallback(", backend)
        self.assertEqual(backend.count("create_channel(renderer)"), 2)

    def test_pr3_gpu_preference_names_clear_x11_macros(self):
        # X11/X.h, which EGL pulls in on Linux, defines None as a macro.
        patch = (HERE.parent / "patches" /
                 "0007-select-the-gpu-the-embedder-asks-for.patch").read_text(encoding="utf-8")
        self.assertNotIn("Rule::None", patch)
        self.assertNotIn("Rule::None", (HERE / "dsa_gfx_backend.cpp").read_text(encoding="utf-8"))

if __name__ == "__main__":
    unittest.main()
