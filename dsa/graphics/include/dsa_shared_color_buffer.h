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

#define DSA_FOURCC(a, b, c, d) \
    ((uint32_t)(a) | ((uint32_t)(b) << 8) | ((uint32_t)(c) << 16) | ((uint32_t)(d) << 24))

/*
 * The internal format the host ColorBuffer gets for a gralloc buffer, from the DRM format
 * minigbm writes into the gralloc handle. Whichever process creates the ColorBuffer first
 * decides its format for all of them, so every process must pick the same one, and it must
 * match the format the guest Vulkan driver makes the buffer's VkImage in: the host binds
 * that image to the ColorBuffer's memory.
 *
 * Formats with no RGB ColorBuffer equivalent (YUV) fall back to RGBA here; a host that
 * makes YUV ColorBuffers gets those instead (dsa_shared_color_buffer_create_format).
 */
static inline uint32_t dsa_shared_color_buffer_gl_format(uint32_t drm_fourcc) {
    switch (drm_fourcc) {
        case DSA_FOURCC('A', 'B', '2', '4'): /* ABGR8888: RGBA_8888 */
        case DSA_FOURCC('X', 'B', '2', '4'): /* XBGR8888: RGBX_8888 */
            return 0x1908; /* GL_RGBA */
        case DSA_FOURCC('A', 'R', '2', '4'): /* ARGB8888: BGRA_8888 */
        case DSA_FOURCC('X', 'R', '2', '4'): /* XRGB8888 */
            return 0x80E1; /* GL_BGRA_EXT */
        case DSA_FOURCC('B', 'G', '2', '4'): /* BGR888: RGB_888 */
            return 0x1907; /* GL_RGB */
        case DSA_FOURCC('R', 'G', '1', '6'): /* RGB565: RGB_565 */
            return 0x8D62; /* GL_RGB565 */
        case DSA_FOURCC('A', 'B', '4', 'H'): /* ABGR16161616F: RGBA_FP16 */
            return 0x881A; /* GL_RGBA16F */
        case DSA_FOURCC('A', 'B', '3', '0'): /* ABGR2101010: RGBA_1010102 */
            return 0x6AF9; /* GL_BGR10_A2_ANGLEX, the host's A2B10G10R10 */
        case DSA_FOURCC('R', '8', ' ', ' '): /* R8: R_8 */
            return 0x8229; /* GL_R8 */
        default:
            return 0x1908; /* GL_RGBA */
    }
}

/*
 * YUV ColorBuffers. A gralloc buffer a CPU writes (a video decoder's pictures, a camera's
 * frames) reaches the host only through an upload of its planes (dsa_cpu_upload.h), and a
 * YUV buffer needs a ColorBuffer of the same YUV layout for that: the guest Vulkan driver
 * makes the buffer's VkImage in it, and the planes are copied into it as they are. A host
 * that makes such ColorBuffers lists this extension (rcGetHostExtensionsString) and takes
 * these internal formats in rcCreateColorBufferWithHandle. Every process asks the same host,
 * so all of them pick the same format for one buffer.
 */
#define DSA_YUV_COLOR_BUFFERS_EXTENSION "ANDROID_EMU_dsa_yuv_color_buffers"
/* Y, then two chroma planes at half width and height, in the buffer's own plane order. */
#define DSA_COLOR_BUFFER_YUV420_THREE_PLANE 0x44534133u /* 'DSA3' */
/* Y, then one plane of interleaved chroma pairs at half width and height. */
#define DSA_COLOR_BUFFER_YUV420_TWO_PLANE 0x44534132u /* 'DSA2' */

/* The YUV layout of a buffer's DRM format, or 0 for a format that is not 4:2:0 8-bit YUV. */
static inline uint32_t dsa_shared_color_buffer_yuv_layout(uint32_t drm_fourcc) {
    switch (drm_fourcc) {
        case DSA_FOURCC('9', '9', '9', '7'): /* YVU420_ANDROID: YV12 */
        case DSA_FOURCC('Y', 'V', '1', '2'): /* YVU420 */
        case DSA_FOURCC('Y', 'U', '1', '2'): /* YUV420 */
            return DSA_COLOR_BUFFER_YUV420_THREE_PLANE;
        case DSA_FOURCC('N', 'V', '1', '2'): /* NV12 */
        case DSA_FOURCC('N', 'V', '2', '1'): /* NV21 */
            return DSA_COLOR_BUFFER_YUV420_TWO_PLANE;
        default:
            return 0;
    }
}

/*
 * The internal format to create a buffer's shared ColorBuffer with. `host_has_yuv` says the
 * host lists DSA_YUV_COLOR_BUFFERS_EXTENSION. A 4:2:0 image needs an even width and height,
 * so a YUV buffer of another size stays RGBA, as it is from a host without the extension.
 */
static inline uint32_t dsa_shared_color_buffer_create_format(uint32_t drm_fourcc, uint32_t width,
                                                             uint32_t height, int host_has_yuv) {
    const uint32_t yuv = dsa_shared_color_buffer_yuv_layout(drm_fourcc);
    if (yuv != 0 && host_has_yuv && width % 2 == 0 && height % 2 == 0) {
        return yuv;
    }
    return dsa_shared_color_buffer_gl_format(drm_fourcc);
}

#undef DSA_FOURCC

#endif /* DSA_SHARED_COLOR_BUFFER_H */
