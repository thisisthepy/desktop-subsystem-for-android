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
        log_once("submitting the copy of a frame into a task window failed");
        return;
    }
    VkResult waited = vk->vkWaitForFences(device, 1, &presenter.fence, VK_TRUE, kCopyTimeoutNs);
    if (waited != VK_SUCCESS) {
        /* The fence stays pending; wait for it before the next submit reuses the buffer. */
        log_once("copying a frame into a task window took longer than a second");
        vk->vkWaitForFences(device, 1, &presenter.fence, VK_TRUE, UINT64_MAX);
        vk->vkResetFences(device, 1, &presenter.fence);
        return;
    }
    vk->vkResetFences(device, 1, &presenter.fence);
    presenter.frame_cb(presenter.context, display_id, content_width, content_height);
}

void run(Presenter* presenter) {
    for (;;) {
        std::map<uint32_t, PendingFrame> frames;
        {
            std::unique_lock<std::mutex> lock(presenter->mutex);
            presenter->wake.wait(lock, [presenter] {
                return presenter->stopping || !presenter->pending.empty();
            });
            if (presenter->stopping) {
                break;
            }
            frames.swap(presenter->pending);
        }
        for (const auto& [display_id, frame] : frames) {
            present(*presenter, display_id, frame);
        }
    }
    VkEmulation* emu = emulation();
    if (!emu) {
        return;
    }
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

void dsa_gfx_display_shutdown(void) { stop_presenter(); }

#else /* !_WIN32 */

extern "C" DSA_GFX_EXPORT int dsa_gfx_set_display_callbacks2(dsa_display_target_fn,
                                                             dsa_display_frame_fn, void*) {
    return -1;
}

extern "C" DSA_GFX_EXPORT int dsa_gfx_get_adapter_luid(uint8_t*) { return -1; }

void dsa_gfx_display_shutdown(void) {}

#endif /* _WIN32 */
