/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 *
 * The host ColorBuffer handle that every guest process uses for one gralloc buffer.
 *
 * Under Hyper-V the guest has no virtio-gpu device, so a gralloc buffer cannot carry a
 * host resource the way it does on Cuttlefish. Instead each process that uses the buffer
 * (the app that draws into it, SurfaceFlinger, the hwcomposer) derives the same handle
 * from the buffer ID that minigbm's allocator writes into the gralloc handle, and opens
 * the ColorBuffer with that handle, creating it first if no process has yet.
 *
 * The allocator numbers buffers from 1 upwards and never reuses a number while it runs.
 * The top bit keeps these handles apart from the ones the host renderer hands out itself,
 * which count up from 1.
 */

#ifndef DSA_SHARED_COLOR_BUFFER_H
#define DSA_SHARED_COLOR_BUFFER_H

#include <stdint.h>

#define DSA_SHARED_COLOR_BUFFER_BIT 0x80000000u

static inline uint32_t dsa_shared_color_buffer_handle(uint32_t buffer_id) {
    return DSA_SHARED_COLOR_BUFFER_BIT | (buffer_id & ~DSA_SHARED_COLOR_BUFFER_BIT);
}

#endif /* DSA_SHARED_COLOR_BUFFER_H */
