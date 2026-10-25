/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

/* The parts of the per-display presentation that the rest of the backend calls. */

#ifndef DSA_GFX_DISPLAY_H
#define DSA_GFX_DISPLAY_H

/* Stops the presentation thread and frees what it imported. Called before the renderer is
 * torn down. */
void dsa_gfx_display_shutdown(void);

#endif /* DSA_GFX_DISPLAY_H */
