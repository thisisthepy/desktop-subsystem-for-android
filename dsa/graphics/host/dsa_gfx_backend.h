/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */

#ifndef DSA_GFX_BACKEND_H
#define DSA_GFX_BACKEND_H

#include <stddef.h>
#include <stdint.h>

#ifdef _WIN32
#define DSA_GFX_EXPORT __declspec(dllexport)
#else
#define DSA_GFX_EXPORT __attribute__((visibility("default")))
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef int (*dsa_stream_read_fn)(void* user_data, void* buf, size_t count);
typedef int (*dsa_stream_write_fn)(void* user_data, const void* buf, size_t count);

typedef void (*dsa_post_callback_fn)(void* context,
                                     uint32_t display_id,
                                     int width,
                                     int height,
                                     int ydir,
                                     int format,
                                     int type,
                                     unsigned char* pixels);

typedef void (*dsa_color_buffer_post_fn)(void* context, uint32_t color_buffer_id);

/* Initializes the host gfxstream renderer (Vulkan only, surfaceless). */
DSA_GFX_EXPORT int dsa_gfx_init(uint32_t width, uint32_t height);

/* Shuts down the host gfxstream renderer. */
DSA_GFX_EXPORT void dsa_gfx_teardown(void);

/* Serves an accepted command stream (port 5566) by creating a render channel
 * and pumping bytes between the stream callbacks and the channel.
 * Returns 0 when the stream terminates cleanly.
 */
DSA_GFX_EXPORT int dsa_gfx_serve_stream(dsa_stream_read_fn read_cb,
                                       dsa_stream_write_fn write_cb,
                                       void* user_data);

/* Serves a process pipe connection (port 5567) by performing the puid
 * handshake (expecting confirm code 100, replying with a unique 64-bit puid)
 * and waiting for client disconnect before destroying process objects.
 */
DSA_GFX_EXPORT int dsa_gfx_process_pipe(dsa_stream_read_fn read_cb,
                                       dsa_stream_write_fn write_cb,
                                       void* user_data);

/* Generates a unique 64-bit puid and notifies the renderer of process creation. */
DSA_GFX_EXPORT uint64_t dsa_gfx_on_process_create(void);

/* Cleans up per-process graphics objects for the given puid. */
DSA_GFX_EXPORT void dsa_gfx_on_process_destroy(uint64_t puid);

/* Registers a framebuffer post callback called when a frame is ready. */
DSA_GFX_EXPORT int dsa_gfx_set_post_callback(dsa_post_callback_fn cb, void* user_data);

/* Registers a color buffer post callback called when a color buffer is posted. */
DSA_GFX_EXPORT int dsa_gfx_set_color_buffer_post_callback(dsa_color_buffer_post_fn cb, void* user_data);

/* Low-level render channel operations */
DSA_GFX_EXPORT void* dsa_gfx_channel_create(void);
DSA_GFX_EXPORT void dsa_gfx_channel_destroy(void* channel);
DSA_GFX_EXPORT int dsa_gfx_channel_write(void* channel, const void* buf, size_t size);
DSA_GFX_EXPORT int dsa_gfx_channel_read(void* channel, void* buf, size_t size, size_t* out_read, uint32_t timeout_ms);
DSA_GFX_EXPORT void dsa_gfx_channel_stop(void* channel);

#ifdef __cplusplus
}
#endif

#endif /* DSA_GFX_BACKEND_H */
