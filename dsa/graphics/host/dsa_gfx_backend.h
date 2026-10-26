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

/* Which GPU the renderer runs on.
 *
 * The embedder may ask for a GPU before dsa_gfx_init. Only a GPU with a graphics queue can
 * match. When none matches, or nothing is asked, gfxstream ranks the GPUs itself: a discrete
 * GPU first, then an integrated one, then the highest Vulkan version. */
#define DSA_GFX_GPU_RANKING 0    /* gfxstream's own ranking */
#define DSA_GFX_GPU_LUID 1       /* the GPU whose VkPhysicalDeviceIDProperties::deviceLUID is `luid` */
#define DSA_GFX_GPU_INTEGRATED 2 /* the first integrated GPU */
#define DSA_GFX_GPU_DISCRETE 3   /* the first discrete GPU */
#define DSA_GFX_GPU_NAME 4       /* the first GPU whose name contains `name`, ignoring case */

/* Asks for the GPU `rule` describes. `luid` (8 bytes, as a Windows LUID) is read only for
 * DSA_GFX_GPU_LUID and `name` (UTF-8) only for DSA_GFX_GPU_NAME. Returns 0 on success, -1
 * when the rule is unknown, its argument is missing, or the renderer is already running. */
DSA_GFX_EXPORT int dsa_gfx_select_gpu(uint32_t rule, const uint8_t* luid, const char* name);

/* The GPU the renderer runs on, and whether it is the one asked for. */
typedef struct dsa_gfx_gpu_info {
    char name[256];          /* VkPhysicalDeviceProperties::deviceName, UTF-8 */
    uint32_t device_type;    /* VkPhysicalDeviceType */
    uint8_t luid[8];         /* valid when luid_valid is 1 */
    int32_t luid_valid;
    int32_t preference_matched; /* 1: the GPU asked for; 0: none matched, so ranked; -1: none asked */
} dsa_gfx_gpu_info;

/* Fills `info`. Returns 0 on success, -1 when the renderer is not initialised. */
DSA_GFX_EXPORT int dsa_gfx_get_gpu_info(dsa_gfx_gpu_info* info);

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

/* Per-display presentation: how each Android task's frames reach its window.
 *
 * The guest's hwcomposer gives each task a display ID and sets the task's finished frame as
 * that display's color buffer (rcSetDisplayColorBuffer). For each such frame the backend
 * asks the embedder where to put it, copies it there on the GPU, and says when it is there.
 * Nothing is read back to the CPU.
 *
 * The hwcomposer keeps each task's color buffers at the largest size the task can have and
 * composes each frame into their top-left region, so that resizing the task's window does
 * not remake them. It sets the display's size (rcSetDisplayPose) to the frame's size before
 * it sets the frame, and only that region is copied and shown.
 *
 * The frame must be complete when it is set: the backend copies it on the renderer's queue
 * right away. A composition the host runs for the guest (rcCompose with a display ID) sets
 * the display's color buffer before it composes, so its result shows one frame late.
 */

/* Called on the backend's presentation thread when `display_id` has a new frame in a color
 * buffer of `width` x `height` pixels. Returns a Windows NT handle to a Direct3D 11 texture
 * of exactly that size, in DXGI_FORMAT_R8G8B8A8_UNORM, made with D3D11_RESOURCE_MISC_SHARED_NTHANDLE on
 * the GPU dsa_gfx_get_adapter_luid names, to copy the frame into; or NULL to drop the frame.
 * The handle and its texture stay the embedder's, and stay valid until the next call for the
 * same display returns something else. */
typedef void* (*dsa_display_target_fn)(void* context,
                                       uint32_t display_id,
                                       uint32_t width,
                                       uint32_t height);

/* Called on the same thread once the frame is in the texture, filling its top-left
 * `width` x `height` pixels, and the GPU has finished copying it. The texture is not written
 * again for this display until this returns. */
typedef void (*dsa_display_frame_fn)(void* context,
                                     uint32_t display_id,
                                     uint32_t width,
                                     uint32_t height);

/* Registers the presentation callbacks; NULLs stop the presentation. Returns 0 on success,
 * -1 when the renderer is not initialised or cannot present on this host. The name carries
 * a 2 because the frame callback gained the frame's size: an embedder built for the older
 * callback does not find this one, rather than calling it with the wrong arguments. */
DSA_GFX_EXPORT int dsa_gfx_set_display_callbacks2(dsa_display_target_fn target,
                                                  dsa_display_frame_fn frame,
                                                  void* context);

/* Writes the LUID of the GPU the renderer runs on (8 bytes, as a Windows LUID). Returns 0 on
 * success, -1 when the renderer is not initialised or the driver does not report one. */
DSA_GFX_EXPORT int dsa_gfx_get_adapter_luid(uint8_t luid[8]);

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
