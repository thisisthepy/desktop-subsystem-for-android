// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// What an app's window surface costs in stencil on this GPU, for the renderer's log.
//
// Android draws an app's views with HWUI on OpenGL ES, which the guest runs on ANGLE over the
// renderer's Vulkan. HWUI asks for a window surface with 8 stencil bits and no depth, and ANGLE
// gives each window surface a depth-stencil image of its config's format. ANGLE on Android
// offers a stencil-only config (GL_STENCIL_INDEX8) when the GPU renders to VK_FORMAT_S8_UINT,
// and EGL's sort puts it before the configs with depth, so HWUI gets it: 1 byte a pixel. On a
// GPU without it, the smallest config with stencil is D24S8 (4 bytes a pixel), or D32S8 where
// D24S8 is missing too: 6.4 MiB rather than 1.6 MiB for a 1208x1383 window. Nothing in the
// guest's configuration chooses otherwise, so the GPU's formats decide, and the renderer says
// which once at its start.

#pragma once

#include <string>

namespace dsa {

// The line for a GPU that renders to S8 (`s8`), D24S8 (`d24s8`) and D32S8 (`d32s8`) as
// depth-stencil attachments with optimal tiling.
inline std::string windowStencilNote(bool s8, bool d24s8, bool d32s8) {
    if (s8) {
        return "an app's window surface gets a stencil-only buffer (VK_FORMAT_S8_UINT, 1 byte "
               "a pixel): the GPU renders to it";
    }
    if (d24s8) {
        return "an app's window surface gets a depth and stencil buffer "
               "(VK_FORMAT_D24_UNORM_S8_UINT, 4 bytes a pixel): the GPU has no stencil-only "
               "format";
    }
    if (d32s8) {
        return "an app's window surface gets a depth and stencil buffer "
               "(VK_FORMAT_D32_SFLOAT_S8_UINT, 5 bytes a pixel or more): the GPU has neither a "
               "stencil-only format nor D24S8";
    }
    return "the GPU renders to no stencil format, so apps' window surfaces have no stencil";
}

}  // namespace dsa
