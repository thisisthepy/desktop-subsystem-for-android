// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The memory types the guest Vulkan driver reports when it shares no memory with the host, and
// how their indices map to the host's.
//
// A Hyper-V VM has no memory the guest and the host share, so the host takes HOST_COHERENT off
// every memory type, and the guest driver keeps its own copy of host-visible memory: what the
// app wrote goes to the host at every submit, and what the device wrote comes back when the
// guest reads it back. Vulkan requires a type that is both host visible and coherent, and most
// apps ask for one for their uniform buffers. Reporting the host's cached type coherent met
// that, but a coherent type's user never invalidates, so the guest had to read the device's
// writes back at every point an app learns its work finished. An app's renderer learns that at
// every frame, and each frame waited for the read back.
//
// So each type the host caches is reported twice:
//
// - as the host reports it, host visible and cached but not coherent. Its index is the host's,
//   and it comes before its twin, so a driver that takes the first type that fits (ANGLE's
//   upload staging, VMA with no preference for coherence) takes it. The device's writes come
//   back only when the app invalidates, as Vulkan asks of a non-coherent type.
// - as its coherent twin, after every type the host reports. The device's writes come back at
//   every point the app learns its work finished, as coherence promises.
//
// The host's types keep their indices, so an index or a type mask the host hands out unchanged
// (a color buffer's memory type, an external memory's properties) still means the same type to
// the guest. A twin's index maps to its source's when it goes to the host, and a host mask that
// holds a source holds its twin too.
//
// The order is what Vulkan asks: a type comes before every type whose flags are a strict
// superset of its own. No host type is coherent, so no host type holds a twin's flags; the
// twins are in their sources' order, so they keep the sources' order among themselves.
//
// When the list has no room for every twin (a host that reports close to 32 types), no twin is
// made and every cached type is reported coherent itself, as before: Vulkan's coherent type
// comes first, the cheaper read back second. Adding coherence to every cached type keeps the
// order too, which twins for only some of them would not.

#pragma once

#include <cstdint>

namespace dsa {

// Vulkan's memory property bits this file looks at (VkMemoryPropertyFlagBits).
inline constexpr uint32_t kMemoryHostVisible = 0x2;
inline constexpr uint32_t kMemoryHostCoherent = 0x4;
inline constexpr uint32_t kMemoryHostCached = 0x8;
// VK_MAX_MEMORY_TYPES.
inline constexpr uint32_t kMaxMemoryTypes = 32;

// Whether the guest reads the device's writes back into its copy of a type's memory each time
// the app learns its work finished (a fence, a semaphore, a wait for the queue or the device):
// only for a type it reports coherent. The others come back when the app invalidates them.
inline bool readBackAtCompletion(uint32_t guestFlags) {
    return (guestFlags & kMemoryHostVisible) && (guestFlags & kMemoryHostCoherent);
}

// Whether the guest reads an allocation back the first time the app maps it: for a type the
// host caches, so the copy, which was not kept in step before, holds what the device wrote.
inline bool readBackAtFirstMap(uint32_t guestFlags) {
    return (guestFlags & kMemoryHostVisible) && (guestFlags & kMemoryHostCached);
}

class MemoryTypeSplit {
   public:
    // Turns the host's memory types (`types`, `*count` of them, coherence already taken off by
    // the host) into the guest's, in place, and keeps how they map. `MemoryType` has
    // `propertyFlags` and `heapIndex`, as VkMemoryType does. The host's list must be the one
    // the guest saw before, if there was one: the guest's indices must not change under it.
    template <typename MemoryType>
    void split(MemoryType* types, uint32_t* count) {
        const uint32_t hostCount = *count > kMaxMemoryTypes ? kMaxMemoryTypes : *count;
        hostCount_ = hostCount;
        twinCount_ = 0;
        uint32_t cached = 0;
        for (uint32_t host = 0; host < hostCount; host++) {
            if (needsCoherence(types[host].propertyFlags)) cached++;
        }
        const bool room = hostCount + cached <= kMaxMemoryTypes;
        for (uint32_t host = 0; host < hostCount; host++) {
            const uint32_t flags = types[host].propertyFlags;
            if (!needsCoherence(flags)) continue;
            if (!room) {
                types[host].propertyFlags = flags | kMemoryHostCoherent;
                continue;
            }
            const uint32_t twin = hostCount + twinCount_;
            types[twin] = types[host];
            types[twin].propertyFlags = flags | kMemoryHostCoherent;
            twinSource_[twinCount_++] = host;
        }
        *count = hostCount + twinCount_;
    }

    // How many types the host reported, which keep their indices in the guest's list.
    uint32_t hostTypeCount() const { return hostCount_; }

    // How many types the guest reports.
    uint32_t typeCount() const { return hostCount_ + twinCount_; }

    // Whether a guest index is a coherent twin the guest added.
    bool isTwin(uint32_t guestIndex) const {
        return guestIndex >= hostCount_ && guestIndex < hostCount_ + twinCount_;
    }

    // The host's index for a guest's. One that names no guest type goes as it is, for the host
    // to refuse.
    uint32_t hostIndex(uint32_t guestIndex) const {
        return isTwin(guestIndex) ? twinSource_[guestIndex - hostCount_] : guestIndex;
    }

    // A host type mask as the guest's: each host type the mask holds, and the twin of each.
    uint32_t guestBits(uint32_t hostBits) const {
        uint32_t bits = hostBits;
        for (uint32_t twin = 0; twin < twinCount_; twin++) {
            if (hostBits & (1u << twinSource_[twin])) bits |= 1u << (hostCount_ + twin);
        }
        return bits;
    }

    // A guest type mask as the host's: each twin it holds becomes its source. Without twins
    // (no split yet, or memory the host shares) the mask is the host's as it is.
    uint32_t hostBits(uint32_t guestBits) const {
        if (twinCount_ == 0) return guestBits;
        uint32_t bits = guestBits & ((1u << hostCount_) - 1);
        for (uint32_t twin = 0; twin < twinCount_; twin++) {
            if (guestBits & (1u << (hostCount_ + twin))) bits |= 1u << twinSource_[twin];
        }
        return bits;
    }

   private:
    // A type the guest keeps in step both ways that the host does not report coherent.
    static bool needsCoherence(uint32_t flags) {
        return (flags & kMemoryHostVisible) && (flags & kMemoryHostCached) &&
               !(flags & kMemoryHostCoherent);
    }

    uint32_t hostCount_ = 0;
    uint32_t twinCount_ = 0;
    uint32_t twinSource_[kMaxMemoryTypes] = {};
};

}  // namespace dsa
