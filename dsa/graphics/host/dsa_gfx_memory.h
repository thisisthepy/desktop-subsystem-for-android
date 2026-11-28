/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

/* The parts of the renderer's memory accounting that the rest of the backend calls. */

#ifndef DSA_GFX_MEMORY_H
#define DSA_GFX_MEMORY_H

#include <stdint.h>

/* Starts counting the renderer's Vulkan allocations, and the trim that frees what nothing uses.
 * Called once the renderer is up. */
void dsa_gfx_memory_install(void);
/* Stops the trim and the count. Called before the renderer is torn down. */
void dsa_gfx_memory_uninstall(void);
/* A guest process connected its process pipe, or it closed. */
void dsa_gfx_memory_process_started(void);
void dsa_gfx_memory_process_ended(void);
/* A guest process said who it is on its process pipe: its pid and name, which the memory
 * report shows for its device and its ColorBuffers. Forgotten when its pipe closes. */
void dsa_gfx_memory_process_identified(uint64_t puid, uint32_t pid, const char* name);
void dsa_gfx_memory_process_forget(uint64_t puid);

/* The process's heaps (dsa_gfx_heap_trim.cpp): gives the free memory they keep committed back
 * to the system, as far as each allocator can without moving anything. */
void dsa_gfx_compact_heaps(void);
/* The process's private committed bytes, or 0 where the system does not say. */
uint64_t dsa_gfx_private_bytes(void);

#endif /* DSA_GFX_MEMORY_H */
