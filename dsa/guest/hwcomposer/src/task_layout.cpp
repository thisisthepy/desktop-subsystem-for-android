// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "task_layout.h"

#include <algorithm>
#include <cmath>
#include <cstdlib>
#include <set>

namespace dsa::hwc {

bool Rect::contains(const Rect& inner) const {
    return !inner.empty() && !empty() && inner.left >= left && inner.top >= top &&
           inner.right <= right && inner.bottom <= bottom;
}

Rect Rect::intersect(const Rect& other) const {
    Rect out{std::max(left, other.left), std::max(top, other.top),
             std::min(right, other.right), std::min(bottom, other.bottom)};
    if (out.empty()) {
        return Rect{};
    }
    return out;
}

Rect Rect::relativeTo(const Rect& origin) const {
    return Rect{left - origin.left, top - origin.top, right - origin.left,
                bottom - origin.top};
}

bool coversDisplay(const Rect& bounds, const Rect& display) {
    return !display.empty() && bounds.intersect(display) == display;
}

Layout assignLayersToTasks(const std::vector<LayerPlacement>& layers,
                           const std::vector<Task>& tasks, const Rect& display) {
    std::vector<LayerPlacement> ordered = layers;
    std::stable_sort(ordered.begin(), ordered.end(),
                     [](const LayerPlacement& a, const LayerPlacement& b) {
                         return a.z != b.z ? a.z < b.z : a.layer < b.layer;
                     });

    // An overlay that covers the display has no window, so it is no candidate for any layer.
    std::vector<Task> sortedTasks;
    size_t screenOverlays = 0;
    for (const Task& task : tasks) {
        if (task.overlay && coversDisplay(task.bounds, display)) {
            ++screenOverlays;
        } else {
            sortedTasks.push_back(task);
        }
    }
    std::sort(sortedTasks.begin(), sortedTasks.end(),
              [](const Task& a, const Task& b) { return a.id < b.id; });

    // The task each layer went to, in z order; nullopt for a layer no task contains.
    std::vector<std::optional<int32_t>> owner(ordered.size());

    // The overlays' own surfaces first. `composedFrom` is where each overlay's layers are this
    // frame: its bounds, or its surface's frame while the agent's report is a frame behind.
    std::map<int32_t, Rect> composedFrom;
    for (const Task& overlay : sortedTasks) {
        if (!overlay.overlay) {
            continue;
        }
        const Rect wanted = overlay.bounds.intersect(display);
        for (size_t i = 0; i < ordered.size(); ++i) {
            if (!owner[i] && !wanted.empty() && ordered[i].frame.intersect(display) == wanted) {
                owner[i] = overlay.id;
                composedFrom[overlay.id] = overlay.bounds;
            }
        }
    }
    for (const Task& overlay : sortedTasks) {
        if (!overlay.overlay || composedFrom.count(overlay.id)) {
            continue;
        }
        // Its app is moving it: the layer of its size that is nobody else's surface, nearest
        // to where the agent last saw it.
        std::optional<size_t> nearest;
        int64_t nearestDistance = 0;
        for (size_t i = 0; i < ordered.size(); ++i) {
            const Rect frame = ordered[i].frame.intersect(display);
            if (owner[i] || frame.width() != overlay.bounds.width() ||
                frame.height() != overlay.bounds.height()) {
                continue;
            }
            bool taskSurface = false;
            for (const Task& task : sortedTasks) {
                taskSurface = taskSurface || (task.id != overlay.id && task.bounds == frame);
            }
            if (taskSurface) {
                continue;
            }
            const int64_t distance =
                std::llabs(static_cast<int64_t>(frame.left) - overlay.bounds.left) +
                std::llabs(static_cast<int64_t>(frame.top) - overlay.bounds.top);
            if (!nearest || distance < nearestDistance) {
                nearest = i;
                nearestDistance = distance;
            }
        }
        if (nearest) {
            owner[*nearest] = overlay.id;
            composedFrom[overlay.id] = ordered[*nearest].frame.intersect(display);
        }
    }

    for (size_t i = 0; i < ordered.size(); ++i) {
        if (owner[i]) {
            continue;
        }
        const Rect frame = ordered[i].frame.intersect(display);
        std::vector<const Task*> candidates;
        for (const Task& task : sortedTasks) {
            if (task.overlay) {
                // Only above its own surface, and inside where that surface is now.
                auto from = composedFrom.find(task.id);
                bool above = false;
                for (size_t below = 0; below < i && !above; ++below) {
                    above = owner[below] && *owner[below] == task.id;
                }
                if (from != composedFrom.end() && above && from->second.contains(frame)) {
                    candidates.push_back(&task);
                }
            } else if (task.bounds.contains(frame)) {
                candidates.push_back(&task);
            }
        }
        if (candidates.empty()) {
            continue;
        }
        if (candidates.size() == 1) {
            owner[i] = candidates.front()->id;
            continue;
        }

        const Task* exact = nullptr;
        int exactCount = 0;
        for (const Task* task : candidates) {
            if (task->bounds == frame) {
                exact = task;
                ++exactCount;
            }
        }
        if (exactCount == 1) {
            owner[i] = exact->id;
            continue;
        }

        std::set<int32_t> candidateIds;
        for (const Task* task : candidates) {
            candidateIds.insert(task->id);
        }
        for (size_t below = i; below-- > 0;) {
            if (owner[below] && candidateIds.count(*owner[below])) {
                owner[i] = owner[below];
                break;
            }
        }
        if (owner[i]) {
            continue;
        }

        const Task* smallest = candidates.front();
        for (const Task* task : candidates) {
            if (task->bounds.area() < smallest->bounds.area()) {
                smallest = task;
            }
        }
        owner[i] = smallest->id;
    }

    Layout layout;
    std::map<int32_t, size_t> index;
    for (const Task& task : sortedTasks) {
        bool used = false;
        for (const auto& id : owner) {
            if (id && *id == task.id) {
                used = true;
                break;
            }
        }
        if (used) {
            index[task.id] = layout.tasks.size();
            auto from = composedFrom.find(task.id);
            const Rect bounds = from != composedFrom.end() ? from->second : task.bounds;
            layout.tasks.push_back(TaskLayers{task.id, bounds, {}, task.overlay});
        }
    }
    // The highest layer an app task owns: an overlay that covers the display lies above it.
    std::optional<size_t> topOfTasks;
    for (size_t i = 0; i < ordered.size(); ++i) {
        if (owner[i]) {
            const TaskLayers& taken = layout.tasks[index[*owner[i]]];
            layout.tasks[index[*owner[i]]].layers.push_back(ordered[i].layer);
            if (!taken.overlay) {
                topOfTasks = i;
            }
        } else {
            layout.unassigned.push_back(ordered[i].layer);
        }
    }
    if (screenOverlays > 0 && topOfTasks) {
        // From the top down, so a display-wide layer under the tasks (the home activity, a
        // wallpaper) is never taken for one.
        std::vector<int64_t> found;
        for (size_t i = ordered.size(); i-- > *topOfTasks + 1 && found.size() < screenOverlays;) {
            if (!owner[i] && ordered[i].frame.intersect(display) == display) {
                found.push_back(ordered[i].layer);
            }
        }
        layout.screen.assign(found.rbegin(), found.rend());
    }
    return layout;
}

bool showsOwnBuffer(const Rect& bounds, const std::vector<OverlaySurface>& layers) {
    if (layers.size() != 1) {
        return false;
    }
    const OverlaySurface& layer = layers.front();
    const auto about = [](float value, int64_t whole) {
        return std::abs(static_cast<double>(value) - static_cast<double>(whole)) <= 0.5;
    };
    return layer.rgbaBuffer && layer.frame == bounds && layer.transform == 0 &&
           layer.alpha >= 1.0f && layer.bufferWidth == bounds.width() &&
           layer.bufferHeight == bounds.height() && about(layer.cropLeft, 0) &&
           about(layer.cropTop, 0) && about(layer.cropRight, layer.bufferWidth) &&
           about(layer.cropBottom, layer.bufferHeight);
}

namespace {

// A layer that shows part of a larger buffer pixel for pixel: an old surface clipped to a task
// that shrank. An app's own surface always shows all of a buffer of its window's size, and a
// surface an app scales shows a crop of another size than its frame.
bool clippedOldSurface(const LayerFill& layer) {
    if (layer.bufferWidth <= 0 || layer.bufferHeight <= 0) {
        return false;
    }
    const bool largerBuffer =
        layer.bufferWidth > layer.frame.width() || layer.bufferHeight > layer.frame.height();
    const bool oneToOne =
        std::abs(static_cast<double>(layer.cropWidth) - static_cast<double>(layer.frame.width())) <= 1 &&
        std::abs(static_cast<double>(layer.cropHeight) - static_cast<double>(layer.frame.height())) <= 1;
    return largerBuffer && oneToOne;
}

}  // namespace

bool drawnAtSize(const Rect& bounds, const std::vector<LayerFill>& layers) {
    for (const LayerFill& layer : layers) {
        if (layer.drawn && layer.frame == bounds && !clippedOldSurface(layer)) {
            return true;
        }
    }
    return false;
}

uint64_t drawnGeometry(const Rect& bounds, const std::vector<LayerFill>& layers) {
    // FNV-1a.
    uint64_t hash = 14695981039346656037ull;
    const auto mix = [&hash](int64_t value) {
        hash ^= static_cast<uint64_t>(value);
        hash *= 1099511628211ull;
    };
    for (const LayerFill& layer : layers) {
        if (!layer.drawn) {
            continue;
        }
        mix(layer.frame.left - bounds.left);
        mix(layer.frame.top - bounds.top);
        if (layer.bufferWidth > 0 && layer.bufferHeight > 0) {
            // Not the layer's size: clipping a surface to a task that shrank changes that, and
            // the app has done nothing. A new layout draws into a buffer of its own size.
            mix(layer.bufferWidth);
            mix(layer.bufferHeight);
        } else {
            mix(layer.frame.width());
            mix(layer.frame.height());
        }
    }
    return hash;
}

bool leavesLastFrame(bool layerMissing, bool hasFrame) { return layerMissing && hasFrame; }

bool reportAsksForFrame(bool boundsChanged) { return boundsChanged; }

uint32_t lastPostedTarget(bool posted, bool onHost, const uint32_t targets[2], int next) {
    if (!posted || !onHost) {
        return 0;
    }
    // Each post flips `next`, so the last one composed is the other target.
    return targets[(next ^ 1) & 1];
}

bool leavesLastFrameUndrawn(const std::vector<LayerFill>& layers, bool hasFrame) {
    if (!hasFrame) {
        return false;
    }
    for (const LayerFill& layer : layers) {
        if (layer.drawn && layer.alpha > 0.0f) {
            return false;
        }
    }
    return true;
}

bool releaseDue(int64_t withoutLayersSinceMs, int64_t nowMs, int64_t delayMs) {
    return withoutLayersSinceMs > 0 && nowMs - withoutLayersSinceMs >= delayMs;
}

bool clientTargetHeld(int64_t lastClientFrameMs, int64_t nowMs, int64_t delayMs) {
    return lastClientFrameMs > 0 && !releaseDue(lastClientFrameMs, nowMs, delayMs);
}

void ClientTargetUse::presented(bool clientLayer, int64_t nowMs) {
    if (clientLayer) {
        mLastMs = std::max<int64_t>(nowMs, 1);
    }
}

std::optional<int64_t> ClientTargetUse::refreshAt() const {
    if (mLastMs == 0) {
        return std::nullopt;
    }
    return mLastMs + mDelayMs;
}

std::optional<int64_t> ClientTargetUse::letGo(int64_t nowMs) {
    if (mLastMs == 0 || held(nowMs)) {
        return std::nullopt;
    }
    const int64_t since = nowMs - mLastMs;
    mLastMs = 0;
    return since;
}

ResizeGate::Decision ResizeGate::decide(int32_t task, int64_t postedWidth, int64_t postedHeight,
                                        const Rect& bounds, bool drawn, int64_t nowMs,
                                        uint64_t geometry) {
    auto found = mTasks.find(task);
    const bool firstFrame = postedWidth <= 0 || postedHeight <= 0 || found == mTasks.end();
    if (firstFrame || drawn || !found->second.filled) {
        // A first frame has nothing to fall back on, a frame that fills is what is waited for,
        // and a task that was not filling has no full frame to keep.
        mTasks[task] = TaskState{drawn, geometry, std::nullopt};
        return Decision::Post;
    }

    TaskState& state = found->second;
    if (!state.held || state.held->width != bounds.width() ||
        state.held->height != bounds.height()) {
        // A new size starts a new wait: whatever the app did, it was not for this size.
        state.held = Held{bounds.width(),
                          bounds.height(),
                          nowMs,
                          nowMs,
                          geometry != state.geometry,
                          geometry,
                          false};
    }
    Held& held = *state.held;
    if (geometry != held.geometry) {
        held.geometry = geometry;
        held.responded = true;
        held.stableSinceMs = nowMs;
    } else if (held.responded && nowMs - held.stableSinceMs >= kSettleMs) {
        // The app has answered and stayed that way: this is its layout, not a stale frame.
        state = TaskState{false, geometry, std::nullopt};
        return Decision::PostSettled;
    }
    if (!held.responded && !held.logged && nowMs - held.sinceMs >= kSlowMs) {
        held.logged = true;
        return Decision::HoldSlow;
    }
    return Decision::Hold;
}

std::optional<int64_t> ResizeGate::nextDeadline() const {
    std::optional<int64_t> first;
    for (const auto& [_, state] : mTasks) {
        if (!state.held) {
            continue;
        }
        const Held& held = *state.held;
        if (!held.responded && held.logged) {
            continue;
        }
        const int64_t deadline =
            held.responded ? held.stableSinceMs + kSettleMs : held.sinceMs + kSlowMs;
        if (!first || deadline < *first) {
            first = deadline;
        }
    }
    return first;
}

bool TaskRegistry::update(int32_t id, const Rect& bounds, bool overlay) {
    auto it = mTasks.find(id);
    if (it != mTasks.end() && it->second.bounds == bounds && it->second.overlay == overlay) {
        return false;
    }
    mTasks[id] = Task{id, bounds, overlay};
    return true;
}

bool TaskRegistry::remove(int32_t id) { return mTasks.erase(id) != 0; }

std::vector<Task> TaskRegistry::tasks() const {
    std::vector<Task> out;
    out.reserve(mTasks.size());
    for (const auto& [id, task] : mTasks) {
        out.push_back(task);
    }
    return out;
}

TaskDisplays::TaskDisplays(uint32_t firstId, uint32_t count) : mFirstId(firstId), mCount(count) {}

std::optional<TaskDisplays::Pairing> TaskDisplays::acquire(int32_t task) {
    auto it = mByTask.find(task);
    if (it != mByTask.end()) {
        return Pairing{it->second, false};
    }
    std::set<uint32_t> used;
    for (const auto& [_, display] : mByTask) {
        used.insert(display);
    }
    for (uint32_t offset = 0; offset < mCount; ++offset) {
        const uint32_t display = mFirstId + offset;
        if (!used.count(display)) {
            mByTask[task] = display;
            return Pairing{display, true};
        }
    }
    return std::nullopt;
}

namespace {

// A drag remakes the buffers each time it crosses a multiple of this. Remaking takes tens of
// milliseconds; each buffer's overshoot past the task costs up to this much of a side.
constexpr int64_t kBufferStep = 512;

// `task` rounded up to a step, no longer than the display unless the task itself is.
int64_t bufferLength(int64_t task, int64_t display) {
    const int64_t rounded = (task + kBufferStep - 1) / kBufferStep * kBufferStep;
    return std::max(task, std::min(rounded, display));
}

}  // namespace

BufferSize taskBufferSize(BufferSize current, const Rect& display, int64_t taskWidth,
                          int64_t taskHeight) {
    const int64_t width = std::max<int64_t>(taskWidth, 1);
    const int64_t height = std::max<int64_t>(taskHeight, 1);
    const BufferSize fitting{bufferLength(width, display.width()),
                             bufferLength(height, display.height())};
    const bool wasteful = current.width * current.height >= 2 * fitting.width * fitting.height;
    if (current.covers(width, height) && !wasteful) {
        return current;
    }
    return fitting;
}

std::optional<uint32_t> TaskDisplays::release(int32_t task) {
    auto it = mByTask.find(task);
    if (it == mByTask.end()) {
        return std::nullopt;
    }
    const uint32_t display = it->second;
    mByTask.erase(it);
    return display;
}

std::optional<uint32_t> TaskDisplays::find(int32_t task) const {
    auto it = mByTask.find(task);
    if (it == mByTask.end()) {
        return std::nullopt;
    }
    return it->second;
}

std::vector<std::pair<int32_t, uint32_t>> TaskDisplays::pairings() const {
    return {mByTask.begin(), mByTask.end()};
}

}  // namespace dsa::hwc
