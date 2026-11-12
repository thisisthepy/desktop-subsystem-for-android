/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

/* What the renderer holds in memory, by owner, so the window manager can say where its
 * committed memory goes.
 *
 * Every Vulkan allocation the renderer makes goes through a device dispatch table: the
 * renderer's own device holds the color buffers behind every gralloc buffer and the textures
 * of task windows imported from Direct3D, and each guest process's device holds what that
 * guest allocates (decoded from its streams). Once the renderer is up, gfxstream hands every
 * dispatch table it fills to the hook here (the renderer's own, the one it makes again after
 * a lost device, and each guest device's), and the allocate and free entries of each are
 * wrapped, so each live allocation is counted by kind: memory only the GPU sees, memory the
 * host's CPU maps (which the guest's copies of host-visible memory are written into), and
 * memory imported from another API. A destroyed device takes its allocations with it.
 *
 * Without the hook, the table of a device made again after a GPU reset was filled anew
 * with the driver's entries, and nothing was counted from then on; a guest device's
 * allocations were never counted. A few allocations made while the renderer started are not
 * seen, and a free of one of them is ignored. */

#include "dsa_gfx_backend.h"
#include "dsa_gfx_memory.h"

#include <atomic>
#include <map>
#include <mutex>
#include <utility>

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

/* The driver's entries of one device's table, which the wrapped ones call. */
struct DeviceEntries {
    PFN_vkAllocateMemory allocate = nullptr;
    PFN_vkFreeMemory free = nullptr;
};

std::mutex s_mutex;
/* Live allocations by device and memory: handles of different devices may be equal. */
std::map<std::pair<VkDevice, VkDeviceMemory>, Allocation> s_live;
/* Kept after counting stops: a wrapped table may still be called while the renderer is torn
 * down, and the call must reach the driver. */
std::map<VkDevice, DeviceEntries> s_devices;
/* The first device's entries, for a call naming a device the hook never saw. One driver
 * serves every device the renderer makes. */
DeviceEntries s_fallback;
VkPhysicalDeviceMemoryProperties s_types = {};
bool s_counting = false;
std::atomic<int32_t> s_processes{0};

/* The driver's vkDestroyDevice behind the two tables a device is destroyed through: the
 * decoder's (a guest's device) and the renderer's instance table (its own device). */
PFN_vkDestroyDevice s_decoder_destroy = nullptr;
PFN_vkDestroyDevice s_instance_destroy = nullptr;

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

DeviceEntries entries_of(VkDevice device) {
    std::lock_guard<std::mutex> lock(s_mutex);
    const auto found = s_devices.find(device);
    return found != s_devices.end() ? found->second : s_fallback;
}

VKAPI_ATTR VkResult VKAPI_CALL counted_allocate(VkDevice device,
                                                const VkMemoryAllocateInfo* info,
                                                const VkAllocationCallbacks* callbacks,
                                                VkDeviceMemory* memory) {
    const DeviceEntries entries = entries_of(device);
    if (!entries.allocate) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    VkResult result = entries.allocate(device, info, callbacks, memory);
    if (result != VK_SUCCESS || !info || !memory) {
        return result;
    }
    Allocation allocation;
    allocation.bytes = info->allocationSize;
    std::lock_guard<std::mutex> lock(s_mutex);
    if (!s_counting) {
        return result;
    }
    if (imported(info)) {
        allocation.kind = Kind::Imported;
    } else if (info->memoryTypeIndex < s_types.memoryTypeCount &&
               (s_types.memoryTypes[info->memoryTypeIndex].propertyFlags &
                VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT)) {
        allocation.kind = Kind::HostVisible;
    }
    s_live[{device, *memory}] = allocation;
    return result;
}

VKAPI_ATTR void VKAPI_CALL counted_free(VkDevice device,
                                        VkDeviceMemory memory,
                                        const VkAllocationCallbacks* callbacks) {
    DeviceEntries entries;
    {
        std::lock_guard<std::mutex> lock(s_mutex);
        s_live.erase({device, memory});
        const auto found = s_devices.find(device);
        entries = found != s_devices.end() ? found->second : s_fallback;
    }
    if (entries.free) {
        entries.free(device, memory, callbacks);
    }
}

/* A device is destroyed: what it still held goes with it. */
void forget_device(VkDevice device) {
    std::lock_guard<std::mutex> lock(s_mutex);
    for (auto it = s_live.lower_bound({device, VkDeviceMemory{}});
         it != s_live.end() && it->first.first == device;) {
        it = s_live.erase(it);
    }
    s_devices.erase(device);
}

VKAPI_ATTR void VKAPI_CALL counted_destroy_through_decoder(VkDevice device,
                                                           const VkAllocationCallbacks* callbacks) {
    forget_device(device);
    s_decoder_destroy(device, callbacks);
}

VKAPI_ATTR void VKAPI_CALL counted_destroy_through_instance(VkDevice device,
                                                            const VkAllocationCallbacks* callbacks) {
    forget_device(device);
    s_instance_destroy(device, callbacks);
}

/* gfxstream filled `dispatch` from `device`: its allocate and free entries are wrapped. */
void wrap_device_table(VkDevice device, VulkanDispatch* dispatch) {
    if (!dispatch || !dispatch->vkAllocateMemory || !dispatch->vkFreeMemory) {
        return;
    }
    if (dispatch->vkAllocateMemory == counted_allocate) {
        return;
    }
    {
        std::lock_guard<std::mutex> lock(s_mutex);
        DeviceEntries entries{dispatch->vkAllocateMemory, dispatch->vkFreeMemory};
        s_devices[device] = entries;
        if (!s_fallback.allocate) {
            s_fallback = entries;
        }
    }
    dispatch->vkAllocateMemory = counted_allocate;
    dispatch->vkFreeMemory = counted_free;
}

void wrap_destroy(VulkanDispatch* table, PFN_vkDestroyDevice wrapper,
                  PFN_vkDestroyDevice* original) {
    if (!table || !table->vkDestroyDevice || table->vkDestroyDevice == wrapper) {
        return;
    }
    *original = table->vkDestroyDevice;
    table->vkDestroyDevice = wrapper;
}

}  // namespace

void dsa_gfx_memory_install(void) {
    VkEmulation* emu = emulation();
    if (!emu) {
        return;
    }
    VulkanDispatch* ivk = emu->getInstanceDispatch();
    if (ivk && ivk->vkGetPhysicalDeviceMemoryProperties) {
        ivk->vkGetPhysicalDeviceMemoryProperties(emu->getPhysicalDevice(), &s_types);
    }
    wrap_destroy(gfxstream::vk::vkDispatch(), counted_destroy_through_decoder, &s_decoder_destroy);
    wrap_destroy(ivk, counted_destroy_through_instance, &s_instance_destroy);
    {
        std::lock_guard<std::mutex> lock(s_mutex);
        s_counting = true;
    }
    /* The renderer's own device was made before the hook was set. */
    wrap_device_table(emu->getDevice(), emu->getDeviceDispatch());
    gfxstream::vk::setDeviceDispatchHook(wrap_device_table);
}

void dsa_gfx_memory_uninstall(void) {
    /* The wrapped entries stay: they forward to the driver, and the teardown still calls
     * them. Only the count stops. */
    std::lock_guard<std::mutex> lock(s_mutex);
    s_counting = false;
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
    if (!s_counting) {
        return -1;
    }
    for (const auto& [key, allocation] : s_live) {
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
