// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Why each Vulkan allocation on a guest's device is what it is, for the renderer's list of them.
//
// The renderer counts every allocation a device makes by wrapping the device's vkAllocateMemory
// (dsa_gfx_memory.cpp), but the wrapper sees only the call: not whether the guest asked for it,
// nor why host-visible memory was not taken from the graphics memory shared with the guest
// (dsa_graphics_memory.h). Two of those reasons were silent, so a memory report could show tens
// of megabytes of CPU-mapped memory on an app's device and nothing said whose it was or why it
// was not shared.
//
// So the decoder notes the reason on its thread just before it calls the driver, and the
// wrapper takes the note in the same call. An allocation that comes with no note was not the
// guest's vkAllocateMemory: the renderer made it for that device itself.

#pragma once

#include <cstdint>
#include <string>

namespace dsa {

enum class AllocationWhy : uint8_t {
    // No note: the renderer made it for the guest's device itself.
    RendererOwn = 0,
    // The guest asked for memory only the GPU sees.
    GpuOnly,
    // The guest imported a ColorBuffer or a buffer.
    Imported,
    // Host-visible memory taken as a grant of the shared graphics memory.
    Granted,
    // Host-visible memory not granted, for each reason in turn.
    DeviceNotShared,
    FeatureAllocates,
    DedicatedImage,
    MoreThanImportGives,
    NoHostImport,
    ImportAlignment,
    RegionRefused,
    TypeRefused,
    ImportRefused,
};

// How a reason reads in the list.
inline const char* allocationWhyText(AllocationWhy why) {
    switch (why) {
        case AllocationWhy::RendererOwn:
            return "made by the renderer for this device, not by the guest's vkAllocateMemory";
        case AllocationWhy::GpuOnly:
            return "the guest's, memory only the GPU sees";
        case AllocationWhy::Imported:
            return "the guest's import of a ColorBuffer or buffer";
        case AllocationWhy::Granted:
            return "the guest's, a grant of the shared graphics memory";
        case AllocationWhy::DeviceNotShared:
            return "not granted: the guest never said this device maps the shared graphics "
                   "memory";
        case AllocationWhy::FeatureAllocates:
            return "not granted: a gfxstream feature (system blob, external blob or host memory "
                   "import) allocates it";
        case AllocationWhy::DedicatedImage:
            return "not granted: an image has it to itself, and an image cannot be bound to "
                   "imported host memory";
        case AllocationWhy::MoreThanImportGives:
            return "not granted: it asks for more than imported host memory gives";
        case AllocationWhy::NoHostImport:
            return "not granted: the driver cannot import host memory";
        case AllocationWhy::ImportAlignment:
            return "not granted: the driver imports host memory only in pieces larger than 64 KiB";
        case AllocationWhy::RegionRefused:
            return "not granted: the shared graphics memory had no room or refused the owner";
        case AllocationWhy::TypeRefused:
            return "not granted: the driver does not take the shared memory for this memory type";
        case AllocationWhy::ImportRefused:
            return "not granted: the driver refused to import the shared memory";
    }
    return "?";
}

// The note of the calling thread, until the next allocation on it takes it.
inline AllocationWhy& pendingAllocationNote() {
    static thread_local AllocationWhy note = AllocationWhy::RendererOwn;
    return note;
}

// Notes why the next vkAllocateMemory on this thread is made, just before it is called.
inline void noteAllocation(AllocationWhy why) { pendingAllocationNote() = why; }

// Takes this thread's note, which the next allocation does not see again.
inline AllocationWhy takeAllocationNote() {
    const AllocationWhy why = pendingAllocationNote();
    pendingAllocationNote() = AllocationWhy::RendererOwn;
    return why;
}

// Vulkan's memory property bits the list names (VkMemoryPropertyFlagBits).
inline constexpr uint32_t kTypeDeviceLocal = 0x1;
inline constexpr uint32_t kTypeHostVisible = 0x2;
inline constexpr uint32_t kTypeHostCoherent = 0x4;
inline constexpr uint32_t kTypeHostCached = 0x8;

// A memory type's flags as the list reads them, such as "device-local host-visible cached".
inline std::string memoryTypeText(uint32_t flags) {
    std::string text;
    const auto add = [&text](const char* word) {
        if (!text.empty()) text += ' ';
        text += word;
    };
    if (flags & kTypeDeviceLocal) add("device-local");
    if (flags & kTypeHostVisible) add("host-visible");
    if (flags & kTypeHostCoherent) add("coherent");
    if (flags & kTypeHostCached) add("cached");
    if (text.empty()) text = "none";
    return text;
}

// What an allocation is dedicated to.
enum class Dedicated : uint8_t { Nothing, Image, Buffer };

inline const char* dedicatedText(Dedicated dedicated) {
    switch (dedicated) {
        case Dedicated::Image:
            return "image";
        case Dedicated::Buffer:
            return "buffer";
        case Dedicated::Nothing:
            break;
    }
    return "-";
}

// One allocation of a guest's device, as the list shows it.
struct AllocationRecord {
    uint64_t bytes = 0;
    uint32_t memoryType = 0;
    uint32_t typeFlags = 0;
    Dedicated dedicated = Dedicated::Nothing;
    AllocationWhy why = AllocationWhy::RendererOwn;
};

// The list's columns, tab-separated, after the device and its process: the bytes, the memory
// type's index and flags, what it is dedicated to, and why.
inline std::string allocationColumns(const AllocationRecord& record) {
    return std::to_string(record.bytes) + "\t" + std::to_string(record.memoryType) + "\t" +
           memoryTypeText(record.typeFlags) + "\t" + dedicatedText(record.dedicated) + "\t" +
           allocationWhyText(record.why);
}

}  // namespace dsa
