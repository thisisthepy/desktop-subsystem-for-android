/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

/* Per-display presentation: copies each display's newest color buffer into the Direct3D 11
 * texture the embedder (the compositor) names for it, on the renderer's own Vulkan queue.
 *
 * The color buffer stays at the largest size the task can have while its window is resized,
 * and the frame fills its top-left region, as large as the display's size (rcSetDisplayPose)
 * says. Only that region is copied, and the embedder is told its size.
 *
 * The guest sets a display's color buffer through the multi-display operations; this file
 * wraps that one operation so it also queues the frame for the presentation thread. The
 * thread keeps only the newest frame of each display, so a slow window never makes the
 * guest wait and never shows a stale frame after a newer one. */

#include "dsa_gfx_backend.h"
#include "dsa_gfx_display.h"

#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "FrameBuffer.h"
#include "host-common/misc.h"
#include "host-common/multi_display_agent.h"
#include "vulkan/BorrowedImageVk.h"
#include "vulkan/VkCommonOperations.h"

#ifdef _WIN32

namespace {

using gfxstream::FrameBuffer;
using gfxstream::vk::BorrowedImageInfoVk;
using gfxstream::vk::VkEmulation;
using gfxstream::vk::VulkanDispatch;

/* How long one copy may take on the GPU before the frame is given up. */
constexpr uint64_t kCopyTimeoutNs = 1000ull * 1000ull * 1000ull;

/* A texture of the embedder's, imported into Vulkan. */
struct Target {
    void* handle = nullptr;
    uint32_t width = 0;
    uint32_t height = 0;
    VkImage image = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
};

/* A frame waiting to be shown: its color buffer, and the display's size when it was set
 * (0 when the display has none). */
struct PendingFrame {
    uint32_t color_buffer = 0;
    uint32_t width = 0;
    uint32_t height = 0;
};

struct Presenter {
    std::mutex mutex;
    std::condition_variable wake;
    /* The newest frame set for each display, waiting to be shown. */
    std::map<uint32_t, PendingFrame> pending;
    /* Displays that lost their window: their imported textures go on the next pass. */
    std::vector<uint32_t> forget;
    bool stopping = false;
    std::thread thread;

    dsa_display_target_fn target_cb = nullptr;
    dsa_display_frame_fn frame_cb = nullptr;
    void* context = nullptr;

    /* Only the presentation thread touches these. */
    std::map<uint32_t, Target> targets;
    VkCommandPool pool = VK_NULL_HANDLE;
    VkCommandBuffer commands = VK_NULL_HANDLE;
    VkFence fence = VK_NULL_HANDLE;
    /* The renderer's device these objects were made on (VkEmulation::deviceGeneration). */
    uint64_t generation = 0;
};

std::mutex s_presenter_mutex;
Presenter* s_presenter = nullptr;
int (*s_original_set_display_color_buffer)(uint32_t, uint32_t) = nullptr;
int (*s_get_display_pose)(uint32_t, int32_t*, int32_t*, uint32_t*, uint32_t*) = nullptr;
bool s_hook_installed = false;

void log_once(const char* what) {
    static std::mutex log_mutex;
    static std::vector<std::string> said;
    std::lock_guard<std::mutex> lock(log_mutex);
    for (const auto& line : said) {
        if (line == what) {
            return;
        }
    }
    said.emplace_back(what);
    std::fprintf(stderr, "dsa_gfx: %s\n", what);
}

VkEmulation* emulation() {
    FrameBuffer* fb = FrameBuffer::getFB();
    if (!fb || !fb->hasEmulationVk()) {
        return nullptr;
    }
    return &fb->getEmulationVk();
}

int hooked_set_display_color_buffer(uint32_t display_id, uint32_t color_buffer) {
    int result = s_original_set_display_color_buffer
                     ? s_original_set_display_color_buffer(display_id, color_buffer)
                     : 0;
    if (color_buffer == 0) {
        return result;
    }
    /* Read here, on the thread that decodes the guest's commands, so the size is the one the
     * guest set just before this frame. */
    PendingFrame frame;
    frame.color_buffer = color_buffer;
    if (s_get_display_pose) {
        int32_t x = 0;
        int32_t y = 0;
        uint32_t w = 0;
        uint32_t h = 0;
        if (s_get_display_pose(display_id, &x, &y, &w, &h) == 0) {
            frame.width = w;
            frame.height = h;
        }
    }
    std::lock_guard<std::mutex> lock(s_presenter_mutex);
    if (s_presenter) {
        std::lock_guard<std::mutex> pending_lock(s_presenter->mutex);
        s_presenter->pending[display_id] = frame;
        s_presenter->wake.notify_one();
    }
    return result;
}

/* Wraps the multi-display operation that sets a display's color buffer, once. */
bool install_hook() {
    if (s_hook_installed) {
        return true;
    }
    QAndroidMultiDisplayAgent operations = emugl::get_emugl_multi_display_operations();
    s_original_set_display_color_buffer = operations.setDisplayColorBuffer;
    s_get_display_pose = operations.getDisplayPose;
    operations.setDisplayColorBuffer = hooked_set_display_color_buffer;
    emugl::set_emugl_multi_display_operations(operations);
    s_hook_installed = true;
    return true;
}

void destroy_target(VkEmulation& emu, Target& target) {
    VulkanDispatch* vk = emu.getDeviceDispatch();
    if (target.image != VK_NULL_HANDLE) {
        vk->vkDestroyImage(emu.getDevice(), target.image, nullptr);
    }
    if (target.memory != VK_NULL_HANDLE) {
        vk->vkFreeMemory(emu.getDevice(), target.memory, nullptr);
    }
    target = Target{};
}

/* Imports the embedder's texture as a Vulkan image the copy can write. */
bool import_target(VkEmulation& emu, void* handle, uint32_t width, uint32_t height,
                   Target* out) {
    VulkanDispatch* vk = emu.getDeviceDispatch();
    VkDevice device = emu.getDevice();
    if (!vk->vkGetMemoryWin32HandlePropertiesKHR) {
        log_once("the Vulkan driver cannot import Direct3D 11 textures "
                 "(VK_KHR_external_memory_win32), so no task window can show frames");
        return false;
    }
    const VkExternalMemoryHandleTypeFlagBits handle_type =
        VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT;

    VkExternalMemoryImageCreateInfo external_info = {};
    external_info.sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO;
    external_info.handleTypes = handle_type;

    VkImageCreateInfo image_info = {};
    image_info.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
    image_info.pNext = &external_info;
    image_info.imageType = VK_IMAGE_TYPE_2D;
    image_info.format = VK_FORMAT_R8G8B8A8_UNORM;
    image_info.extent = {width, height, 1};
    image_info.mipLevels = 1;
    image_info.arrayLayers = 1;
    image_info.samples = VK_SAMPLE_COUNT_1_BIT;
    image_info.tiling = VK_IMAGE_TILING_OPTIMAL;
    image_info.usage = VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
    image_info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    image_info.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;

    Target target;
    target.handle = handle;
    target.width = width;
    target.height = height;
    if (vk->vkCreateImage(device, &image_info, nullptr, &target.image) != VK_SUCCESS) {
        log_once("could not create a Vulkan image for a task window's texture");
        return false;
    }

    VkMemoryWin32HandlePropertiesKHR handle_properties = {};
    handle_properties.sType = VK_STRUCTURE_TYPE_MEMORY_WIN32_HANDLE_PROPERTIES_KHR;
    if (vk->vkGetMemoryWin32HandlePropertiesKHR(device, handle_type, handle,
                                                &handle_properties) != VK_SUCCESS) {
        log_once("the Vulkan driver refused a task window's Direct3D 11 texture");
        destroy_target(emu, target);
        return false;
    }
    VkMemoryRequirements requirements = {};
    vk->vkGetImageMemoryRequirements(device, target.image, &requirements);
    uint32_t type_bits = requirements.memoryTypeBits & handle_properties.memoryTypeBits;
    if (type_bits == 0) {
        type_bits = handle_properties.memoryTypeBits;
    }
    if (type_bits == 0) {
        log_once("no Vulkan memory type can hold a task window's texture");
        destroy_target(emu, target);
        return false;
    }
    uint32_t type_index = 0;
    while (!(type_bits & (1u << type_index))) {
        type_index++;
    }

    VkMemoryDedicatedAllocateInfo dedicated = {};
    dedicated.sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO;
    dedicated.image = target.image;

    VkImportMemoryWin32HandleInfoKHR import_info = {};
    import_info.sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_WIN32_HANDLE_INFO_KHR;
    import_info.pNext = &dedicated;
    import_info.handleType = handle_type;
    import_info.handle = static_cast<HANDLE>(handle);

    VkMemoryAllocateInfo allocate_info = {};
    allocate_info.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    allocate_info.pNext = &import_info;
    allocate_info.allocationSize = requirements.size;
    allocate_info.memoryTypeIndex = type_index;
    if (vk->vkAllocateMemory(device, &allocate_info, nullptr, &target.memory) != VK_SUCCESS) {
        log_once("could not import a task window's Direct3D 11 texture into Vulkan");
        destroy_target(emu, target);
        return false;
    }
    if (vk->vkBindImageMemory(device, target.image, target.memory, 0) != VK_SUCCESS) {
        log_once("could not bind a task window's texture to its Vulkan image");
        destroy_target(emu, target);
        return false;
    }
    *out = target;
    return true;
}

/* The renderer made a new device after a GPU reset: everything made on the old one went with
 * it, so it is forgotten, not destroyed, and made again on the new one as frames come. */
void follow_device(VkEmulation& emu, Presenter& presenter) {
    const uint64_t generation = emu.deviceGeneration();
    if (presenter.generation == generation) {
        return;
    }
    presenter.generation = generation;
    presenter.targets.clear();
    presenter.pool = VK_NULL_HANDLE;
    presenter.commands = VK_NULL_HANDLE;
    presenter.fence = VK_NULL_HANDLE;
    log_once("the renderer made a new Vulkan device after a GPU reset; task windows follow it");
}

bool ensure_commands(VkEmulation& emu, Presenter& presenter) {
    if (presenter.commands != VK_NULL_HANDLE) {
        return true;
    }
    VulkanDispatch* vk = emu.getDeviceDispatch();
    VkDevice device = emu.getDevice();
    VkCommandPoolCreateInfo pool_info = {};
    pool_info.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO;
    pool_info.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
    pool_info.queueFamilyIndex = emu.getQueueFamilyIndex();
    if (vk->vkCreateCommandPool(device, &pool_info, nullptr, &presenter.pool) != VK_SUCCESS) {
        return false;
    }
    VkCommandBufferAllocateInfo buffer_info = {};
    buffer_info.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO;
    buffer_info.commandPool = presenter.pool;
    buffer_info.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    buffer_info.commandBufferCount = 1;
    if (vk->vkAllocateCommandBuffers(device, &buffer_info, &presenter.commands) != VK_SUCCESS) {
        return false;
    }
    VkFenceCreateInfo fence_info = {};
    fence_info.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO;
    if (vk->vkCreateFence(device, &fence_info, nullptr, &presenter.fence) != VK_SUCCESS) {
        return false;
    }
    return true;
}

VkImageMemoryBarrier image_barrier(VkImage image, VkAccessFlags src_access,
                                   VkAccessFlags dst_access, VkImageLayout old_layout,
                                   VkImageLayout new_layout, uint32_t src_family,
                                   uint32_t dst_family) {
    VkImageMemoryBarrier barrier = {};
    barrier.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
    barrier.srcAccessMask = src_access;
    barrier.dstAccessMask = dst_access;
    barrier.oldLayout = old_layout;
    barrier.newLayout = new_layout;
    barrier.srcQueueFamilyIndex = src_family;
    barrier.dstQueueFamilyIndex = dst_family;
    barrier.image = image;
    barrier.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    return barrier;
}

void cmd_barriers(VulkanDispatch* vk, VkCommandBuffer commands, VkPipelineStageFlags src,
                  VkPipelineStageFlags dst, const std::vector<VkImageMemoryBarrier>& barriers) {
    if (barriers.empty()) {
        return;
    }
    vk->vkCmdPipelineBarrier(commands, src, dst, 0, 0, nullptr, 0, nullptr,
                             static_cast<uint32_t>(barriers.size()), barriers.data());
}

/* Copies the part of `frame`'s color buffer the frame fills into the display's texture and
 * tells the embedder. */
void present(Presenter& presenter, uint32_t display_id, const PendingFrame& frame) {
    const uint32_t color_buffer = frame.color_buffer;
    VkEmulation* emu = emulation();
    FrameBuffer* fb = FrameBuffer::getFB();
    if (!emu || !fb) {
        return;
    }
    /* A lost device is made again before the copy, and none is made while the copy uses it. */
    if (!emu->recoverIfLost()) {
        return;
    }
    auto device_use = emu->useDevice();
    follow_device(*emu, presenter);
    /* Held for the copy, so the guest destroying the buffer meanwhile cannot free its image. */
    auto keep_alive = fb->findColorBuffer(color_buffer);
    if (!keep_alive) {
        return;
    }
    const uint32_t width = keep_alive->getWidth();
    const uint32_t height = keep_alive->getHeight();
    if (width == 0 || height == 0) {
        return;
    }

    void* handle = presenter.target_cb(presenter.context, display_id, width, height);
    Target& target = presenter.targets[display_id];
    if (handle != target.handle || width != target.width || height != target.height) {
        destroy_target(*emu, target);
    }
    if (!handle) {
        presenter.targets.erase(display_id);
        return;
    }
    if (target.image == VK_NULL_HANDLE && !import_target(*emu, handle, width, height, &target)) {
        presenter.targets.erase(display_id);
        return;
    }
    if (!ensure_commands(*emu, presenter)) {
        log_once("could not set up the Vulkan commands that copy frames into task windows");
        return;
    }
    /* Borrowed last: borrowing records the layout the copy leaves the buffer in, so from
     * here on the copy must be submitted. */
    std::unique_ptr<BorrowedImageInfoVk> source = emu->borrowColorBufferForDisplay(color_buffer);
    if (!source) {
        return;
    }
    const uint32_t image_width = source->imageCreateInfo.extent.width;
    const uint32_t image_height = source->imageCreateInfo.extent.height;
    /* The region the frame fills: the display's size, clipped to the buffer, the texture and
     * the image. A display without a size fills the whole buffer. */
    uint32_t content_width = frame.width ? frame.width : width;
    uint32_t content_height = frame.height ? frame.height : height;
    content_width = std::min(std::min(content_width, width), image_width);
    content_height = std::min(std::min(content_height, height), image_height);
    if (content_width == 0 || content_height == 0) {
        return;
    }
    const int32_t copy_width = static_cast<int32_t>(content_width);
    const int32_t copy_height = static_cast<int32_t>(content_height);

    VulkanDispatch* vk = emu->getDeviceDispatch();
    VkDevice device = emu->getDevice();
    const uint32_t family = emu->getQueueFamilyIndex();

    std::vector<VkImageMemoryBarrier> acquire_transfers;
    std::vector<VkImageMemoryBarrier> acquire_layouts;
    std::vector<VkImageMemoryBarrier> release_layouts;
    std::vector<VkImageMemoryBarrier> release_transfers;
    gfxstream::vk::addNeededBarriersToUseBorrowedImage(
        *source, family, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
        VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_ACCESS_TRANSFER_READ_BIT, &acquire_transfers,
        &acquire_layouts, &release_layouts, &release_transfers);

    /* The texture's earlier contents do not matter; it comes from and goes back to
     * Direct3D. */
    acquire_layouts.push_back(image_barrier(
        target.image, 0, VK_ACCESS_TRANSFER_WRITE_BIT, VK_IMAGE_LAYOUT_UNDEFINED,
        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED));
    release_transfers.push_back(image_barrier(
        target.image, VK_ACCESS_TRANSFER_WRITE_BIT, 0, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        VK_IMAGE_LAYOUT_GENERAL, family, VK_QUEUE_FAMILY_EXTERNAL));

    VkCommandBufferBeginInfo begin = {};
    begin.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
    begin.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vk->vkResetCommandBuffer(presenter.commands, 0);
    vk->vkBeginCommandBuffer(presenter.commands, &begin);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                 VK_PIPELINE_STAGE_TRANSFER_BIT, acquire_transfers);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                 VK_PIPELINE_STAGE_TRANSFER_BIT, acquire_layouts);

    VkImageBlit region = {};
    region.srcSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.srcOffsets[1] = {copy_width, copy_height, 1};
    region.dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.dstOffsets[1] = {copy_width, copy_height, 1};
    vk->vkCmdBlitImage(presenter.commands, source->image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                       target.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region,
                       VK_FILTER_NEAREST);

    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_TRANSFER_BIT,
                 VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, release_layouts);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_TRANSFER_BIT,
                 VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, release_transfers);
    vk->vkEndCommandBuffer(presenter.commands);

    VkSubmitInfo submit = {};
    submit.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO;
    submit.commandBufferCount = 1;
    submit.pCommandBuffers = &presenter.commands;
    VkResult submitted;
    {
        android::base::AutoLock lock(*emu->getQueueLock());
        submitted = vk->vkQueueSubmit(emu->getQueue(), 1, &submit, presenter.fence);
    }
    if (submitted != VK_SUCCESS) {
        if (submitted == VK_ERROR_DEVICE_LOST) {
            /* The next frame finds a new device. */
            emu->noteDeviceLost();
        }
        log_once("submitting the copy of a frame into a task window failed");
        return;
    }
    VkResult waited = vk->vkWaitForFences(device, 1, &presenter.fence, VK_TRUE, kCopyTimeoutNs);
    if (waited == VK_ERROR_DEVICE_LOST) {
        emu->noteDeviceLost();
        log_once("the GPU was reset while a frame was copied into a task window");
        return;
    }
    if (waited != VK_SUCCESS) {
        /* The fence stays pending; wait for it before the next submit reuses the buffer. */
        log_once("copying a frame into a task window took longer than a second");
        if (vk->vkWaitForFences(device, 1, &presenter.fence, VK_TRUE, UINT64_MAX) ==
            VK_ERROR_DEVICE_LOST) {
            emu->noteDeviceLost();
            return;
        }
        vk->vkResetFences(device, 1, &presenter.fence);
        return;
    }
    vk->vkResetFences(device, 1, &presenter.fence);
    presenter.frame_cb(presenter.context, display_id, content_width, content_height);
}

void run(Presenter* presenter) {
    for (;;) {
        std::map<uint32_t, PendingFrame> frames;
        std::vector<uint32_t> forget;
        {
            std::unique_lock<std::mutex> lock(presenter->mutex);
            presenter->wake.wait(lock, [presenter] {
                return presenter->stopping || !presenter->pending.empty() ||
                       !presenter->forget.empty();
            });
            if (presenter->stopping) {
                break;
            }
            frames.swap(presenter->pending);
            forget.swap(presenter->forget);
        }
        if (!forget.empty()) {
            if (VkEmulation* emu = emulation()) {
                auto device_use = emu->useDevice();
                follow_device(*emu, *presenter);
                for (uint32_t display_id : forget) {
                    auto it = presenter->targets.find(display_id);
                    if (it != presenter->targets.end()) {
                        destroy_target(*emu, it->second);
                        presenter->targets.erase(it);
                    }
                }
            }
        }
        for (const auto& [display_id, frame] : frames) {
            present(*presenter, display_id, frame);
        }
    }
    VkEmulation* emu = emulation();
    if (!emu) {
        return;
    }
    auto device_use = emu->useDevice();
    follow_device(*emu, *presenter);
    for (auto& [display_id, target] : presenter->targets) {
        destroy_target(*emu, target);
    }
    presenter->targets.clear();
    VulkanDispatch* vk = emu->getDeviceDispatch();
    if (presenter->fence != VK_NULL_HANDLE) {
        vk->vkDestroyFence(emu->getDevice(), presenter->fence, nullptr);
    }
    if (presenter->pool != VK_NULL_HANDLE) {
        vk->vkDestroyCommandPool(emu->getDevice(), presenter->pool, nullptr);
    }
}

void stop_presenter() {
    Presenter* presenter = nullptr;
    {
        std::lock_guard<std::mutex> lock(s_presenter_mutex);
        presenter = s_presenter;
        s_presenter = nullptr;
    }
    if (!presenter) {
        return;
    }
    {
        std::lock_guard<std::mutex> lock(presenter->mutex);
        presenter->stopping = true;
        presenter->wake.notify_one();
    }
    if (presenter->thread.joinable()) {
        presenter->thread.join();
    }
    delete presenter;
}

}  // namespace

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_display_callbacks2(dsa_display_target_fn target,
                                                             dsa_display_frame_fn frame,
                                                             void* context) {
    stop_presenter();
    if (!target || !frame) {
        return 0;
    }
    if (!emulation()) {
        return -1;
    }
    if (!install_hook()) {
        return -1;
    }
    auto* presenter = new Presenter();
    presenter->target_cb = target;
    presenter->frame_cb = frame;
    presenter->context = context;
    presenter->thread = std::thread(run, presenter);
    std::lock_guard<std::mutex> lock(s_presenter_mutex);
    s_presenter = presenter;
    return 0;
}

extern "C" DSA_GFX_EXPORT void dsa_gfx_forget_display(uint32_t display_id) {
    std::lock_guard<std::mutex> lock(s_presenter_mutex);
    Presenter* presenter = s_presenter;
    if (!presenter) {
        return;
    }
    std::lock_guard<std::mutex> frames(presenter->mutex);
    presenter->pending.erase(display_id);
    presenter->forget.push_back(display_id);
    presenter->wake.notify_one();
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_get_adapter_luid(uint8_t luid[8]) {
    VkEmulation* emu = emulation();
    if (!emu || !luid) {
        return -1;
    }
    VulkanDispatch* ivk = emu->getInstanceDispatch();
    auto get_properties2 = ivk->vkGetPhysicalDeviceProperties2
                               ? ivk->vkGetPhysicalDeviceProperties2
                               : ivk->vkGetPhysicalDeviceProperties2KHR;
    if (!get_properties2) {
        return -1;
    }
    VkPhysicalDeviceIDProperties id_properties = {};
    id_properties.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES;
    VkPhysicalDeviceProperties2 properties = {};
    properties.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2;
    properties.pNext = &id_properties;
    get_properties2(emu->getPhysicalDevice(), &properties);
    if (!id_properties.deviceLUIDValid) {
        return -1;
    }
    std::memcpy(luid, id_properties.deviceLUID, 8);
    return 0;
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_display_pixels_callback(dsa_display_pixels_fn,
                                                                  void*) {
    return -1;
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_display_dmabuf_callbacks(dsa_display_dmabuf_fn,
                                                                   dsa_display_dmabuf_frame_fn,
                                                                   dsa_display_pixels_fn, void*) {
    return -1;
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_display_iosurface_callbacks(
    dsa_display_iosurface_fn, dsa_display_iosurface_frame_fn, dsa_display_pixels_fn, void*) {
    return -1;
}

void dsa_gfx_display_shutdown(void) { stop_presenter(); }

#else /* !_WIN32 */

/* On Linux there is no Direct3D texture to copy into. Each display's newest frame is copied on
 * the GPU into a ring of linear Vulkan images the renderer exports as dmabufs, which the
 * embedder hands to the Wayland compositor (linux-dmabuf): nothing goes through the CPU.
 * Where the driver cannot export a dmabuf, or the embedder asks for it, the frame is read
 * back to memory instead and handed over as pixels. The hook and the pending frames work as
 * on Windows: only the newest frame of each display is kept, so a slow window never makes the
 * guest wait. */

namespace {

using gfxstream::FrameBuffer;
using gfxstream::vk::BorrowedImageInfoVk;
using gfxstream::vk::VkEmulation;
using gfxstream::vk::VulkanDispatch;

constexpr uint32_t kGlRgba = 0x1908;
constexpr uint32_t kGlUnsignedByte = 0x1401;

/* The largest frame shown, in pixels on a side. A size beyond it is a broken guest. */
constexpr uint32_t kMaxSide = 16384;

/* How long one copy may take on the GPU before the frame is given up. */
constexpr uint64_t kCopyTimeoutNs = 1000ull * 1000ull * 1000ull;

/* How many images each display cycles through, so the compositor reads one while the next
 * frame is copied into another. */
constexpr uint32_t kSlots = 3;

/* DRM_FORMAT_ABGR8888: R, G, B, A in memory, as VK_FORMAT_R8G8B8A8_UNORM. */
constexpr uint32_t kFourccAbgr8888 = 0x34324241;
constexpr uint64_t kModifierLinear = 0;

struct PendingFrame {
    uint32_t color_buffer = 0;
    uint32_t width = 0;
    uint32_t height = 0;
};

/* One exported image. */
struct Slot {
    uint32_t width = 0;
    uint32_t height = 0;
    VkImage image = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
};

struct Display {
    Slot slots[kSlots];
    uint32_t next = 0;
};

#ifdef __APPLE__
/* CoreFoundation's reference counting, which an IOSurfaceRef follows. */
extern "C" const void* CFRetain(const void* object);
extern "C" void CFRelease(const void* object);

/* VkImportMetalIOSurfaceInfoEXT (VK_EXT_metal_objects), laid out as the Vulkan headers
 * declare it, so the file builds whether or not they were included with the Metal
 * platform. */
struct ImportIoSurfaceInfo {
    VkStructureType sType;
    const void* pNext;
    void* ioSurface;
};
constexpr VkStructureType kImportMetalIoSurfaceInfo = static_cast<VkStructureType>(1000311009);

/* How many of a display's IOSurfaces stay imported: the embedder cycles through two. */
constexpr size_t kImportsPerDisplay = 3;

/* One of the embedder's IOSurfaces, imported into Vulkan and held while it is. */
struct SurfaceImport {
    const void* surface = nullptr;
    uint32_t width = 0;
    uint32_t height = 0;
    VkImage image = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    uint64_t last_used = 0;
};
#endif

struct PixelPresenter {
    std::mutex mutex;
    std::condition_variable wake;
    std::map<uint32_t, PendingFrame> pending;
    /* Displays the embedder no longer shows. */
    std::vector<uint32_t> forget;
    bool stopping = false;
    std::thread thread;
    dsa_display_pixels_fn pixels_cb = nullptr;
    dsa_display_dmabuf_fn dmabuf_cb = nullptr;
    dsa_display_dmabuf_frame_fn dmabuf_frame_cb = nullptr;
    dsa_display_iosurface_fn iosurface_cb = nullptr;
    dsa_display_iosurface_frame_fn iosurface_frame_cb = nullptr;
    void* context = nullptr;
    /* Only the presentation thread touches these. */
    std::vector<uint8_t> buffer;
    std::map<uint32_t, Display> displays;
    bool dmabuf_failed = false;
    bool iosurface_failed = false;
#ifdef __APPLE__
    std::map<uint32_t, std::vector<SurfaceImport>> imports;
    uint64_t uses = 0;
#endif
    VkCommandPool pool = VK_NULL_HANDLE;
    VkCommandBuffer commands = VK_NULL_HANDLE;
    VkFence fence = VK_NULL_HANDLE;
    /* The renderer's device these objects were made on (VkEmulation::deviceGeneration). */
    uint64_t generation = 0;
};

std::mutex s_presenter_mutex;
PixelPresenter* s_presenter = nullptr;
int (*s_original_set_display_color_buffer)(uint32_t, uint32_t) = nullptr;
int (*s_get_display_pose)(uint32_t, int32_t*, int32_t*, uint32_t*, uint32_t*) = nullptr;
bool s_hook_installed = false;

void log_once(const char* what) {
    static std::mutex log_mutex;
    static std::vector<std::string> said;
    std::lock_guard<std::mutex> lock(log_mutex);
    for (const auto& line : said) {
        if (line == what) {
            return;
        }
    }
    said.emplace_back(what);
    std::fprintf(stderr, "dsa_gfx: %s\n", what);
}

VkEmulation* emulation() {
    FrameBuffer* fb = FrameBuffer::getFB();
    if (!fb || !fb->hasEmulationVk()) {
        return nullptr;
    }
    return &fb->getEmulationVk();
}

int hooked_set_display_color_buffer(uint32_t display_id, uint32_t color_buffer) {
    int result = s_original_set_display_color_buffer
                     ? s_original_set_display_color_buffer(display_id, color_buffer)
                     : 0;
    if (color_buffer == 0) {
        return result;
    }
    PendingFrame frame;
    frame.color_buffer = color_buffer;
    if (s_get_display_pose) {
        int32_t x = 0;
        int32_t y = 0;
        uint32_t w = 0;
        uint32_t h = 0;
        if (s_get_display_pose(display_id, &x, &y, &w, &h) == 0) {
            frame.width = w;
            frame.height = h;
        }
    }
    std::lock_guard<std::mutex> lock(s_presenter_mutex);
    if (s_presenter) {
        std::lock_guard<std::mutex> pending_lock(s_presenter->mutex);
        s_presenter->pending[display_id] = frame;
        s_presenter->wake.notify_one();
    }
    return result;
}

bool install_hook() {
    if (s_hook_installed) {
        return true;
    }
    QAndroidMultiDisplayAgent operations = emugl::get_emugl_multi_display_operations();
    s_original_set_display_color_buffer = operations.setDisplayColorBuffer;
    s_get_display_pose = operations.getDisplayPose;
    operations.setDisplayColorBuffer = hooked_set_display_color_buffer;
    emugl::set_emugl_multi_display_operations(operations);
    s_hook_installed = true;
    return true;
}

void destroy_slot(VkEmulation& emu, Slot& slot) {
    VulkanDispatch* vk = emu.getDeviceDispatch();
    if (slot.image != VK_NULL_HANDLE) {
        vk->vkDestroyImage(emu.getDevice(), slot.image, nullptr);
    }
    if (slot.memory != VK_NULL_HANDLE) {
        vk->vkFreeMemory(emu.getDevice(), slot.memory, nullptr);
    }
    slot = Slot{};
}

/* Makes a linear image the copy can write and the compositor can read, and exports its
 * memory as a dmabuf to the embedder. */
bool export_slot(VkEmulation& emu, PixelPresenter& presenter, uint32_t display_id,
                 uint32_t index, uint32_t width, uint32_t height, Slot* out) {
    VulkanDispatch* vk = emu.getDeviceDispatch();
    VkDevice device = emu.getDevice();
    if (!emu.supportsDmaBuf()) {
        log_once("the Vulkan device cannot export dmabufs (VK_EXT_external_memory_dma_buf); "
                 "frames are read back instead");
        return false;
    }
    if (!vk->vkGetMemoryFdKHR) {
        log_once("the Vulkan driver cannot export memory (VK_KHR_external_memory_fd); "
                 "frames are read back instead");
        return false;
    }
    VkExternalMemoryImageCreateInfo external_info = {};
    external_info.sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO;
    external_info.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;

    VkImageCreateInfo image_info = {};
    image_info.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
    image_info.pNext = &external_info;
    image_info.imageType = VK_IMAGE_TYPE_2D;
    image_info.format = VK_FORMAT_R8G8B8A8_UNORM;
    image_info.extent = {width, height, 1};
    image_info.mipLevels = 1;
    image_info.arrayLayers = 1;
    image_info.samples = VK_SAMPLE_COUNT_1_BIT;
    image_info.tiling = VK_IMAGE_TILING_LINEAR;
    image_info.usage = VK_IMAGE_USAGE_TRANSFER_DST_BIT;
    image_info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    image_info.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;

    Slot slot;
    slot.width = width;
    slot.height = height;
    if (vk->vkCreateImage(device, &image_info, nullptr, &slot.image) != VK_SUCCESS) {
        log_once("the Vulkan driver cannot make an exportable image; frames are read back");
        return false;
    }
    VkMemoryRequirements requirements = {};
    vk->vkGetImageMemoryRequirements(device, slot.image, &requirements);
    uint32_t type_index = UINT32_MAX;
    VkPhysicalDeviceMemoryProperties memory = {};
    emu.getInstanceDispatch()->vkGetPhysicalDeviceMemoryProperties(emu.getPhysicalDevice(),
                                                                   &memory);
    for (uint32_t i = 0; i < memory.memoryTypeCount; i++) {
        if ((requirements.memoryTypeBits & (1u << i)) &&
            (memory.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) {
            type_index = i;
            break;
        }
    }
    if (type_index == UINT32_MAX) {
        for (uint32_t i = 0; i < memory.memoryTypeCount; i++) {
            if (requirements.memoryTypeBits & (1u << i)) {
                type_index = i;
                break;
            }
        }
    }
    if (type_index == UINT32_MAX) {
        destroy_slot(emu, slot);
        return false;
    }
    VkMemoryDedicatedAllocateInfo dedicated = {};
    dedicated.sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO;
    dedicated.image = slot.image;
    VkExportMemoryAllocateInfo export_info = {};
    export_info.sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO;
    export_info.pNext = &dedicated;
    export_info.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;
    VkMemoryAllocateInfo allocate_info = {};
    allocate_info.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    allocate_info.pNext = &export_info;
    allocate_info.allocationSize = requirements.size;
    allocate_info.memoryTypeIndex = type_index;
    if (vk->vkAllocateMemory(device, &allocate_info, nullptr, &slot.memory) != VK_SUCCESS ||
        vk->vkBindImageMemory(device, slot.image, slot.memory, 0) != VK_SUCCESS) {
        log_once("the Vulkan driver cannot export an image as a dmabuf; frames are read back");
        destroy_slot(emu, slot);
        return false;
    }
    VkMemoryGetFdInfoKHR fd_info = {};
    fd_info.sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR;
    fd_info.memory = slot.memory;
    fd_info.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;
    int fd = -1;
    if (vk->vkGetMemoryFdKHR(device, &fd_info, &fd) != VK_SUCCESS || fd < 0) {
        log_once("the Vulkan driver did not export a dmabuf; frames are read back");
        destroy_slot(emu, slot);
        return false;
    }
    VkImageSubresource subresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0};
    VkSubresourceLayout layout = {};
    vk->vkGetImageSubresourceLayout(device, slot.image, &subresource, &layout);
    /* The embedder owns the descriptor from here, whatever it answers. */
    if (presenter.dmabuf_cb(presenter.context, display_id, index, width, height, fd,
                            static_cast<uint32_t>(layout.rowPitch),
                            static_cast<uint32_t>(layout.offset), kFourccAbgr8888,
                            kModifierLinear) != 0) {
        destroy_slot(emu, slot);
        return false;
    }
    *out = slot;
    return true;
}

bool ensure_commands(VkEmulation& emu, PixelPresenter& presenter) {
    if (presenter.commands != VK_NULL_HANDLE) {
        return true;
    }
    VulkanDispatch* vk = emu.getDeviceDispatch();
    VkDevice device = emu.getDevice();
    VkCommandPoolCreateInfo pool_info = {};
    pool_info.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO;
    pool_info.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
    pool_info.queueFamilyIndex = emu.getQueueFamilyIndex();
    if (vk->vkCreateCommandPool(device, &pool_info, nullptr, &presenter.pool) != VK_SUCCESS) {
        return false;
    }
    VkCommandBufferAllocateInfo buffer_info = {};
    buffer_info.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO;
    buffer_info.commandPool = presenter.pool;
    buffer_info.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    buffer_info.commandBufferCount = 1;
    if (vk->vkAllocateCommandBuffers(device, &buffer_info, &presenter.commands) != VK_SUCCESS) {
        return false;
    }
    VkFenceCreateInfo fence_info = {};
    fence_info.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO;
    return vk->vkCreateFence(device, &fence_info, nullptr, &presenter.fence) == VK_SUCCESS;
}

VkImageMemoryBarrier image_barrier(VkImage image, VkAccessFlags src_access,
                                   VkAccessFlags dst_access, VkImageLayout old_layout,
                                   VkImageLayout new_layout, uint32_t src_family,
                                   uint32_t dst_family) {
    VkImageMemoryBarrier barrier = {};
    barrier.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
    barrier.srcAccessMask = src_access;
    barrier.dstAccessMask = dst_access;
    barrier.oldLayout = old_layout;
    barrier.newLayout = new_layout;
    barrier.srcQueueFamilyIndex = src_family;
    barrier.dstQueueFamilyIndex = dst_family;
    barrier.image = image;
    barrier.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
    return barrier;
}

void cmd_barriers(VulkanDispatch* vk, VkCommandBuffer commands, VkPipelineStageFlags src,
                  VkPipelineStageFlags dst, const std::vector<VkImageMemoryBarrier>& barriers) {
    if (barriers.empty()) {
        return;
    }
    vk->vkCmdPipelineBarrier(commands, src, dst, 0, 0, nullptr, 0, nullptr,
                             static_cast<uint32_t>(barriers.size()), barriers.data());
}

/* Copies the frame into the display's next dmabuf on the GPU. False when it cannot, and the
 * frame is read back instead. */
bool present_dmabuf(PixelPresenter& presenter, uint32_t display_id, const PendingFrame& frame) {
    VkEmulation* emu = emulation();
    FrameBuffer* fb = FrameBuffer::getFB();
    if (!emu || !fb) {
        return false;
    }
    auto keep_alive = fb->findColorBuffer(frame.color_buffer);
    if (!keep_alive) {
        return true;
    }
    const uint32_t width = keep_alive->getWidth();
    const uint32_t height = keep_alive->getHeight();
    if (width == 0 || height == 0 || width > kMaxSide || height > kMaxSide) {
        return true;
    }
    Display& display = presenter.displays[display_id];
    const uint32_t index = display.next;
    Slot& slot = display.slots[index];
    if (slot.width != width || slot.height != height) {
        destroy_slot(*emu, slot);
        if (!export_slot(*emu, presenter, display_id, index, width, height, &slot)) {
            return false;
        }
    }
    if (!ensure_commands(*emu, presenter)) {
        log_once("could not set up the Vulkan commands that copy frames into task windows");
        return false;
    }
    std::unique_ptr<BorrowedImageInfoVk> source = emu->borrowColorBufferForDisplay(frame.color_buffer);
    if (!source) {
        return true;
    }
    uint32_t content_width = frame.width ? frame.width : width;
    uint32_t content_height = frame.height ? frame.height : height;
    content_width = std::min(std::min(content_width, width), source->imageCreateInfo.extent.width);
    content_height =
        std::min(std::min(content_height, height), source->imageCreateInfo.extent.height);
    if (content_width == 0 || content_height == 0) {
        return true;
    }
    VulkanDispatch* vk = emu->getDeviceDispatch();
    VkDevice device = emu->getDevice();
    const uint32_t family = emu->getQueueFamilyIndex();

    std::vector<VkImageMemoryBarrier> acquire_transfers;
    std::vector<VkImageMemoryBarrier> acquire_layouts;
    std::vector<VkImageMemoryBarrier> release_layouts;
    std::vector<VkImageMemoryBarrier> release_transfers;
    gfxstream::vk::addNeededBarriersToUseBorrowedImage(
        *source, family, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
        VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_ACCESS_TRANSFER_READ_BIT, &acquire_transfers,
        &acquire_layouts, &release_layouts, &release_transfers);
    acquire_layouts.push_back(image_barrier(
        slot.image, 0, VK_ACCESS_TRANSFER_WRITE_BIT, VK_IMAGE_LAYOUT_UNDEFINED,
        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED));
    release_transfers.push_back(image_barrier(
        slot.image, VK_ACCESS_TRANSFER_WRITE_BIT, 0, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        VK_IMAGE_LAYOUT_GENERAL, family, VK_QUEUE_FAMILY_FOREIGN_EXT));

    VkCommandBufferBeginInfo begin = {};
    begin.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
    begin.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vk->vkResetCommandBuffer(presenter.commands, 0);
    vk->vkBeginCommandBuffer(presenter.commands, &begin);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                 VK_PIPELINE_STAGE_TRANSFER_BIT, acquire_transfers);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                 VK_PIPELINE_STAGE_TRANSFER_BIT, acquire_layouts);
    VkImageCopy region = {};
    region.srcSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.extent = {content_width, content_height, 1};
    vk->vkCmdCopyImage(presenter.commands, source->image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                       slot.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_TRANSFER_BIT,
                 VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, release_layouts);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_TRANSFER_BIT,
                 VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, release_transfers);
    vk->vkEndCommandBuffer(presenter.commands);

    VkSubmitInfo submit = {};
    submit.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO;
    submit.commandBufferCount = 1;
    submit.pCommandBuffers = &presenter.commands;
    VkResult submitted;
    {
        android::base::AutoLock lock(*emu->getQueueLock());
        submitted = vk->vkQueueSubmit(emu->getQueue(), 1, &submit, presenter.fence);
    }
    if (submitted != VK_SUCCESS) {
        if (submitted == VK_ERROR_DEVICE_LOST) {
            /* The next frame finds a new device. */
            emu->noteDeviceLost();
        }
        log_once("submitting the copy of a frame into a task window failed");
        return true;
    }
    const VkResult waited =
        vk->vkWaitForFences(device, 1, &presenter.fence, VK_TRUE, kCopyTimeoutNs);
    if (waited == VK_ERROR_DEVICE_LOST) {
        emu->noteDeviceLost();
        log_once("the GPU was reset while a frame was copied into a task window");
        return true;
    }
    if (waited != VK_SUCCESS) {
        log_once("copying a frame into a task window took longer than a second");
        vk->vkWaitForFences(device, 1, &presenter.fence, VK_TRUE, UINT64_MAX);
    }
    vk->vkResetFences(device, 1, &presenter.fence);
    display.next = (index + 1) % kSlots;
    presenter.dmabuf_frame_cb(presenter.context, display_id, index, content_width,
                              content_height);
    return true;
}

#ifdef __APPLE__

void release_import(VkEmulation& emu, SurfaceImport& import) {
    VulkanDispatch* vk = emu.getDeviceDispatch();
    if (import.image != VK_NULL_HANDLE) {
        vk->vkDestroyImage(emu.getDevice(), import.image, nullptr);
    }
    if (import.memory != VK_NULL_HANDLE) {
        vk->vkFreeMemory(emu.getDevice(), import.memory, nullptr);
    }
    if (import.surface) {
        CFRelease(import.surface);
    }
    import = SurfaceImport{};
}

void forget_display(PixelPresenter& presenter, uint32_t display_id) {
    auto found = presenter.imports.find(display_id);
    if (found == presenter.imports.end()) {
        return;
    }
    VkEmulation* emu = emulation();
    if (emu) {
        for (SurfaceImport& import : found->second) {
            release_import(*emu, import);
        }
    }
    presenter.imports.erase(found);
}

/* Whether the Vulkan device can take an IOSurface as an image's storage. */
bool imports_iosurfaces(VkEmulation& emu) {
    static int answer = -1;
    if (answer >= 0) {
        return answer == 1;
    }
    answer = 0;
    VulkanDispatch* ivk = emu.getInstanceDispatch();
    uint32_t count = 0;
    if (ivk->vkEnumerateDeviceExtensionProperties(emu.getPhysicalDevice(), nullptr, &count,
                                                  nullptr) != VK_SUCCESS) {
        return false;
    }
    std::vector<VkExtensionProperties> extensions(count);
    if (ivk->vkEnumerateDeviceExtensionProperties(emu.getPhysicalDevice(), nullptr, &count,
                                                  extensions.data()) != VK_SUCCESS) {
        return false;
    }
    for (const VkExtensionProperties& extension : extensions) {
        if (std::strcmp(extension.extensionName, "VK_EXT_metal_objects") == 0) {
            answer = 1;
        }
    }
    return answer == 1;
}

/* Makes a Vulkan image whose storage is the embedder's IOSurface, and holds the surface. */
bool import_surface(VkEmulation& emu, const void* surface, uint32_t width, uint32_t height,
                    SurfaceImport* out) {
    VulkanDispatch* vk = emu.getDeviceDispatch();
    VkDevice device = emu.getDevice();
    ImportIoSurfaceInfo import_info = {};
    import_info.sType = kImportMetalIoSurfaceInfo;
    import_info.ioSurface = const_cast<void*>(surface);

    VkImageCreateInfo image_info = {};
    image_info.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
    image_info.pNext = &import_info;
    image_info.imageType = VK_IMAGE_TYPE_2D;
    image_info.format = VK_FORMAT_B8G8R8A8_UNORM;
    image_info.extent = {width, height, 1};
    image_info.mipLevels = 1;
    image_info.arrayLayers = 1;
    image_info.samples = VK_SAMPLE_COUNT_1_BIT;
    image_info.tiling = VK_IMAGE_TILING_OPTIMAL;
    image_info.usage = VK_IMAGE_USAGE_TRANSFER_DST_BIT;
    image_info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    image_info.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;

    SurfaceImport import;
    import.width = width;
    import.height = height;
    if (vk->vkCreateImage(device, &image_info, nullptr, &import.image) != VK_SUCCESS) {
        log_once("the Vulkan driver cannot make an image of an IOSurface; frames are read back");
        return false;
    }
    VkMemoryRequirements requirements = {};
    vk->vkGetImageMemoryRequirements(device, import.image, &requirements);
    VkPhysicalDeviceMemoryProperties memory = {};
    emu.getInstanceDispatch()->vkGetPhysicalDeviceMemoryProperties(emu.getPhysicalDevice(),
                                                                   &memory);
    uint32_t type_index = UINT32_MAX;
    for (uint32_t i = 0; i < memory.memoryTypeCount; i++) {
        if ((requirements.memoryTypeBits & (1u << i)) &&
            (memory.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) {
            type_index = i;
            break;
        }
    }
    if (type_index == UINT32_MAX) {
        for (uint32_t i = 0; i < memory.memoryTypeCount; i++) {
            if (requirements.memoryTypeBits & (1u << i)) {
                type_index = i;
                break;
            }
        }
    }
    /* MoltenVK keeps the image's texture on the IOSurface; the memory bound is only what
     * the API asks for. */
    VkMemoryDedicatedAllocateInfo dedicated = {};
    dedicated.sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO;
    dedicated.image = import.image;
    VkMemoryAllocateInfo allocate_info = {};
    allocate_info.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
    allocate_info.pNext = &dedicated;
    allocate_info.allocationSize = requirements.size;
    allocate_info.memoryTypeIndex = type_index;
    if (type_index == UINT32_MAX ||
        vk->vkAllocateMemory(device, &allocate_info, nullptr, &import.memory) != VK_SUCCESS ||
        vk->vkBindImageMemory(device, import.image, import.memory, 0) != VK_SUCCESS) {
        log_once("the Vulkan driver cannot bind an IOSurface image; frames are read back");
        release_import(emu, import);
        return false;
    }
    import.surface = CFRetain(surface);
    *out = import;
    return true;
}

/* The display's import of `surface`, made when it is new. The oldest import goes when the
 * display holds more than it cycles through. */
SurfaceImport* surface_import(VkEmulation& emu, PixelPresenter& presenter, uint32_t display_id,
                              const void* surface, uint32_t width, uint32_t height) {
    std::vector<SurfaceImport>& imports = presenter.imports[display_id];
    presenter.uses++;
    for (SurfaceImport& import : imports) {
        if (import.surface == surface && import.width == width && import.height == height) {
            import.last_used = presenter.uses;
            return &import;
        }
    }
    SurfaceImport import;
    if (!import_surface(emu, surface, width, height, &import)) {
        return nullptr;
    }
    import.last_used = presenter.uses;
    while (imports.size() >= kImportsPerDisplay) {
        auto oldest = std::min_element(imports.begin(), imports.end(),
                                       [](const SurfaceImport& a, const SurfaceImport& b) {
                                           return a.last_used < b.last_used;
                                       });
        release_import(emu, *oldest);
        imports.erase(oldest);
    }
    imports.push_back(import);
    return &imports.back();
}

/* Copies the frame into the IOSurface the embedder gives, on the GPU. False when the device
 * cannot, and the frame is read back instead. */
bool present_iosurface(PixelPresenter& presenter, uint32_t display_id, const PendingFrame& frame) {
    VkEmulation* emu = emulation();
    FrameBuffer* fb = FrameBuffer::getFB();
    if (!emu || !fb) {
        return false;
    }
    if (!imports_iosurfaces(*emu)) {
        log_once("the Vulkan device has no VK_EXT_metal_objects; frames are read back");
        return false;
    }
    auto keep_alive = fb->findColorBuffer(frame.color_buffer);
    if (!keep_alive) {
        return true;
    }
    const uint32_t width = keep_alive->getWidth();
    const uint32_t height = keep_alive->getHeight();
    if (width == 0 || height == 0 || width > kMaxSide || height > kMaxSide) {
        return true;
    }
    void* surface = presenter.iosurface_cb(presenter.context, display_id, width, height);
    if (!surface) {
        /* Shown nowhere: what was imported for the display goes. */
        forget_display(presenter, display_id);
        return true;
    }
    SurfaceImport* target = surface_import(*emu, presenter, display_id, surface, width, height);
    if (!target) {
        return false;
    }
    if (!ensure_commands(*emu, presenter)) {
        log_once("could not set up the Vulkan commands that copy frames into task windows");
        return false;
    }
    std::unique_ptr<BorrowedImageInfoVk> source = emu->borrowColorBufferForDisplay(frame.color_buffer);
    if (!source) {
        return true;
    }
    uint32_t content_width = frame.width ? frame.width : width;
    uint32_t content_height = frame.height ? frame.height : height;
    content_width = std::min(std::min(content_width, width), source->imageCreateInfo.extent.width);
    content_height =
        std::min(std::min(content_height, height), source->imageCreateInfo.extent.height);
    if (content_width == 0 || content_height == 0) {
        return true;
    }
    VulkanDispatch* vk = emu->getDeviceDispatch();
    VkDevice device = emu->getDevice();
    const uint32_t family = emu->getQueueFamilyIndex();

    std::vector<VkImageMemoryBarrier> acquire_transfers;
    std::vector<VkImageMemoryBarrier> acquire_layouts;
    std::vector<VkImageMemoryBarrier> release_layouts;
    std::vector<VkImageMemoryBarrier> release_transfers;
    gfxstream::vk::addNeededBarriersToUseBorrowedImage(
        *source, family, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
        VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_ACCESS_TRANSFER_READ_BIT, &acquire_transfers,
        &acquire_layouts, &release_layouts, &release_transfers);
    acquire_layouts.push_back(image_barrier(
        target->image, 0, VK_ACCESS_TRANSFER_WRITE_BIT, VK_IMAGE_LAYOUT_UNDEFINED,
        VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED));
    release_layouts.push_back(image_barrier(
        target->image, VK_ACCESS_TRANSFER_WRITE_BIT, 0, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
        VK_IMAGE_LAYOUT_GENERAL, VK_QUEUE_FAMILY_IGNORED, VK_QUEUE_FAMILY_IGNORED));

    VkCommandBufferBeginInfo begin = {};
    begin.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
    begin.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vk->vkResetCommandBuffer(presenter.commands, 0);
    vk->vkBeginCommandBuffer(presenter.commands, &begin);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                 VK_PIPELINE_STAGE_TRANSFER_BIT, acquire_transfers);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                 VK_PIPELINE_STAGE_TRANSFER_BIT, acquire_layouts);
    /* A blit, not a copy: the color buffer is RGBA and the surface BGRA, and only a blit
     * converts between them. The sizes match, so nothing is scaled. */
    VkImageBlit region = {};
    region.srcSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.srcOffsets[1] = {static_cast<int32_t>(content_width),
                            static_cast<int32_t>(content_height), 1};
    region.dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.dstOffsets[1] = region.srcOffsets[1];
    vk->vkCmdBlitImage(presenter.commands, source->image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                       target->image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region,
                       VK_FILTER_NEAREST);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_TRANSFER_BIT,
                 VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, release_layouts);
    cmd_barriers(vk, presenter.commands, VK_PIPELINE_STAGE_TRANSFER_BIT,
                 VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, release_transfers);
    vk->vkEndCommandBuffer(presenter.commands);

    VkSubmitInfo submit = {};
    submit.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO;
    submit.commandBufferCount = 1;
    submit.pCommandBuffers = &presenter.commands;
    VkResult submitted;
    {
        android::base::AutoLock lock(*emu->getQueueLock());
        submitted = vk->vkQueueSubmit(emu->getQueue(), 1, &submit, presenter.fence);
    }
    if (submitted != VK_SUCCESS) {
        if (submitted == VK_ERROR_DEVICE_LOST) {
            /* The next frame finds a new device. */
            emu->noteDeviceLost();
        }
        log_once("submitting the copy of a frame into a task window failed");
        return true;
    }
    const VkResult waited =
        vk->vkWaitForFences(device, 1, &presenter.fence, VK_TRUE, kCopyTimeoutNs);
    if (waited == VK_ERROR_DEVICE_LOST) {
        emu->noteDeviceLost();
        log_once("the GPU was reset while a frame was copied into a task window");
        return true;
    }
    if (waited != VK_SUCCESS) {
        log_once("copying a frame into a task window took longer than a second");
        vk->vkWaitForFences(device, 1, &presenter.fence, VK_TRUE, UINT64_MAX);
    }
    vk->vkResetFences(device, 1, &presenter.fence);
    presenter.iosurface_frame_cb(presenter.context, display_id, content_width, content_height);
    return true;
}

#endif /* __APPLE__ */

void present_pixels(PixelPresenter& presenter, uint32_t display_id, const PendingFrame& frame) {
    FrameBuffer* fb = FrameBuffer::getFB();
    if (!fb || !presenter.pixels_cb || frame.width == 0 || frame.height == 0 ||
        frame.width > kMaxSide || frame.height > kMaxSide) {
        return;
    }
    const uint64_t size = uint64_t(frame.width) * frame.height * 4;
    presenter.buffer.resize(size);
    fb->readColorBuffer(frame.color_buffer, 0, 0, int(frame.width), int(frame.height), kGlRgba,
                        kGlUnsignedByte, presenter.buffer.data(), size);
    presenter.pixels_cb(presenter.context, display_id, frame.width, frame.height,
                        presenter.buffer.data());
}

/* The renderer made a new device after a GPU reset: what was made on the old one went with
 * it, so it is forgotten, not destroyed, and made again as frames come. */
void follow_device(VkEmulation& emu, PixelPresenter& presenter) {
    const uint64_t generation = emu.deviceGeneration();
    if (presenter.generation == generation) {
        return;
    }
    presenter.generation = generation;
    presenter.displays.clear();
#ifdef __APPLE__
    /* The surfaces stay the embedder's; only this side's hold on them goes. */
    for (auto& [display_id, imports] : presenter.imports) {
        for (SurfaceImport& import : imports) {
            if (import.surface) {
                CFRelease(import.surface);
            }
        }
    }
    presenter.imports.clear();
#endif
    presenter.dmabuf_failed = false;
    presenter.iosurface_failed = false;
    presenter.pool = VK_NULL_HANDLE;
    presenter.commands = VK_NULL_HANDLE;
    presenter.fence = VK_NULL_HANDLE;
    log_once("the renderer made a new Vulkan device after a GPU reset; task windows follow it");
}

void present(PixelPresenter& presenter, uint32_t display_id, const PendingFrame& frame) {
    if (VkEmulation* emu = emulation()) {
        /* A lost device is made again before the copy, and none is made while it is used. */
        if (!emu->recoverIfLost()) {
            return;
        }
        auto device_use = emu->useDevice();
        follow_device(*emu, presenter);
#ifdef __APPLE__
        if (presenter.iosurface_cb && presenter.iosurface_frame_cb && !presenter.iosurface_failed) {
            if (present_iosurface(presenter, display_id, frame)) {
                return;
            }
            presenter.iosurface_failed = true;
        }
#endif
        if (presenter.dmabuf_cb && presenter.dmabuf_frame_cb && !presenter.dmabuf_failed) {
            if (present_dmabuf(presenter, display_id, frame)) {
                return;
            }
            presenter.dmabuf_failed = true;
        }
    }
    /* Read back without the device lock: the read makes a lost device again itself. */
    present_pixels(presenter, display_id, frame);
}

void run(PixelPresenter* presenter) {
    for (;;) {
        std::map<uint32_t, PendingFrame> frames;
        std::vector<uint32_t> forget;
        {
            std::unique_lock<std::mutex> lock(presenter->mutex);
            presenter->wake.wait(lock, [presenter] {
                return presenter->stopping || !presenter->pending.empty() ||
                       !presenter->forget.empty();
            });
            if (presenter->stopping) {
                break;
            }
            frames.swap(presenter->pending);
            forget.swap(presenter->forget);
        }
#ifdef __APPLE__
        if (VkEmulation* emu = emulation()) {
            auto device_use = emu->useDevice();
            follow_device(*emu, *presenter);
            for (uint32_t display_id : forget) {
                forget_display(*presenter, display_id);
            }
        }
#endif
        for (const auto& [display_id, frame] : frames) {
            present(*presenter, display_id, frame);
        }
    }
    VkEmulation* emu = emulation();
    if (!emu) {
        return;
    }
    auto device_use = emu->useDevice();
    follow_device(*emu, *presenter);
#ifdef __APPLE__
    for (auto& [display_id, imports] : presenter->imports) {
        for (SurfaceImport& import : imports) {
            release_import(*emu, import);
        }
    }
    presenter->imports.clear();
#endif
    for (auto& [display_id, display] : presenter->displays) {
        for (Slot& slot : display.slots) {
            destroy_slot(*emu, slot);
        }
    }
    VulkanDispatch* vk = emu->getDeviceDispatch();
    if (presenter->fence != VK_NULL_HANDLE) {
        vk->vkDestroyFence(emu->getDevice(), presenter->fence, nullptr);
    }
    if (presenter->pool != VK_NULL_HANDLE) {
        vk->vkDestroyCommandPool(emu->getDevice(), presenter->pool, nullptr);
    }
}

void stop_presenter() {
    PixelPresenter* presenter = nullptr;
    {
        std::lock_guard<std::mutex> lock(s_presenter_mutex);
        presenter = s_presenter;
        s_presenter = nullptr;
    }
    if (!presenter) {
        return;
    }
    {
        std::lock_guard<std::mutex> lock(presenter->mutex);
        presenter->stopping = true;
        presenter->wake.notify_one();
    }
    if (presenter->thread.joinable()) {
        presenter->thread.join();
    }
    delete presenter;
}

/* The ways a presentation can hand frames over; the first that works for a frame is used. */
struct Handover {
    dsa_display_pixels_fn pixels = nullptr;
    dsa_display_dmabuf_fn dmabuf = nullptr;
    dsa_display_dmabuf_frame_fn dmabuf_frame = nullptr;
    dsa_display_iosurface_fn iosurface = nullptr;
    dsa_display_iosurface_frame_fn iosurface_frame = nullptr;
};

int start_presenter(const Handover& handover, void* context) {
    stop_presenter();
    if (!handover.pixels && !(handover.dmabuf && handover.dmabuf_frame) &&
        !(handover.iosurface && handover.iosurface_frame)) {
        return 0;
    }
    if (!FrameBuffer::getFB()) {
        return -1;
    }
    if (!install_hook()) {
        return -1;
    }
    auto* presenter = new PixelPresenter();
    presenter->pixels_cb = handover.pixels;
    presenter->dmabuf_cb = handover.dmabuf;
    presenter->dmabuf_frame_cb = handover.dmabuf_frame;
    presenter->iosurface_cb = handover.iosurface;
    presenter->iosurface_frame_cb = handover.iosurface_frame;
    presenter->context = context;
    presenter->thread = std::thread(run, presenter);
    std::lock_guard<std::mutex> lock(s_presenter_mutex);
    s_presenter = presenter;
    return 0;
}

}  // namespace

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_display_callbacks2(dsa_display_target_fn,
                                                             dsa_display_frame_fn, void*) {
    return -1;
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_display_pixels_callback(dsa_display_pixels_fn pixels,
                                                                  void* context) {
    Handover handover;
    handover.pixels = pixels;
    return start_presenter(handover, context);
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_display_dmabuf_callbacks(
    dsa_display_dmabuf_fn dmabuf, dsa_display_dmabuf_frame_fn frame,
    dsa_display_pixels_fn fallback, void* context) {
    if (dmabuf && frame && !emulation()) {
        return -1;
    }
    Handover handover;
    handover.pixels = fallback;
    handover.dmabuf = dmabuf;
    handover.dmabuf_frame = frame;
    return start_presenter(handover, context);
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_display_iosurface_callbacks(
    dsa_display_iosurface_fn target, dsa_display_iosurface_frame_fn frame,
    dsa_display_pixels_fn fallback, void* context) {
#ifdef __APPLE__
    if (target && frame && !emulation()) {
        return -1;
    }
    Handover handover;
    handover.pixels = fallback;
    handover.iosurface = target;
    handover.iosurface_frame = frame;
    return start_presenter(handover, context);
#else
    (void)target;
    (void)frame;
    (void)fallback;
    (void)context;
    return -1;
#endif
}

extern "C" DSA_GFX_EXPORT void dsa_gfx_forget_display(uint32_t display_id) {
    std::lock_guard<std::mutex> lock(s_presenter_mutex);
    if (s_presenter) {
        std::lock_guard<std::mutex> pending_lock(s_presenter->mutex);
        s_presenter->pending.erase(display_id);
        s_presenter->forget.push_back(display_id);
        s_presenter->wake.notify_one();
    }
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_get_adapter_luid(uint8_t*) { return -1; }

void dsa_gfx_display_shutdown(void) { stop_presenter(); }

#endif /* _WIN32 */
