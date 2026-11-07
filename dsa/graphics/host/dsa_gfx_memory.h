/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

/* The parts of the renderer's memory accounting that the rest of the backend calls. */

#ifndef DSA_GFX_MEMORY_H
#define DSA_GFX_MEMORY_H

/* Starts counting the renderer's Vulkan allocations. Called once the renderer is up. */
void dsa_gfx_memory_install(void);
/* Stops counting. Called before the renderer is torn down. */
void dsa_gfx_memory_uninstall(void);
/* A guest process connected its process pipe, or it closed. */
void dsa_gfx_memory_process_started(void);
void dsa_gfx_memory_process_ended(void);

#endif /* DSA_GFX_MEMORY_H */
