// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "task_composer.h"

#include <algorithm>
#include <chrono>
#include <unordered_map>

#include <sys/system_properties.h>

#include "log.h"

namespace dsa::hwc {

namespace {

// How long a layer's acquire fence may take before its buffer is composed anyway.
constexpr int kFenceTimeoutMs = 3000;
// The display a compose request names so that the host composes without showing anything.
constexpr uint32_t kNoDisplay = 0;
// The blend mode SurfaceFlinger's client target is composed with.
constexpr int32_t kBlendPremultiplied = 2;

int64_t steadyMs() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(
               std::chrono::steady_clock::now().time_since_epoch())
        .count();
}

double traceMonoMs() {
    return std::chrono::duration<double, std::milli>(
               std::chrono::steady_clock::now().time_since_epoch())
        .count();
}

int64_t traceWallMs() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(
               std::chrono::system_clock::now().time_since_epoch())
        .count();
}

bool traceDetailRequested() {
    char value[PROP_VALUE_MAX] = {};
    __system_property_get("debug.dsa.resize_trace", value);
    return value[0] == '1';
}

void logTrace(const std::vector<std::string>& lines) {
    for (const std::string& line : lines) {
        HWC_LOGI("%s", line.c_str());
    }
}

}  // namespace

TaskComposer::TaskComposer()
    : mTrace(traceDetailRequested()), mReconnector([this] { reconnectLoop(); }) {}

TaskComposer::~TaskComposer() {
    {
        std::lock_guard<std::mutex> lock(mReconnectMutex);
        mStopping = true;
    }
    mReconnectWake.notify_all();
    mReconnector.join();
}

void TaskComposer::setSender(Sender send) {
    std::lock_guard<std::mutex> lock(mCallbackMutex);
    mSend = std::move(send);
}

void TaskComposer::setRefreshRequest(RefreshRequest refresh) {
    std::lock_guard<std::mutex> lock(mCallbackMutex);
    mRefresh = std::move(refresh);
}

void TaskComposer::send(const std::vector<std::vector<uint8_t>>& outbox) {
    if (outbox.empty()) {
        return;
    }
    Sender send;
    {
        std::lock_guard<std::mutex> lock(mCallbackMutex);
        send = mSend;
    }
    if (!send) {
        // channelConnected() announces every pairing once the channel is there.
        return;
    }
    for (const auto& frame : outbox) {
        send(frame);
    }
}

void TaskComposer::taskUpdated(const TaskUpdate& update) {
    bool changed;
    {
        std::lock_guard<std::mutex> lock(mMutex);
        changed = mTasks.update(update.id, update.bounds);
        if (changed) {
            logTrace(mTrace.boundsChanged(update.id, update.bounds.width(),
                                          update.bounds.height(), traceMonoMs(), traceWallMs()));
        }
    }
    if (changed) {
        // The window's frame has to follow its new place now, not at the next app redraw.
        RefreshRequest refresh;
        {
            std::lock_guard<std::mutex> lock(mCallbackMutex);
            refresh = mRefresh;
        }
        if (refresh) {
            refresh();
        }
    }
}

void TaskComposer::taskEnded(int32_t task) {
    std::lock_guard<std::mutex> lock(mMutex);
    mTasks.remove(task);
    mResizeGate.forget(task);
    mTrace.forget(task);
    mDisplays.release(task);
    auto it = mOutputs.find(task);
    if (it != mOutputs.end()) {
        releaseOutputLocked(it->second);
        mOutputs.erase(it);
    }
}

void TaskComposer::channelConnected() {
    std::vector<std::vector<uint8_t>> outbox;
    {
        std::lock_guard<std::mutex> lock(mMutex);
        for (auto& [task, output] : mOutputs) {
            if (output.onHost) {
                outbox.push_back(encodeTaskFrames(task, output.display));
                output.announced = true;
            }
        }
    }
    send(outbox);
}

void TaskComposer::releaseOutputLocked(Output& output) {
    if (!output.onHost || !mHost.connected()) {
        return;
    }
    for (uint32_t& target : output.targets) {
        mHost.closeColorBuffer(target);
        target = 0;
    }
    mHost.destroyDisplay(output.display);
    output.onHost = false;
}

void TaskComposer::syncGenerationLocked() {
    if (mHost.generation() == mGeneration) {
        return;
    }
    // A new connection: whatever the hwcomposer made on the host before went with the old
    // one, so every display is made, and announced, again.
    mGeneration = mHost.generation();
    mShared.clear();
    mResizeGate.clear();
    for (auto& [_, output] : mOutputs) {
        output.onHost = false;
        output.announced = false;
        output.targets[0] = output.targets[1] = 0;
        output.next = 0;
    }
}

uint32_t TaskComposer::sharedHandleLocked(const BufferRef& buffer) {
    if (!buffer.valid()) {
        return 0;
    }
    auto it = mShared.find(buffer.id);
    if (it != mShared.end()) {
        return it->second;
    }
    const uint32_t handle = mHost.openSharedColorBuffer(buffer);
    if (handle != 0) {
        mShared.emplace(buffer.id, handle);
    }
    return handle;
}

void TaskComposer::present(Frame& frame) {
    std::vector<std::vector<uint8_t>> outbox;
    std::optional<int64_t> refreshAt;
    {
        std::lock_guard<std::mutex> lock(mMutex);
        if (!mHost.connected()) {
            // The reconnecting thread asks for a frame once the host is back; connecting
            // here would hold up SurfaceFlinger for as long as the host takes to answer.
            mDisconnected = true;
            mReconnectWake.notify_all();
            return;
        }
        syncGenerationLocked();

        std::vector<LayerPlacement> placements;
        std::unordered_map<int64_t, FrameLayer*> byId;
        placements.reserve(frame.layers.size());
        for (FrameLayer& layer : frame.layers) {
            placements.push_back(LayerPlacement{layer.id, layer.z, layer.frame});
            byId[layer.id] = &layer;
        }
        const Layout layout = assignLayersToTasks(placements, mTasks.tasks(), frame.display);

        bool clientTargetReady = false;
        const int64_t now = steadyMs();
        for (const TaskLayers& task : layout.tasks) {
            const auto pairing = mDisplays.acquire(task.task);
            if (!pairing) {
                if (!mWarnedNoDisplay) {
                    HWC_LOGE("every host display is taken; task %d gets no window frames", task.task);
                    mWarnedNoDisplay = true;
                }
                continue;
            }
            Output& output = mOutputs[task.task];
            output.display = pairing->displayId;
            const int64_t width = std::max<int64_t>(task.bounds.width(), 1);
            const int64_t height = std::max<int64_t>(task.bounds.height(), 1);

            // After a resize, the first frame at the new size is one the app drew at it.
            std::vector<LayerFill> fills;
            fills.reserve(task.layers.size());
            for (int64_t id : task.layers) {
                const FrameLayer& layer = *byId[id];
                fills.push_back(LayerFill{
                    layer.frame,
                    layer.client || (layer.mode == ComposeMode::Device && layer.buffer.valid())});
            }
            const ResizeGate::Decision decision = mResizeGate.decide(
                task.task, output.onHost ? output.width : 0, output.onHost ? output.height : 0,
                task.bounds, drawnAtSize(task.bounds, fills), now);
            if (decision == ResizeGate::Decision::Hold) {
                logTrace(mTrace.held(task.task, width, height, traceMonoMs(), traceWallMs()));
                continue;
            }
            if (decision == ResizeGate::Decision::PostLate) {
                HWC_LOGW("task %d did not draw at %lldx%lld within %lld ms; its frame goes as it is",
                         task.task, static_cast<long long>(width), static_cast<long long>(height),
                         static_cast<long long>(ResizeGate::kHoldMs));
            }

            if (!output.onHost) {
                if (!mHost.createDisplay(output.display, static_cast<uint32_t>(width),
                                         static_cast<uint32_t>(height))) {
                    continue;
                }
                output.onHost = true;
                output.width = width;
                output.height = height;
            } else if (output.width != width || output.height != height) {
                // The window was resized: its frames are composed at the new size.
                for (uint32_t& target : output.targets) {
                    mHost.closeColorBuffer(target);
                    target = 0;
                }
                mHost.resizeDisplay(output.display, static_cast<uint32_t>(width),
                                    static_cast<uint32_t>(height));
                output.width = width;
                output.height = height;
            }
            if (!output.announced) {
                // Before the first post, so the host knows whose frames these are.
                outbox.push_back(encodeTaskFrames(task.task, output.display));
                output.announced = true;
            }

            uint32_t& target = output.targets[output.next];
            if (target == 0) {
                target = mHost.createColorBuffer(static_cast<uint32_t>(width),
                                                 static_cast<uint32_t>(height));
            }
            if (target == 0) {
                continue;
            }

            std::vector<ComposeInput> inputs;
            Rect clientArea;
            int clientSlot = -1;
            for (int64_t id : task.layers) {
                FrameLayer& layer = *byId[id];
                if (layer.client) {
                    // SurfaceFlinger drew it into the client target. The task's part of the
                    // client target goes where its lowest client layer is.
                    clientArea = clientArea.empty()
                                     ? layer.frame.intersect(task.bounds)
                                     : Rect{std::min(clientArea.left, layer.frame.left),
                                            std::min(clientArea.top, layer.frame.top),
                                            std::max(clientArea.right, layer.frame.right),
                                            std::max(clientArea.bottom, layer.frame.bottom)}
                                           .intersect(task.bounds);
                    if (clientSlot < 0) {
                        clientSlot = static_cast<int>(inputs.size());
                        inputs.emplace_back();
                    }
                    continue;
                }
                ComposeInput in;
                in.mode = layer.mode;
                if (layer.mode == ComposeMode::Device) {
                    if (!waitForFence(layer.acquireFence, kFenceTimeoutMs)) {
                        HWC_LOGW("layer %lld's buffer was not ready in %d ms",
                                 static_cast<long long>(layer.id), kFenceTimeoutMs);
                    }
                    in.colorBuffer = sharedHandleLocked(layer.buffer);
                    if (in.colorBuffer == 0) {
                        continue;
                    }
                }
                in.frame = layer.frame;
                in.crop = layer.crop;
                in.blendMode = layer.blendMode;
                in.alpha = layer.alpha;
                std::copy(std::begin(layer.color), std::end(layer.color), std::begin(in.color));
                in.transform = layer.transform;
                inputs.push_back(in);
            }
            if (clientSlot >= 0) {
                if (!clientTargetReady) {
                    waitForFence(frame.clientTargetFence, kFenceTimeoutMs);
                    clientTargetReady = true;
                }
                ComposeInput& slice = inputs[static_cast<size_t>(clientSlot)];
                slice.colorBuffer = sharedHandleLocked(frame.clientTarget);
                // The client target covers the display, so its buffer coordinates are
                // display coordinates.
                slice.frame = clientArea;
                slice.crop = FRect{static_cast<float>(clientArea.left),
                                   static_cast<float>(clientArea.top),
                                   static_cast<float>(clientArea.right),
                                   static_cast<float>(clientArea.bottom)};
                slice.blendMode = kBlendPremultiplied;
                if (slice.colorBuffer == 0 || clientArea.empty()) {
                    inputs.erase(inputs.begin() + clientSlot);
                }
            }

            // Composed for display 0, which names no task window: given the task's display,
            // the host would make the target that display's frame before composing into
            // it, and the window would show each frame one late. The frame goes to the
            // task's display only once it is whole (post).
            const std::vector<uint8_t> request =
                buildComposeRequest(kNoDisplay, target, task.bounds, inputs);
            if (mHost.compose(request)) {
                mHost.post(output.display, target);
                output.next ^= 1;
                logTrace(mTrace.posted(task.task, width, height,
                                       decision == ResizeGate::Decision::PostLate, traceMonoMs(),
                                       traceWallMs()));
            }
        }

        // Drop this process's hold on buffers SurfaceFlinger can no longer show.
        for (auto it = mShared.begin(); it != mShared.end();) {
            if (frame.liveBuffers.count(it->first) || it->first == frame.clientTarget.id) {
                ++it;
            } else {
                mHost.closeColorBuffer(it->second);
                it = mShared.erase(it);
            }
        }

        if (!mHost.connected()) {
            mDisconnected = true;
            mReconnectWake.notify_all();
        }
        refreshAt = mResizeGate.nextDeadline();
    }
    if (refreshAt) {
        // SurfaceFlinger presents only when something changes; a held task whose app never
        // draws still has to be sent once its time is up.
        std::lock_guard<std::mutex> lock(mReconnectMutex);
        mRefreshAtMs = std::max<int64_t>(*refreshAt, 1);
        mReconnectWake.notify_all();
    }
    send(outbox);
}

void TaskComposer::refreshNow() {
    RefreshRequest refresh;
    {
        std::lock_guard<std::mutex> lock(mCallbackMutex);
        refresh = mRefresh;
    }
    if (refresh) {
        refresh();
    }
}

void TaskComposer::reconnectLoop() {
    std::unique_lock<std::mutex> wait(mReconnectMutex);
    while (!mStopping) {
        if (!mDisconnected) {
            if (mRefreshAtMs != 0) {
                const int64_t left = mRefreshAtMs - steadyMs();
                if (left <= 0) {
                    mRefreshAtMs = 0;
                    wait.unlock();
                    refreshNow();
                    wait.lock();
                    continue;
                }
                mReconnectWake.wait_for(wait, std::chrono::milliseconds(std::min<int64_t>(left, 1000)));
                continue;
            }
            mReconnectWake.wait_for(wait, std::chrono::seconds(1));
            continue;
        }
        wait.unlock();
        // Connecting can take a while; frames go on being dropped meanwhile.
        auto connection = HostRenderer::open();
        bool reconnected = false;
        if (connection) {
            std::lock_guard<std::mutex> lock(mMutex);
            mHost.adopt(std::move(connection));
            syncGenerationLocked();
            reconnected = true;
        }
        if (reconnected) {
            mDisconnected = false;
            refreshNow();
        }
        wait.lock();
        if (!reconnected && !mStopping) {
            mReconnectWake.wait_for(wait, std::chrono::seconds(1));
        }
    }
}

}  // namespace dsa::hwc
