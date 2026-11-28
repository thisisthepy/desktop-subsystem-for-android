/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

/* Giving the free memory of the renderer's heaps back to the system, and reading how much
 * private memory the renderer has committed, on each host system. Kept apart from the memory
 * accounting so the system headers stay out of the files that include gfxstream's. */

#include "dsa_gfx_memory.h"

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <psapi.h>

#include <vector>
#elif defined(__APPLE__)
#include <malloc/malloc.h>
#elif defined(__linux__)
#include <malloc.h>
#endif

void dsa_gfx_compact_heaps(void) {
#ifdef _WIN32
    /* Every heap of the process: the default one, the C runtime's, and those the GPU driver
     * made. HeapCompact decommits the free blocks at their segments' ends. */
    const DWORD count = GetProcessHeaps(0, nullptr);
    if (count == 0) {
        return;
    }
    /* Room for heaps made between the two calls. */
    std::vector<HANDLE> heaps(count + 8);
    const DWORD found = GetProcessHeaps(static_cast<DWORD>(heaps.size()), heaps.data());
    if (found == 0 || found > heaps.size()) {
        return;
    }
    for (DWORD i = 0; i < found; i++) {
        HeapCompact(heaps[i], 0);
    }
#elif defined(__APPLE__)
    malloc_zone_pressure_relief(nullptr, 0);
#elif defined(__linux__) && defined(__GLIBC__)
    malloc_trim(0);
#endif
}

uint64_t dsa_gfx_private_bytes(void) {
#ifdef _WIN32
    PROCESS_MEMORY_COUNTERS_EX counters = {};
    counters.cb = sizeof(counters);
    if (!K32GetProcessMemoryInfo(GetCurrentProcess(),
                                 reinterpret_cast<PROCESS_MEMORY_COUNTERS*>(&counters),
                                 sizeof(counters))) {
        return 0;
    }
    return counters.PrivateUsage;
#else
    return 0;
#endif
}
