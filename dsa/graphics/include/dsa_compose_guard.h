// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// A composition never writes a ColorBuffer the host is still copying into a task window.
//
// The hwcomposer composes each task's frame into a ColorBuffer of its own (rcComposeWithoutPost)
// and then makes it the task display's current frame (rcSetDisplayColorBuffer). The renderer
// copies that frame into the task window later, on its presentation thread. Composing the next
// frame into the same ColorBuffer before that copy has read it would show a picture half of one
// frame and half of the next, or the next frame at the size of the one before. The hwcomposer
// used to keep two ColorBuffers per task for that, each as large as the task can be (24 MiB for a
// task of 3072x2048).
//
// A host that lists DSA_COMPOSE_GUARD_EXTENSION makes rcComposeWithoutPost wait, for at most
// DSA_COMPOSE_GUARD_WAIT_MS, until no frame in the target ColorBuffer waits for or is in its copy
// into a window. If one still is by then, nothing is composed and the call answers
// DSA_COMPOSE_TARGET_BUSY, and the hwcomposer composes the frame into a second ColorBuffer
// instead. So a task whose window keeps up holds one ColorBuffer, and only a task whose host is
// behind holds two, as every task did before.

#pragma once

#include <stdint.h>

#define DSA_COMPOSE_GUARD_EXTENSION "ANDROID_EMU_dsa_compose_waits_for_presentation"

// What rcComposeWithoutPost answers when its target was still being copied into a window. A
// hwcomposer that does not know it takes it as a composition that failed, and keeps its frame.
#define DSA_COMPOSE_TARGET_BUSY (-2)

// The longest a composition waits for its target's copy. A copy takes a few milliseconds; a
// longer wait means the presentation is held up by another window, and a second ColorBuffer is
// cheaper than the frame time lost.
#define DSA_COMPOSE_GUARD_WAIT_MS 8u

#ifdef __cplusplus
extern "C" {
#endif

// Waits up to `timeout_ms` until no frame in `color_buffer` waits for or is in its copy into a
// task window. Answers 1 when none is, 0 when one still is.
typedef int (*dsa_compose_guard_fn)(uint32_t color_buffer, uint32_t timeout_ms);

// Set by the renderer's presentation when it tracks the frames it copies; null otherwise, and
// then the host does not list DSA_COMPOSE_GUARD_EXTENSION. Defined with rcComposeWithoutPost
// (host patch 0046).
extern dsa_compose_guard_fn dsa_compose_target_guard;

#ifdef __cplusplus
}
#endif
