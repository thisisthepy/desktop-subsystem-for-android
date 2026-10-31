// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "task_layout.h"

#include <algorithm>
#include <cmath>
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

Layout assignLayersToTasks(const std::vector<LayerPlacement>& layers,
                           const std::vector<Task>& tasks, const Rect& display) {
    std::vector<LayerPlacement> ordered = layers;
    std::stable_sort(ordered.begin(), ordered.end(),
                     [](const LayerPlacement& a, const LayerPlacement& b) {
                         return a.z != b.z ? a.z < b.z : a.layer < b.layer;
                     });

    std::vector<Task> sortedTasks = tasks;
    std::sort(sortedTasks.begin(), sortedTasks.end(),
              [](const Task& a, const Task& b) { return a.id < b.id; });

    // The task each layer went to, in z order; nullopt for a layer no task contains.
    std::vector<std::optional<int32_t>> owner(ordered.size());

    for (size_t i = 0; i < ordered.size(); ++i) {
        const Rect frame = ordered[i].frame.intersect(display);
        std::vector<const Task*> candidates;
        for (const Task& task : sortedTasks) {
            if (task.bounds.contains(frame)) {
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
            layout.tasks.push_back(TaskLayers{task.id, task.bounds, {}});
        }
    }
    for (size_t i = 0; i < ordered.size(); ++i) {
        if (owner[i]) {
            layout.tasks[index[*owner[i]]].layers.push_back(ordered[i].layer);
        } else {
            layout.unassigned.push_back(ordered[i].layer);
        }
    }
    return layout;
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

uint64_t drawnGeometry(const std::vector<LayerFill>& layers) {
    // FNV-1a over the frames of the drawn layers.
    uint64_t hash = 14695981039346656037ull;
    const auto mix = [&hash](int32_t value) {
        hash ^= static_cast<uint32_t>(value);
        hash *= 1099511628211ull;
    };
    for (const LayerFill& layer : layers) {
        if (layer.drawn) {
            mix(layer.frame.left);
            mix(layer.frame.top);
            mix(layer.frame.right);
            mix(layer.frame.bottom);
        }
    }
    return hash;
}

ResizeGate::Decision ResizeGate::decide(int32_t task, int64_t postedWidth, int64_t postedHeight,
                                        const Rect& bounds, bool drawn, int64_t nowMs,
                                        uint64_t geometry) {
    const bool firstFrame = postedWidth <= 0 || postedHeight <= 0;
    const bool sameSize = postedWidth == bounds.width() && postedHeight == bounds.height();
    if (firstFrame || sameSize || drawn) {
        mHeld.erase(task);
        return Decision::Post;
    }
    auto it = mHeld.find(task);
    if (it == mHeld.end() || it->second.width != bounds.width() ||
        it->second.height != bounds.height()) {
        // A new size starts a new wait: the app cannot have drawn at it yet.
        mHeld[task] = Held{bounds.width(), bounds.height(), nowMs, geometry, false};
        return Decision::Hold;
    }
    Held& held = it->second;
    if (geometry != held.geometry) {
        held.reacted = true;
    }
    if (nowMs >= held.deadline()) {
        mHeld.erase(it);
        return Decision::PostLate;
    }
    return Decision::Hold;
}

std::optional<int64_t> ResizeGate::nextDeadline() const {
    std::optional<int64_t> first;
    for (const auto& [_, held] : mHeld) {
        const int64_t deadline = held.deadline();
        if (!first || deadline < *first) {
            first = deadline;
        }
    }
    return first;
}

bool TaskRegistry::update(int32_t id, const Rect& bounds) {
    auto it = mTasks.find(id);
    if (it != mTasks.end() && it->second == bounds) {
        return false;
    }
    mTasks[id] = bounds;
    return true;
}

bool TaskRegistry::remove(int32_t id) { return mTasks.erase(id) != 0; }

std::vector<Task> TaskRegistry::tasks() const {
    std::vector<Task> out;
    out.reserve(mTasks.size());
    for (const auto& [id, bounds] : mTasks) {
        out.push_back(Task{id, bounds});
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

BufferSize taskBufferSize(BufferSize current, const Rect& display, int64_t taskWidth,
                          int64_t taskHeight) {
    const int64_t width = std::max<int64_t>({display.width(), taskWidth, 1});
    const int64_t height = std::max<int64_t>({display.height(), taskHeight, 1});
    if (current.covers(width, height)) {
        return current;
    }
    return BufferSize{width, height};
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
