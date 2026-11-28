// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// What an app's window surface costs in stencil on a GPU (dsa_stencil_format.h), run on the
// build machine.

#include "dsa_stencil_format.h"

#include <cstdio>
#include <string>

static int failures = 0;

#define EXPECT(cond)                                                                \
    do {                                                                            \
        if (!(cond)) {                                                              \
            std::fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond); \
            ++failures;                                                             \
        }                                                                           \
    } while (0)

static bool contains(const std::string& text, const char* part) {
    return text.find(part) != std::string::npos;
}

// nfr1: a GPU that renders to S8 gives window surfaces a stencil-only buffer, whatever else
// it has.
static void nfr1_a_gpu_with_s8_gives_window_surfaces_stencil_only() {
    EXPECT(contains(dsa::windowStencilNote(true, true, true), "VK_FORMAT_S8_UINT, 1 byte"));
    EXPECT(contains(dsa::windowStencilNote(true, false, false), "stencil-only"));
}

// nfr1: without S8 the line says which larger format the surface gets instead.
static void nfr1_without_s8_the_larger_format_is_named() {
    EXPECT(contains(dsa::windowStencilNote(false, true, true), "D24_UNORM_S8_UINT, 4 bytes"));
    EXPECT(contains(dsa::windowStencilNote(false, false, true), "D32_SFLOAT_S8_UINT"));
    EXPECT(contains(dsa::windowStencilNote(false, false, false), "no stencil format"));
}

int main() {
    nfr1_a_gpu_with_s8_gives_window_surfaces_stencil_only();
    nfr1_without_s8_the_larger_format_is_named();
    if (failures) {
        std::fprintf(stderr, "%d failures\n", failures);
        return 1;
    }
    std::puts("stencil format: all passed");
    return 0;
}
