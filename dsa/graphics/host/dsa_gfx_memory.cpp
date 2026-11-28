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
 * seen, and a free of one of them is ignored.
 *
 * And the trim: a thread that, once a second, lets go of what nothing uses any more without
 * waiting for the guest's next call (the color buffers the guest closed whose close delay has
 * passed, the Vulkan objects of destroyed color buffers, whose GPU work has finished, and the
 * staging buffer once no copy used it for a while), and, a moment after a guest's device went
 * away, what that device left in the renderer's command pool and heaps. */

#include "dsa_allocation_note.h"
#include "dsa_device_gone_trim.h"
#include "dsa_gfx_backend.h"
#include "dsa_gfx_memory.h"
#include "dsa_graphics_memory.h"
#include "dsa_stencil_format.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <functional>
#include <map>
#include <mutex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "FrameBuffer.h"
#include "RenderThreadInfo.h"
#include "aemu/base/system/System.h"
#include "host-common/logging.h"
#include "vulkan/VkCommonOperations.h"
#include "vulkan/VulkanDispatch.h"

/* The embedder's log (dsa_gfx_backend.cpp). */
void dsa_gfx_log_line(char severity, const char* message);

namespace {

using gfxstream::FrameBuffer;
using gfxstream::vk::VkEmulation;
using gfxstream::vk::VulkanDispatch;

enum class Kind { DeviceLocal, HostVisible, Imported };

/* What an imported allocation is: a ColorBuffer's memory (a handle the renderer's own device
 * exported), or a grant of the graphics memory shared with the guest (a host pointer into
 * the shared region). */
enum class Import { Nothing, ColorBuffer, Grant };

struct Allocation {
    uint64_t bytes = 0;
    Kind kind = Kind::DeviceLocal;
    Import import = Import::Nothing;
    /* The host memory type, what the memory is dedicated to, and why it is what it is, for the
     * list of a guest's allocations (dsa_allocation_note.h). */
    uint32_t memory_type = 0;
    dsa::Dedicated dedicated = dsa::Dedicated::Nothing;
    dsa::AllocationWhy why = dsa::AllocationWhy::RendererOwn;
};

/* Who a guest process is, as it said on its process pipe. */
struct Identity {
    uint32_t pid = 0;
    std::string name;
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
/* The guest process each guest device serves: the process of the render thread that made its
 * first allocation. Under s_mutex. */
std::map<VkDevice, uint64_t> s_device_process;
/* The guest processes that said who they are, by the renderer's number for them. Under
 * s_mutex; one goes when its process pipe closes. */
std::map<uint64_t, Identity> s_identities;

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

Import imported(const VkMemoryAllocateInfo* info) {
    for (auto* next = static_cast<const VkBaseInStructure*>(info->pNext); next;
         next = next->pNext) {
        switch (next->sType) {
#ifdef _WIN32
            case VK_STRUCTURE_TYPE_IMPORT_MEMORY_WIN32_HANDLE_INFO_KHR:
#endif
            case VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR:
                return Import::ColorBuffer;
            case VK_STRUCTURE_TYPE_IMPORT_MEMORY_HOST_POINTER_INFO_EXT:
                return Import::Grant;
            default:
                break;
        }
    }
    return Import::Nothing;
}

/* What the allocation `info` describes is dedicated to: an image, a buffer, or nothing. */
dsa::Dedicated dedicated_to(const VkMemoryAllocateInfo* info) {
    for (auto* next = static_cast<const VkBaseInStructure*>(info->pNext); next;
         next = next->pNext) {
        if (next->sType == VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO) {
            const auto* dedicated = reinterpret_cast<const VkMemoryDedicatedAllocateInfo*>(next);
            if (dedicated->image != VK_NULL_HANDLE) {
                return dsa::Dedicated::Image;
            }
            if (dedicated->buffer != VK_NULL_HANDLE) {
                return dsa::Dedicated::Buffer;
            }
        }
    }
    return dsa::Dedicated::Nothing;
}

/* The guest process the calling render thread serves, or 0 (the renderer's own threads). */
uint64_t calling_process() {
    gfxstream::RenderThreadInfo* info = gfxstream::RenderThreadInfo::get();
    return info ? info->m_puid : 0;
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
    /* Taken whatever happens, so a note never reaches a later allocation on this thread. */
    const dsa::AllocationWhy why = dsa::takeAllocationNote();
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
    allocation.import = imported(info);
    allocation.memory_type = info->memoryTypeIndex;
    allocation.dedicated = dedicated_to(info);
    allocation.why = why;
    const uint64_t process = calling_process();
    std::lock_guard<std::mutex> lock(s_mutex);
    if (!s_counting) {
        return result;
    }
    if (process != 0) {
        s_device_process.emplace(device, process);
    }
    if (allocation.import != Import::Nothing) {
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
    s_device_process.erase(device);
}

/* Guest devices that went away, for the trim of what they left behind (dsa_device_gone_trim.h). */
std::mutex s_gone_mutex;
dsa::DeviceGoneTrim s_gone;

VKAPI_ATTR void VKAPI_CALL counted_destroy_through_decoder(VkDevice device,
                                                           const VkAllocationCallbacks* callbacks) {
    forget_device(device);
    s_decoder_destroy(device, callbacks);
    /* Only a guest's device is destroyed through the decoder. The trim thread gives back what
     * it left once no other device has gone for a moment; nothing waits for it here. */
    std::lock_guard<std::mutex> lock(s_gone_mutex);
    s_gone.deviceGone(dsa::DeviceGoneTrim::Clock::now());
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

/* The trim's thread, and how it is told to end. */
std::mutex s_trim_mutex;
std::condition_variable s_trim_wake;
bool s_trim_stop = false;
std::thread s_trim;

/* How often the trim looks. Freeing waits for nothing, so a look costs a lock and a check. */
constexpr std::chrono::seconds kTrimInterval{1};

/* How long the staging buffer stays with no copy using it. A video whose frames a guest CPU
 * writes uses it every frame and keeps it; a screenshot makes it again in a millisecond. */
constexpr std::chrono::seconds kStagingIdle{5};

/* Once guest devices went away and no other has gone for a moment: the renderer's command pool
 * gives back what its command buffers grew to, and the heaps their freed blocks. The working
 * set is left alone, since windows may be open. The log line says how much private memory it
 * gave back, which is how to tell on a computer whether the trim is worth its cost. */
void trim_after_devices_gone() {
    unsigned gone = 0;
    {
        std::lock_guard<std::mutex> lock(s_gone_mutex);
        if (!s_gone.due(dsa::DeviceGoneTrim::Clock::now())) {
            return;
        }
        gone = s_gone.takeGone();
    }
    const auto started = std::chrono::steady_clock::now();
    const uint64_t before = dsa_gfx_private_bytes();
    if (VkEmulation* emu = emulation()) {
        emu->trimCommandPool();
    }
    dsa_gfx_compact_heaps();
    const uint64_t after = dsa_gfx_private_bytes();
    const double took_ms = std::chrono::duration<double, std::milli>(
                               std::chrono::steady_clock::now() - started)
                               .count();
    char line[320];
    if (before != 0 && after != 0) {
        std::snprintf(line, sizeof(line),
                      "%u guest Vulkan device(s) went away: the renderer trimmed its command pool "
                      "and compacted its heaps in %.1f ms; private memory %.1f -> %.1f MiB "
                      "(%.1f MiB given back)",
                      gone, took_ms, before / 1048576.0, after / 1048576.0,
                      (static_cast<double>(before) - static_cast<double>(after)) / 1048576.0);
    } else {
        std::snprintf(line, sizeof(line),
                      "%u guest Vulkan device(s) went away: the renderer trimmed its command pool "
                      "and compacted its heaps in %.1f ms",
                      gone, took_ms);
    }
    dsa_gfx_log_line('N', line);
}

void trim_run() {
    std::unique_lock<std::mutex> lock(s_trim_mutex);
    while (!s_trim_wake.wait_for(lock, kTrimInterval, [] { return s_trim_stop; })) {
        lock.unlock();
        if (FrameBuffer* fb = FrameBuffer::getFB()) {
            /* The color buffers the guest closed last wait for their close delay on a list that
             * was looked at only when the next buffer closed: a video's whole queue, closed in
             * one second, stayed for as long as nothing else closed. They go here, first, so
             * their Vulkan objects are retired before the free below. */
            const int closed = fb->closeDelayedColorBuffers();
            if (closed > 0) {
                INFO("Destroyed %d color buffers the guest closed and nothing opened again.",
                     closed);
            }
        }
        if (VkEmulation* emu = emulation()) {
            /* A destroyed color buffer's image and memory are freed once the GPU is done with
             * them; without this they waited for the next color buffer to be made or
             * destroyed, so those of a window that closed last stayed. */
            emu->freeFinishedRetired();
            /* The staging buffer of CPU reads and writes of color buffers, made as large as
             * the copy that needed it, goes when no copy used it for a while. It used to be
             * 128 MiB of host-visible memory held from the start. */
            emu->releaseIdleStaging(kStagingIdle);
        }
        /* Graphics memory shared with the guest that the renderer revoked while a guest file
         * still held it: once the guest kernel says it let go, the pages are zeroed and given
         * back here, rather than when the next allocation looks (dsa_graphics_memory.h). */
        dsa::graphicsMemory().reclaim();
        /* What a guest's device that went away left behind, while windows may still be open:
         * before, the heaps were compacted only once no task window was left. */
        trim_after_devices_gone();
        lock.lock();
    }
}

void start_trim() {
    std::lock_guard<std::mutex> lock(s_trim_mutex);
    if (s_trim.joinable()) {
        return;
    }
    s_trim_stop = false;
    s_trim = std::thread(trim_run);
}

void stop_trim() {
    std::thread trim;
    {
        std::lock_guard<std::mutex> lock(s_trim_mutex);
        s_trim_stop = true;
        trim.swap(s_trim);
    }
    s_trim_wake.notify_all();
    if (trim.joinable()) {
        trim.join();
    }
}

/* Says once which depth-stencil buffer an app's window surface gets on this GPU
 * (dsa_stencil_format.h): the guest's ANGLE takes a stencil-only one where the GPU renders to
 * VK_FORMAT_S8_UINT, which the guest sees as the renderer's device reports it. */
void say_window_stencil(VulkanDispatch* ivk, VkPhysicalDevice physical) {
    if (!ivk || !ivk->vkGetPhysicalDeviceFormatProperties || physical == VK_NULL_HANDLE) {
        return;
    }
    const auto renders = [&](VkFormat format) {
        VkFormatProperties properties = {};
        ivk->vkGetPhysicalDeviceFormatProperties(physical, format, &properties);
        return (properties.optimalTilingFeatures &
                VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT) != 0;
    };
    const std::string note = dsa::windowStencilNote(renders(VK_FORMAT_S8_UINT),
                                                    renders(VK_FORMAT_D24_UNORM_S8_UINT),
                                                    renders(VK_FORMAT_D32_SFLOAT_S8_UINT));
    dsa_gfx_log_line('N', note.c_str());
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
    say_window_stencil(ivk, emu->getPhysicalDevice());
    wrap_destroy(gfxstream::vk::vkDispatch(), counted_destroy_through_decoder, &s_decoder_destroy);
    wrap_destroy(ivk, counted_destroy_through_instance, &s_instance_destroy);
    {
        std::lock_guard<std::mutex> lock(s_mutex);
        s_counting = true;
    }
    /* The renderer's own device was made before the hook was set. */
    wrap_device_table(emu->getDevice(), emu->getDeviceDispatch());
    gfxstream::vk::setDeviceDispatchHook(wrap_device_table);
    start_trim();
}

void dsa_gfx_memory_uninstall(void) {
    /* Before the renderer is torn down, which the trim must not reach into. */
    stop_trim();
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

extern "C" DSA_GFX_EXPORT int dsa_gfx_get_memory_owners(dsa_gfx_memory_owners* out) {
    if (!out) {
        return -1;
    }
    *out = dsa_gfx_memory_owners{};
    /* The renderer's own device now: after a GPU reset, the one made again. */
    VkEmulation* emu = emulation();
    const VkDevice own = emu ? emu->getDevice() : VK_NULL_HANDLE;
    std::lock_guard<std::mutex> lock(s_mutex);
    if (!s_counting) {
        return -1;
    }
    std::map<VkDevice, uint64_t> guest_devices;
    for (const auto& [key, allocation] : s_live) {
        const int kind = allocation.kind == Kind::DeviceLocal   ? DSA_GFX_MEMORY_DEVICE_LOCAL
                         : allocation.kind == Kind::HostVisible ? DSA_GFX_MEMORY_HOST_VISIBLE
                                                                : DSA_GFX_MEMORY_IMPORTED;
        if (key.first == own) {
            out->renderer_bytes[kind] += allocation.bytes;
            out->renderer_count[kind]++;
        } else {
            out->guest_bytes[kind] += allocation.bytes;
            out->guest_count[kind]++;
            guest_devices[key.first] += allocation.bytes;
        }
    }
    out->guest_devices = static_cast<uint32_t>(guest_devices.size());
    std::vector<uint64_t> held;
    held.reserve(guest_devices.size());
    for (const auto& [device, bytes] : guest_devices) {
        held.push_back(bytes);
    }
    std::sort(held.begin(), held.end(), std::greater<uint64_t>());
    for (size_t i = 0; i < held.size() && i < DSA_GFX_MEMORY_LARGEST_DEVICES; i++) {
        out->largest_guest_device_bytes[i] = held[i];
    }
    return 0;
}

void dsa_gfx_memory_process_identified(uint64_t puid, uint32_t pid, const char* name) {
    Identity identity;
    identity.pid = pid;
    identity.name = name ? name : "";
    std::lock_guard<std::mutex> lock(s_mutex);
    s_identities[puid] = std::move(identity);
}

void dsa_gfx_memory_process_forget(uint64_t puid) {
    std::lock_guard<std::mutex> lock(s_mutex);
    s_identities.erase(puid);
}

namespace {

void copy_name(char* out, size_t size, const std::string& name) {
    const size_t n = std::min(size - 1, name.size());
    std::memcpy(out, name.data(), n);
    out[n] = '\0';
}

/* "name (pid N)", "process N" for one that never said who it is, or "the renderer" for 0.
 * Under s_mutex. */
std::string process_label_locked(uint64_t puid) {
    if (puid == 0) {
        return "the renderer";
    }
    const auto found = s_identities.find(puid);
    if (found == s_identities.end()) {
        return "process " + std::to_string(puid) + " (gone or unnamed)";
    }
    const std::string name = found->second.name.empty() ? "?" : found->second.name;
    return name + " (pid " + std::to_string(found->second.pid) + ")";
}

}  // namespace

extern "C" DSA_GFX_EXPORT int dsa_gfx_get_guest_devices(dsa_gfx_guest_device* out,
                                                        uint32_t capacity,
                                                        uint32_t* total) {
    if (total) {
        *total = 0;
    }
    VkEmulation* emu = emulation();
    const VkDevice own = emu ? emu->getDevice() : VK_NULL_HANDLE;
    std::lock_guard<std::mutex> lock(s_mutex);
    if (!s_counting) {
        return -1;
    }
    std::map<VkDevice, dsa_gfx_guest_device> devices;
    for (const auto& [key, allocation] : s_live) {
        if (key.first == own) {
            continue;
        }
        dsa_gfx_guest_device& d = devices[key.first];
        const int kind = allocation.kind == Kind::DeviceLocal   ? DSA_GFX_MEMORY_DEVICE_LOCAL
                         : allocation.kind == Kind::HostVisible ? DSA_GFX_MEMORY_HOST_VISIBLE
                                                                : DSA_GFX_MEMORY_IMPORTED;
        d.bytes[kind] += allocation.bytes;
        d.count[kind]++;
        if (allocation.import == Import::ColorBuffer) {
            d.color_buffer_import_bytes += allocation.bytes;
            d.color_buffer_imports++;
        } else if (allocation.import == Import::Grant) {
            d.grant_bytes += allocation.bytes;
            d.grants++;
        }
    }
    std::vector<dsa_gfx_guest_device> list;
    list.reserve(devices.size());
    for (auto& [device, d] : devices) {
        const auto process = s_device_process.find(device);
        if (process != s_device_process.end()) {
            d.process = process->second;
            const auto identity = s_identities.find(process->second);
            if (identity != s_identities.end()) {
                d.pid = identity->second.pid;
                copy_name(d.name, sizeof(d.name), identity->second.name);
            }
        }
        list.push_back(d);
    }
    const auto held = [](const dsa_gfx_guest_device& d) {
        return d.bytes[0] + d.bytes[1] + d.bytes[2];
    };
    std::sort(list.begin(), list.end(),
              [&](const auto& a, const auto& b) { return held(a) > held(b); });
    if (total) {
        *total = static_cast<uint32_t>(list.size());
    }
    for (uint32_t i = 0; out && i < capacity && i < list.size(); i++) {
        out[i] = list[i];
    }
    return 0;
}

extern "C" DSA_GFX_EXPORT size_t dsa_gfx_describe_color_buffers(char* out, size_t capacity) {
    FrameBuffer* fb = FrameBuffer::getFB();
    if (!fb) {
        return 0;
    }
    auto buffers = fb->describeColorBuffers();
    std::sort(buffers.begin(), buffers.end(), [](const auto& a, const auto& b) {
        return a.allocationBytes > b.allocationBytes;
    });
    const uint64_t now = android::base::getUnixTimeUs();
    const auto ago = [now](uint64_t us) -> std::string {
        if (us == 0 || us > now) {
            return "-";
        }
        char text[32];
        std::snprintf(text, sizeof(text), "%.1f", (now - us) / 1e6);
        return text;
    };
    uint64_t total = 0;
    for (const auto& b : buffers) {
        total += b.allocationBytes;
    }
    std::string text;
    char line[512];
    std::snprintf(line, sizeof(line),
                  "# %zu ColorBuffers, %.1f MiB of Vulkan memory. Ages and times are seconds "
                  "before this list was made; \"-\" is never.\n",
                  buffers.size(), total / 1048576.0);
    text += line;
    text +=
        "handle\twidth\theight\tformat\tframework_format\tbytes\trefcount\topened\t"
        "closed_s_ago\tage_s\tlast_post_s_ago\trole\tcreator\tholders\n";
    {
        std::lock_guard<std::mutex> lock(s_mutex);
        for (const auto& b : buffers) {
            const std::string creator = b.createdUs ? process_label_locked(b.creatorPuid) : "?";
            std::string role;
            if (b.displayId >= 0) {
                role = "task frame (display " + std::to_string(b.displayId) + ")";
            } else if (creator.find("surfaceflinger") != std::string::npos) {
                role = "client target";
            } else if (creator.find("composer") != std::string::npos) {
                role = "hwcomposer target";
            } else {
                role = "buffer";
            }
            /* Each holder once, with how many times it opened the buffer. */
            std::map<uint64_t, int> holders;
            for (uint64_t puid : b.holders) {
                holders[puid]++;
            }
            std::string held;
            for (const auto& [puid, times] : holders) {
                if (!held.empty()) {
                    held += ", ";
                }
                held += process_label_locked(puid);
                if (times > 1) {
                    held += " x" + std::to_string(times);
                }
            }
            if (held.empty()) {
                held = "none";
            }
            std::snprintf(line, sizeof(line), "%u\t%u\t%u\t0x%x\t%u\t%llu\t%u\t%s\t%s\t%s\t%s\t",
                          b.handle, b.width, b.height, b.format, b.frameworkFormat,
                          static_cast<unsigned long long>(b.allocationBytes), b.refcount,
                          b.opened ? "yes" : "no", ago(b.closedUs).c_str(),
                          ago(b.createdUs).c_str(), ago(b.lastPostUs).c_str());
            text += line;
            text += role + "\t" + creator + "\t" + held + "\n";
        }
    }
    if (out && capacity > 0) {
        const size_t n = std::min(capacity - 1, text.size());
        std::memcpy(out, text.data(), n);
        out[n] = '\0';
    }
    return text.size() + 1;
}

extern "C" DSA_GFX_EXPORT size_t dsa_gfx_describe_guest_allocations(char* out, size_t capacity) {
    VkEmulation* emu = emulation();
    const VkDevice own = emu ? emu->getDevice() : VK_NULL_HANDLE;
    struct Line {
        VkDevice device;
        uint64_t device_bytes;
        Allocation allocation;
    };
    std::vector<Line> lines;
    std::string text;
    {
        std::lock_guard<std::mutex> lock(s_mutex);
        if (!s_counting) {
            return 0;
        }
        std::map<VkDevice, uint64_t> held;
        for (const auto& [key, allocation] : s_live) {
            if (key.first != own) {
                held[key.first] += allocation.bytes;
            }
        }
        uint64_t total = 0;
        for (const auto& [key, allocation] : s_live) {
            if (key.first == own) {
                continue;
            }
            lines.push_back({key.first, held[key.first], allocation});
            total += allocation.bytes;
        }
        /* The devices that hold the most first, each device's largest first. */
        std::sort(lines.begin(), lines.end(), [](const Line& a, const Line& b) {
            if (a.device_bytes != b.device_bytes) return a.device_bytes > b.device_bytes;
            if (a.device != b.device) return a.device < b.device;
            return a.allocation.bytes > b.allocation.bytes;
        });
        char line[160];
        std::snprintf(line, sizeof(line),
                      "# %zu allocations on %zu guest devices, %.1f MiB. Each line is one "
                      "vkAllocateMemory on a guest process's device.\n",
                      lines.size(), held.size(), total / 1048576.0);
        text += line;
        text += "device\tprocess\tkind\tbytes\tmemory_type\ttype_flags\tdedicated\twhy\n";
        for (const Line& entry : lines) {
            const auto process = s_device_process.find(entry.device);
            const std::string label = process != s_device_process.end()
                                          ? process_label_locked(process->second)
                                          : std::string("?");
            const char* kind = entry.allocation.kind == Kind::DeviceLocal   ? "GPU-only"
                               : entry.allocation.kind == Kind::HostVisible ? "CPU-mapped"
                               : entry.allocation.import == Import::Grant   ? "imported grant"
                                                                            : "imported";
            dsa::AllocationRecord record;
            record.bytes = entry.allocation.bytes;
            record.memoryType = entry.allocation.memory_type;
            record.typeFlags = entry.allocation.memory_type < s_types.memoryTypeCount
                                   ? s_types.memoryTypes[entry.allocation.memory_type].propertyFlags
                                   : 0;
            record.dedicated = entry.allocation.dedicated;
            record.why = entry.allocation.why;
            std::snprintf(line, sizeof(line), "%p\t", static_cast<void*>(entry.device));
            text += line;
            text += label + "\t" + kind + "\t" + dsa::allocationColumns(record) + "\n";
        }
    }
    if (out && capacity > 0) {
        const size_t n = std::min(capacity - 1, text.size());
        std::memcpy(out, text.data(), n);
        out[n] = '\0';
    }
    return text.size() + 1;
}
