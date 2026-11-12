// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The memory types the guest Vulkan driver reports without memory it shares with the host: each
// type the host caches is reported as the host reports it (not coherent, read back when the app
// invalidates) and as a coherent twin after the host's types (read back whenever the app learns
// its work finished).

#include <cstdint>
#include <cstdio>
#include <cstdlib>

#include "dsa_memory_types.h"

namespace {

using dsa::kMaxMemoryTypes;
using dsa::kMemoryHostCached;
using dsa::kMemoryHostCoherent;
using dsa::kMemoryHostVisible;
using dsa::MemoryTypeSplit;
using dsa::readBackAtCompletion;
using dsa::readBackAtFirstMap;

constexpr uint32_t kDeviceLocal = 0x1;
constexpr uint32_t kHV = kMemoryHostVisible;
constexpr uint32_t kHC = kMemoryHostCached;
constexpr uint32_t kCoherent = kMemoryHostCoherent;

// VkMemoryType's two members.
struct MemoryType {
    uint32_t propertyFlags;
    uint32_t heapIndex;
};

int failures = 0;

void check(bool ok, const char* test, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "%s: %s\n", test, what);
        failures++;
    }
}

// The order Vulkan asks of a device's types: a type comes before every type whose flags are a
// strict superset of its own.
bool inVulkanOrder(const MemoryType* types, uint32_t count) {
    for (uint32_t x = 0; x < count; x++) {
        for (uint32_t y = 0; y < x; y++) {
            const uint32_t fx = types[x].propertyFlags;
            const uint32_t fy = types[y].propertyFlags;
            if (fx != fy && (fx & fy) == fx) return false;  // x is a strict subset of y
        }
    }
    return true;
}

// The first type in `bits` that has every flag in `required`, as an app that takes the first
// type that fits (ANGLE's staging, Skia's VMA with no preference) finds it.
int firstFit(const MemoryType* types, uint32_t count, uint32_t bits, uint32_t required) {
    for (uint32_t i = 0; i < count; i++) {
        if ((bits & (1u << i)) && (types[i].propertyFlags & required) == required) {
            return static_cast<int>(i);
        }
    }
    return -1;
}

// An integrated GPU after the host took coherence off: device local only, and one cached
// host-visible type. Lavapipe on CI reports only the second.
void integrated(MemoryType* types, uint32_t* count) {
    types[0] = {kDeviceLocal, 0};
    types[1] = {kDeviceLocal | kHV | kHC, 0};
    *count = 2;
}

// A discrete GPU after the host took coherence off: device local, uncached and cached system
// memory, and resizable BAR.
void discrete(MemoryType* types, uint32_t* count) {
    types[0] = {kDeviceLocal, 0};
    types[1] = {kHV, 1};
    types[2] = {kHV | kHC, 1};
    types[3] = {kDeviceLocal | kHV, 0};
    *count = 4;
}

void pr3_a_cached_type_is_listed_non_coherent_first_and_coherent_after_the_hosts_types() {
    const char* test = __func__;
    MemoryType types[kMaxMemoryTypes] = {};
    uint32_t count = 0;
    integrated(types, &count);
    MemoryTypeSplit split;
    split.split(types, &count);
    check(count == 3, test, "one twin for the one cached type");
    check(split.hostTypeCount() == 2 && split.typeCount() == 3, test, "the counts are kept");
    check(types[0].propertyFlags == kDeviceLocal, test, "a type the host does not cache is as it was");
    check(types[1].propertyFlags == (kDeviceLocal | kHV | kHC), test,
          "the cached type keeps its index and stays non-coherent");
    check(types[2].propertyFlags == (kDeviceLocal | kHV | kHC | kCoherent), test,
          "its twin is coherent");
    check(types[2].heapIndex == types[1].heapIndex, test, "the twin is in its source's heap");
    check(!split.isTwin(1) && split.isTwin(2), test, "only the added type is a twin");
    check(inVulkanOrder(types, count), test, "the list is in the order Vulkan asks");
    check(firstFit(types, count, 0x7, kHV | kCoherent) == 2, test,
          "Vulkan's host-visible coherent type is there");
}

void pr3_the_first_type_that_fits_a_host_visible_request_is_the_non_coherent_one() {
    const char* test = __func__;
    MemoryType types[kMaxMemoryTypes] = {};
    uint32_t count = 0;
    discrete(types, &count);
    MemoryTypeSplit split;
    split.split(types, &count);
    check(count == 5, test, "one twin, for the one cached type");
    check(types[4].propertyFlags == (kHV | kHC | kCoherent) && types[4].heapIndex == 1, test,
          "the twin is the cached type, coherent, in its heap");
    check(types[1].propertyFlags == kHV && types[3].propertyFlags == (kDeviceLocal | kHV), test,
          "an uncached type gets no twin and is as the host reported it");
    check(inVulkanOrder(types, count), test, "the list is in the order Vulkan asks");
    const uint32_t all = split.guestBits(0xF);
    check(firstFit(types, count, all, kHV | kHC) == 2, test,
          "cached staging takes the non-coherent type");
    check(firstFit(types, count, all, kHV | kCoherent) == 4, test,
          "an app that needs coherence still finds it");
}

void pr3_twins_keep_their_sources_order_when_one_cached_type_is_a_subset_of_another() {
    const char* test = __func__;
    MemoryType types[kMaxMemoryTypes] = {};
    types[0] = {kDeviceLocal, 0};
    types[1] = {kHV | kHC, 1};
    types[2] = {kDeviceLocal | kHV | kHC, 0};
    uint32_t count = 3;
    MemoryTypeSplit split;
    split.split(types, &count);
    check(count == 5, test, "a twin for each cached type");
    check(split.hostIndex(3) == 1 && split.hostIndex(4) == 2, test, "twins in their sources' order");
    check(inVulkanOrder(types, count), test, "the list is in the order Vulkan asks");
}

void pr3_a_twin_index_goes_to_the_host_as_its_sources() {
    const char* test = __func__;
    MemoryType types[kMaxMemoryTypes] = {};
    uint32_t count = 0;
    discrete(types, &count);
    MemoryTypeSplit split;
    split.split(types, &count);
    for (uint32_t i = 0; i < 4; i++) {
        check(split.hostIndex(i) == i, test, "a host type keeps its index");
    }
    check(split.hostIndex(4) == 2, test, "the twin allocates from its source");
    check(split.hostIndex(7) == 7, test, "an index that names no type goes as it is");
}

void pr3_every_mask_that_holds_a_cached_type_holds_its_twin() {
    const char* test = __func__;
    MemoryType types[kMaxMemoryTypes] = {};
    uint32_t count = 0;
    discrete(types, &count);
    MemoryTypeSplit split;
    split.split(types, &count);
    // A buffer's memoryTypeBits from the host.
    check(split.guestBits(0x6) == (0x6u | 0x10u), test, "the cached type brings its twin");
    check(split.guestBits(0x9) == 0x9u, test, "a mask without it is as the host sent it");
    check(split.guestBits(0) == 0, test, "an empty mask stays empty");
    // The other way, as a mask goes back to the host.
    check(split.hostBits(0x10) == 0x4u, test, "the twin is its source to the host");
    check(split.hostBits(0x1F) == 0xFu, test, "every guest type is a host type to the host");
    check(split.hostBits(split.guestBits(0xF)) == 0xFu, test, "a mask goes there and back");
}

void pr3_without_room_for_every_twin_each_cached_type_is_reported_coherent_itself() {
    const char* test = __func__;
    MemoryType types[kMaxMemoryTypes] = {};
    uint32_t count = kMaxMemoryTypes - 1;
    for (uint32_t i = 0; i < count; i++) types[i] = {kDeviceLocal, 0};
    types[count - 2] = {kHV | kHC, 1};
    types[count - 1] = {kDeviceLocal | kHV | kHC, 0};
    MemoryTypeSplit split;
    split.split(types, &count);
    check(count == kMaxMemoryTypes - 1, test, "no twin when not every cached type has room");
    check(split.typeCount() == count && !split.isTwin(count - 1), test, "nothing was added");
    check(types[count - 2].propertyFlags == (kHV | kHC | kCoherent), test,
          "a cached type is reported coherent where it is");
    check(types[count - 1].propertyFlags == (kDeviceLocal | kHV | kHC | kCoherent), test,
          "and so is the other");
    check(inVulkanOrder(types, count), test, "the list is in the order Vulkan asks");
    check(readBackAtCompletion(types[count - 1].propertyFlags), test,
          "and it is read back as a coherent type is");
    check(split.hostBits(0x7FFFFFFFu) == 0x7FFFFFFFu && split.guestBits(0x40000000u) == 0x40000000u,
          test, "masks and indices are the host's");
    check(split.hostIndex(count - 1) == count - 1, test, "an index is the host's");
}

void pr3_a_list_the_host_already_made_coherent_is_left_as_it_is() {
    const char* test = __func__;
    // A host that shares memory with the guest keeps coherence; the guest makes no twins.
    MemoryType types[kMaxMemoryTypes] = {};
    types[0] = {kDeviceLocal, 0};
    types[1] = {kHV | kCoherent, 1};
    types[2] = {kHV | kHC | kCoherent, 1};
    uint32_t count = 3;
    MemoryTypeSplit split;
    split.split(types, &count);
    check(count == 3 && split.typeCount() == 3, test, "no twin of a coherent type");
    check(split.guestBits(0x7) == 0x7u && split.hostBits(0x7) == 0x7u, test, "masks unchanged");
}

void pr3_with_no_split_masks_and_indices_are_the_hosts() {
    const char* test = __func__;
    // Memory the host shares: the guest never splits, and every transform leaves things be.
    MemoryTypeSplit split;
    check(split.hostBits(0xFFFFFFFFu) == 0xFFFFFFFFu && split.hostBits(0x5) == 0x5u, test,
          "a mask goes to the host as it is");
    check(split.guestBits(0x5) == 0x5u, test, "a mask comes from the host as it is");
    check(split.hostIndex(3) == 3 && !split.isTwin(3), test, "an index goes as it is");
}

void pr3_the_same_host_list_gives_the_same_guest_list_every_time() {
    const char* test = __func__;
    MemoryTypeSplit split;
    MemoryType first[kMaxMemoryTypes] = {};
    uint32_t firstCount = 0;
    discrete(first, &firstCount);
    split.split(first, &firstCount);
    MemoryType second[kMaxMemoryTypes] = {};
    uint32_t secondCount = 0;
    discrete(second, &secondCount);
    split.split(second, &secondCount);
    bool same = firstCount == secondCount;
    for (uint32_t i = 0; same && i < firstCount; i++) {
        same = first[i].propertyFlags == second[i].propertyFlags &&
               first[i].heapIndex == second[i].heapIndex;
    }
    check(same, test, "an app that asks again sees the same indices");
    check(split.hostIndex(4) == 2, test, "and they map as before");
}

void fr4_a_non_coherent_type_is_read_back_only_when_the_app_invalidates() {
    const char* test = __func__;
    // The guest reads the device's writes back at completion only for coherent types: the
    // non-coherent one comes back through the app's own vkInvalidateMappedMemoryRanges, which
    // always reads back.
    check(!readBackAtCompletion(kDeviceLocal | kHV | kHC), test,
          "a cached non-coherent type is not read back when the app's work finishes");
    check(!readBackAtCompletion(kHV), test, "an uncached type is not read back either");
    check(!readBackAtCompletion(kDeviceLocal), test, "memory the app cannot map is not read back");
    check(readBackAtFirstMap(kDeviceLocal | kHV | kHC), test,
          "its copy is read back once, at its first map, to start in step");
    check(!readBackAtFirstMap(kHV), test, "an uncached type starts as the app writes it");
}

void pr3_a_coherent_type_is_read_back_whenever_the_app_learns_its_work_finished() {
    const char* test = __func__;
    MemoryType types[kMaxMemoryTypes] = {};
    uint32_t count = 0;
    discrete(types, &count);
    MemoryTypeSplit split;
    split.split(types, &count);
    int atCompletion = 0;
    for (uint32_t i = 0; i < count; i++) {
        if (readBackAtCompletion(types[i].propertyFlags)) {
            atCompletion++;
            check(split.isTwin(i), test, "only the coherent twin is read back at completion");
            check(readBackAtFirstMap(types[i].propertyFlags), test,
                  "and at its first map, as its source");
        }
    }
    check(atCompletion == 1, test, "the twin is read back at completion");
}

}  // namespace

int main() {
    pr3_a_cached_type_is_listed_non_coherent_first_and_coherent_after_the_hosts_types();
    pr3_the_first_type_that_fits_a_host_visible_request_is_the_non_coherent_one();
    pr3_twins_keep_their_sources_order_when_one_cached_type_is_a_subset_of_another();
    pr3_a_twin_index_goes_to_the_host_as_its_sources();
    pr3_every_mask_that_holds_a_cached_type_holds_its_twin();
    pr3_without_room_for_every_twin_each_cached_type_is_reported_coherent_itself();
    pr3_a_list_the_host_already_made_coherent_is_left_as_it_is();
    pr3_with_no_split_masks_and_indices_are_the_hosts();
    pr3_the_same_host_list_gives_the_same_guest_list_every_time();
    fr4_a_non_coherent_type_is_read_back_only_when_the_app_invalidates();
    pr3_a_coherent_type_is_read_back_whenever_the_app_learns_its_work_finished();
    if (failures != 0) {
        std::fprintf(stderr, "%d failed\n", failures);
        return EXIT_FAILURE;
    }
    return EXIT_SUCCESS;
}
