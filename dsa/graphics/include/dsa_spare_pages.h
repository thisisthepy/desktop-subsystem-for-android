// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Handing back the pages of a reused buffer that the next use does not need yet.
//
// A buffer that is reused for every transfer grows to the largest transfer it ever carried and
// keeps that size: the guest's stream to the host renderer for each thread, and the staging
// copy that takes what a CPU wrote into a gralloc buffer up to the host (the hwcomposer's and
// the guest Vulkan driver's). Once a video, a large texture or a big command batch has gone
// through, its pages stay resident in the process for as long as it runs, and with them in the
// VM, although nothing reads them again before they are written.
//
// After each transfer that went past a small part kept warm, the whole pages beyond it are
// marked free with MADV_FREE. The buffer keeps its size and its addresses. A page written again
// before the kernel takes it costs nothing more than the write; a page the kernel took comes
// back as a zero page on its next write. The kernel takes them when it reclaims page cache, as
// the guest's working set trim does, so they go back to the host without a page fault on every
// transfer while a video plays. The contents of the pages marked are lost, which is why it is
// only for buffers whose contents are dead once they have been sent.

#pragma once

#include <cstddef>
#include <cstdint>

#if defined(__linux__)
#include <sys/mman.h>
#include <unistd.h>
#endif

namespace dsa {

// The part of a buffer that stays warm: the commands of an ordinary frame fit in it, so the
// common transfer never touches a page handed back.
inline constexpr size_t kSpareKeepBytes = 256 * 1024;

// The whole pages of a buffer of `used` bytes at `address`, beyond its first `keep` bytes,
// with pages of `page` bytes: where they start and how many bytes they cover. Nothing (a
// length of 0) when the transfer did not go past `keep`, or covered no whole page past it.
struct SpareRange {
    uintptr_t start = 0;
    size_t length = 0;
};

inline SpareRange spareRange(uintptr_t address, size_t used, size_t keep, size_t page) {
    SpareRange range;
    if (page == 0 || used <= keep) {
        return range;
    }
    const uintptr_t from = address + keep;
    const uintptr_t to = address + used;
    const uintptr_t first = (from + page - 1) / page * page;
    const uintptr_t last = to / page * page;
    if (last > first) {
        range.start = first;
        range.length = static_cast<size_t>(last - first);
    }
    return range;
}

// Marks the spare pages of a buffer whose first `used` bytes were just sent free (see above).
// Returns the bytes marked; 0 where the platform has no MADV_FREE or refuses it.
inline size_t releaseSparePages(void* buffer, size_t used, size_t keep = kSpareKeepBytes) {
#if defined(__linux__) && defined(MADV_FREE)
    if (!buffer) {
        return 0;
    }
    const long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) {
        return 0;
    }
    const SpareRange range =
        spareRange(reinterpret_cast<uintptr_t>(buffer), used, keep, static_cast<size_t>(page));
    if (range.length == 0) {
        return 0;
    }
    if (madvise(reinterpret_cast<void*>(range.start), range.length, MADV_FREE) != 0) {
        return 0;
    }
    return range.length;
#else
    (void)buffer;
    (void)used;
    (void)keep;
    return 0;
#endif
}

}  // namespace dsa
