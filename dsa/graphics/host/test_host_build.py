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
        self.assertIn("CMAKE_MSVC_DEBUG_INFORMATION_FORMAT=ProgramDatabase", script)
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
        self.assertIn("region.srcOffsets[1] = {copy_width, copy_height, 1};", source)
        self.assertIn("region.dstOffsets[1] = {copy_width, copy_height, 1};", source)
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

    def test_pr3_gpu_preference_names_clear_x11_macros(self):
        # X11/X.h, which EGL pulls in on Linux, defines None as a macro.
        patch = (HERE.parent / "patches" /
                 "0007-select-the-gpu-the-embedder-asks-for.patch").read_text(encoding="utf-8")
        self.assertNotIn("Rule::None", patch)
        self.assertNotIn("Rule::None", (HERE / "dsa_gfx_backend.cpp").read_text(encoding="utf-8"))

if __name__ == "__main__":
    unittest.main()
