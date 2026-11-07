/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

/* What the renderer holds in memory, by owner, so the window manager can say where its
 * committed memory goes; and the trim that lets go of what nothing uses.
 *
 * Every Vulkan allocation the renderer makes goes through its device dispatch table:
 * the guest's own allocations (decoded from its streams), the color buffers behind every
 * gralloc buffer, and the textures of task windows imported from Direct3D. Once the
 * renderer is up, the allocate and free entries of that table are wrapped, so each live
 * allocation is counted by kind: memory only the GPU sees, memory the host's CPU maps
 * (which the guest's copies of host-visible memory are written into), and memory imported
 * from another API. A few allocations made while the renderer started are not seen, and a
 * free of one of them is ignored. */

#include "dsa_gfx_backend.h"
#include "dsa_gfx_memory.h"

#include <atomic>
#include <map>
#include <mutex>

#include "FrameBuffer.h"
#include "vulkan/VkCommonOperations.h"
#include "vulkan/VulkanDispatch.h"

namespace {

using gfxstream::FrameBuffer;
using gfxstream::vk::VkEmulation;
using gfxstream::vk::VulkanDispatch;

enum class Kind { DeviceLocal, HostVisible, Imported };

struct Allocation {
    uint64_t bytes = 0;
    Kind kind = Kind::DeviceLocal;
};

std::mutex s_mutex;
std::map<VkDeviceMemory, Allocation> s_live;
VkPhysicalDeviceMemoryProperties s_types = {};
VulkanDispatch* s_dispatch = nullptr;
PFN_vkAllocateMemory s_allocate = nullptr;
PFN_vkFreeMemory s_free = nullptr;
std::atomic<int32_t> s_processes{0};

VkEmulation* emulation() {
    FrameBuffer* fb = FrameBuffer::getFB();
    if (!fb || !fb->hasEmulationVk()) {
        return nullptr;
    }
    return &fb->getEmulationVk();
}

bool imported(const VkMemoryAllocateInfo* info) {
    for (auto* next = static_cast<const VkBaseInStructure*>(info->pNext); next;
         next = next->pNext) {
        switch (next->sType) {
#ifdef _WIN32
            case VK_STRUCTURE_TYPE_IMPORT_MEMORY_WIN32_HANDLE_INFO_KHR:
#endif
            case VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR:
            case VK_STRUCTURE_TYPE_IMPORT_MEMORY_HOST_POINTER_INFO_EXT:
                return true;
            default:
                break;
        }
    }
    return false;
}

VKAPI_ATTR VkResult VKAPI_CALL counted_allocate(VkDevice device,
                                                const VkMemoryAllocateInfo* info,
                                                const VkAllocationCallbacks* callbacks,
                                                VkDeviceMemory* memory) {
    VkResult result = s_allocate(device, info, callbacks, memory);
    if (result != VK_SUCCESS || !info || !memory) {
        return result;
    }
    Allocation allocation;
    allocation.bytes = info->allocationSize;
    if (imported(info)) {
        allocation.kind = Kind::Imported;
    } else if (info->memoryTypeIndex < s_types.memoryTypeCount &&
               (s_types.memoryTypes[info->memoryTypeIndex].propertyFlags &
                VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT)) {
        allocation.kind = Kind::HostVisible;
    }
    std::lock_guard<std::mutex> lock(s_mutex);
    s_live[*memory] = allocation;
    return result;
}

VKAPI_ATTR void VKAPI_CALL counted_free(VkDevice device,
                                        VkDeviceMemory memory,
                                        const VkAllocationCallbacks* callbacks) {
    {
        std::lock_guard<std::mutex> lock(s_mutex);
        s_live.erase(memory);
    }
    s_free(device, memory, callbacks);
}

}  // namespace

void dsa_gfx_memory_install(void) {
    VkEmulation* emu = emulation();
    if (!emu) {
        return;
    }
    VulkanDispatch* vk = emu->getDeviceDispatch();
    if (!vk || !vk->vkAllocateMemory || !vk->vkFreeMemory || vk->vkAllocateMemory == counted_allocate) {
        return;
    }
    VulkanDispatch* ivk = emu->getInstanceDispatch();
    if (ivk && ivk->vkGetPhysicalDeviceMemoryProperties) {
        ivk->vkGetPhysicalDeviceMemoryProperties(emu->getPhysicalDevice(), &s_types);
    }
    s_dispatch = vk;
    s_allocate = vk->vkAllocateMemory;
    s_free = vk->vkFreeMemory;
    vk->vkAllocateMemory = counted_allocate;
    vk->vkFreeMemory = counted_free;
}

void dsa_gfx_memory_uninstall(void) {
    std::lock_guard<std::mutex> lock(s_mutex);
    if (s_dispatch && s_dispatch->vkAllocateMemory == counted_allocate) {
        s_dispatch->vkAllocateMemory = s_allocate;
        s_dispatch->vkFreeMemory = s_free;
    }
    s_dispatch = nullptr;
    s_live.clear();
}

void dsa_gfx_memory_process_started(void) { s_processes.fetch_add(1, std::memory_order_relaxed); }

void dsa_gfx_memory_process_ended(void) { s_processes.fetch_sub(1, std::memory_order_relaxed); }

extern "C" DSA_GFX_EXPORT int dsa_gfx_get_memory_stats(dsa_gfx_memory_stats* out) {
    if (!out) {
        return -1;
    }
    *out = dsa_gfx_memory_stats{};
    out->guest_processes = s_processes.load(std::memory_order_relaxed);
    std::lock_guard<std::mutex> lock(s_mutex);
    if (!s_dispatch) {
        return -1;
    }
    for (const auto& [memory, allocation] : s_live) {
        switch (allocation.kind) {
            case Kind::DeviceLocal:
                out->device_local_bytes += allocation.bytes;
                out->device_local_count++;
                break;
            case Kind::HostVisible:
                out->host_visible_bytes += allocation.bytes;
                out->host_visible_count++;
                break;
            case Kind::Imported:
                out->imported_bytes += allocation.bytes;
                out->imported_count++;
                break;
        }
        if (allocation.bytes > out->largest_bytes) {
            out->largest_bytes = allocation.bytes;
        }
    }
    return 0;
}
