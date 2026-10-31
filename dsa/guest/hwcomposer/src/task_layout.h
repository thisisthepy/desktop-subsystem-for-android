// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// How the hwcomposer splits the one freeform display into the tasks the host shows as
// windows. Nothing here touches Android or the host renderer, so the rules are tested on
// the build machine (tests/task_layout_test.cpp).

#pragma once

#include <cstdint>
#include <map>
#include <optional>
#include <utility>
#include <vector>

namespace dsa::hwc {

// A rectangle in display pixels: left and top inclusive, right and bottom exclusive.
struct Rect {
    int32_t left = 0;
    int32_t top = 0;
    int32_t right = 0;
    int32_t bottom = 0;

    int64_t width() const { return static_cast<int64_t>(right) - left; }
    int64_t height() const { return static_cast<int64_t>(bottom) - top; }
    bool empty() const { return width() <= 0 || height() <= 0; }
    int64_t area() const { return empty() ? 0 : width() * height(); }

    // Whether `inner` is not empty and lies entirely inside this rectangle.
    bool contains(const Rect& inner) const;
    Rect intersect(const Rect& other) const;
    // This rectangle moved so that `origin`'s top left corner becomes (0, 0).
    Rect relativeTo(const Rect& origin) const;

    bool operator==(const Rect& other) const {
        return left == other.left && top == other.top && right == other.right &&
               bottom == other.bottom;
    }
    bool operator!=(const Rect& other) const { return !(*this == other); }
};

// Where SurfaceFlinger places one layer on the display.
struct LayerPlacement {
    int64_t layer = 0;
    int32_t z = 0;
    Rect frame;
};

// A task the agent reported: its ID and its window's bounds on the display.
struct Task {
    int32_t id = 0;
    Rect bounds;
};

// The layers of one task, bottom to top.
struct TaskLayers {
    int32_t task = 0;
    Rect bounds;
    std::vector<int64_t> layers;
};

struct Layout {
    // Tasks that have at least one layer, in ascending task ID.
    std::vector<TaskLayers> tasks;
    // Layers no task contains, bottom to top. They are never sent to the host.
    std::vector<int64_t> unassigned;
};

// Splits the display's layers among the tasks.
//
// A layer belongs to a task whose bounds contain the layer's display frame (clipped to the
// display). A layer no task contains is left out: the wallpaper, display-wide backgrounds
// and dim layers, anything that straddles a window's edge. Sending it would put pixels
// that belong to no window, or to another window, into a host window.
//
// When several tasks contain a layer (one window lies over another), the layer goes to:
//   1. the task whose bounds equal the layer's frame exactly, if exactly one does: that
//      layer is the task's own surface;
//   2. otherwise the task of the nearest layer below it that one of those tasks owns: a
//      task's layers are adjacent in z order, so a popup sits just above its window;
//   3. otherwise the smallest of those tasks, then the lowest task ID.
Layout assignLayersToTasks(const std::vector<LayerPlacement>& layers,
                           const std::vector<Task>& tasks, const Rect& display);

// The latest bounds of every live task, as the agent reports them.
class TaskRegistry {
   public:
    // Records a task, or its new bounds. Returns whether anything changed.
    bool update(int32_t id, const Rect& bounds);
    // Forgets a task. Returns whether it was known.
    bool remove(int32_t id);
    void clear() { mTasks.clear(); }
    // The live tasks, in ascending ID.
    std::vector<Task> tasks() const;

   private:
    std::map<int32_t, Rect> mTasks;
};

// One layer of a task, as far as telling whether its app has drawn at the task's size goes.
struct LayerFill {
    Rect frame;
    // It shows a buffer an app drew (not a solid colour).
    bool drawn = false;
    // The buffer's size and the part of it the layer shows; 0 when that is not known, as for
    // a layer SurfaceFlinger composes itself.
    int64_t bufferWidth = 0;
    int64_t bufferHeight = 0;
    float cropWidth = 0;
    float cropHeight = 0;
};

// Whether the task's app has drawn at the size of `bounds`: one of its drawn layers fills the
// bounds exactly. Until then the task's own surface still has its old size. When a task
// shrinks, SurfaceFlinger clips the old, larger surface to the task, so its frame fills the
// bounds at once; that layer shows part of a larger buffer 1:1, which is the old picture cut
// off, and does not count.
bool drawnAtSize(const Rect& bounds, const std::vector<LayerFill>& layers);

// A value that changes when the app changes its drawn layers: where they are in the task, and
// the size of their buffers. It does not change when the task moves or clips an old surface, or
// when the app draws a new picture into the same layout.
uint64_t drawnGeometry(const Rect& bounds, const std::vector<LayerFill>& layers);

// Whether a task's frame goes to the host, after the task's size changed.
//
// When a task's bounds take a new size, its app has not redrawn yet: its surface keeps the
// old size until the app draws again. A frame composed at the new size then would show the
// old picture in one corner and nothing beside it. The host meanwhile stretches the last
// frame it has over the resized window, which shows no empty space. So after a size change
// the task's frames are held until its app has drawn at the new size, and the first frame
// that goes is a whole one.
//
// Only what was filling is held, and the same goes for a frame of the size the host has: an app
// being recreated at the end of a drag has an incomplete frame too.
//   - A first frame goes as it is.
//   - A frame that fills the task goes.
//   - If the last frame sent did not fill the task (a dialog, a window with margins), every frame
//     goes: such a window has no full frame to keep, and must keep updating.
//   - Otherwise the last frame stays, stretched, with no time limit while the app has not
//     responded, because a stale frame would show empty space. One held for kSlowMs at one size
//     is logged once (HoldSlow).
//   - Once the app has responded by changing its drawn layers (drawnGeometry) and they have been
//     stable for kSettleMs, the frame goes even though it does not fill the task (PostSettled):
//     that is the app's real layout at this size.
class ResizeGate {
   public:
    static constexpr int64_t kSettleMs = 500;
    static constexpr int64_t kSlowMs = 2000;

    enum class Decision {
        // Send the frame.
        Post,
        // Keep the host's last frame; look again at the next frame.
        Hold,
        // Keep the host's last frame, and log that the app has been slow. Once per size.
        HoldSlow,
        // Send the frame, which does not fill the task: the app has settled on this layout.
        PostSettled,
    };

    // `posted` is the size the task's frames last had on the host (0 x 0 before the first),
    // `bounds` the task's bounds now, `drawn` whether its app has drawn at their size,
    // `geometry` the task's drawnGeometry().
    Decision decide(int32_t task, int64_t postedWidth, int64_t postedHeight, const Rect& bounds,
                    bool drawn, int64_t nowMs, uint64_t geometry);
    void forget(int32_t task) { mTasks.erase(task); }
    void clear() { mTasks.clear(); }
    // When a frame must be looked at, in case nothing else changes: to send a layout that has
    // settled, or to log a slow app. Empty when no held task is due either.
    std::optional<int64_t> nextDeadline() const;

   private:
    struct Held {
        int64_t width = 0;
        int64_t height = 0;
        int64_t sinceMs = 0;
        int64_t stableSinceMs = 0;
        // The app's drawn layers differ from those of the last frame sent.
        bool responded = false;
        uint64_t geometry = 0;
        bool logged = false;
    };
    struct TaskState {
        // The last frame sent filled the task.
        bool filled = false;
        // The drawn layers of the last frame sent.
        uint64_t geometry = 0;
        std::optional<Held> held;
    };
    std::map<int32_t, TaskState> mTasks;
};

// The size of a task's ColorBuffers on the host. Each frame is composed into their top-left
// corner at the task's size, so they only need to be at least as large as the task.
struct BufferSize {
    int64_t width = 0;
    int64_t height = 0;

    bool covers(int64_t w, int64_t h) const { return width >= w && height >= h; }
    bool operator==(const BufferSize& other) const {
        return width == other.width && height == other.height;
    }
    bool operator!=(const BufferSize& other) const { return !(*this == other); }
};

// The size a task's ColorBuffers should have, given the size they have now (`current`, 0 x 0
// when there are none), the freeform display, and the task's size. They are made at the
// display's size, the largest a task inside it can be, so that resizing the task's window
// does not remake them: the answer is `current` while that still holds both the display and
// the task. A task larger than the display gets buffers of its own size in that direction.
BufferSize taskBufferSize(BufferSize current, const Rect& display, int64_t taskWidth,
                          int64_t taskHeight);

// Which host display carries each task's frames. A task keeps its display from its first
// frame until it ends, so the host sees one stable pairing per window.
class TaskDisplays {
   public:
    // Display IDs are taken from [firstId, firstId + count).
    TaskDisplays(uint32_t firstId, uint32_t count);

    struct Pairing {
        uint32_t displayId = 0;
        // True when this call made the pairing, so the host has to be told.
        bool created = false;
    };

    // The task's display, pairing it with the lowest free one first if it has none.
    // Empty when every display is taken.
    std::optional<Pairing> acquire(int32_t task);
    // Ends the task's pairing and frees its display, which is returned.
    std::optional<uint32_t> release(int32_t task);
    std::optional<uint32_t> find(int32_t task) const;
    // Every current pairing as (task, display), in ascending task ID.
    std::vector<std::pair<int32_t, uint32_t>> pairings() const;

   private:
    uint32_t mFirstId;
    uint32_t mCount;
    std::map<int32_t, uint32_t> mByTask;
};

}  // namespace dsa::hwc
