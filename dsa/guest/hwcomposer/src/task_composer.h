// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Turns each frame of the one freeform display into one frame per task: the layers of each
// task are composed on the host GPU into the top-left corner of a ColorBuffer, at the size of
// the task's window, which becomes the current frame of that task's own host display and is
// posted. The host never receives the display as a whole.
//
// The ColorBuffers are kept at the display's size (taskBufferSize), so resizing a window
// remakes none of them: only the host display's size, which tells the host how much of the
// buffer the frame fills, changes with the task.

#pragma once

#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <functional>
#include <map>
#include <mutex>
#include <optional>
#include <set>
#include <thread>
#include <vector>

#include "channel_messages.h"
#include "compose_request.h"
#include "frame_pipeline.h"
#include "host_renderer.h"
#include "resize_trace.h"
#include "task_layout.h"
#include "unique_fd.h"

namespace dsa::hwc {

// One layer of a frame, as the display hands it over.
struct FrameLayer {
    int64_t id = 0;
    int32_t z = 0;
    // SurfaceFlinger composes this layer into the client target itself.
    bool client = false;
    ComposeMode mode = ComposeMode::Device;
    BufferRef buffer;
    UniqueFd acquireFence;
    Rect frame;
    FRect crop;
    int32_t blendMode = 1;
    float alpha = 1.0f;
    uint8_t color[4] = {0, 0, 0, 255};
    int32_t transform = 0;
};

struct Frame {
    Rect display;
    // In no particular order; z decides.
    std::vector<FrameLayer> layers;
    BufferRef clientTarget;
    UniqueFd clientTargetFence;
    // Every gralloc buffer SurfaceFlinger may still show again: the ones in the slots.
    std::set<uint32_t> liveBuffers;
};

class TaskComposer {
   public:
    using Sender = std::function<void(const std::vector<uint8_t>&)>;
    using RefreshRequest = std::function<void()>;

    // Display IDs given to tasks: 1 upwards. Display 0 is the host renderer's own.
    static constexpr uint32_t kFirstTaskDisplay = 1;
    static constexpr uint32_t kTaskDisplays = 63;

    // onPresentStart runs once on the thread that sends frames to the host.
    explicit TaskComposer(std::function<void()> onPresentStart = {});
    ~TaskComposer();

    // Where TaskFrames go (the control channel), and how to ask SurfaceFlinger for a new frame.
    void setSender(Sender send);
    void setRefreshRequest(RefreshRequest refresh);

    void taskUpdated(const TaskUpdate& update);
    void taskEnded(int32_t task);
    // The control channel came (back) up: tell the host every pairing again, since it may
    // have missed them.
    void channelConnected();

    // Hands the frame to the thread that composes and posts every task's frame on the host,
    // and returns without waiting for the host. It first waits for the frame before, so by
    // the time it returns the host has finished with that frame's buffers: SurfaceFlinger
    // gives a buffer back to its app only when the present after the one that showed it
    // returns, so none needs a release fence.
    void present(Frame& frame);
    // Returns once the host has finished with every frame handed over.
    void drain();

   private:
    struct Output {
        uint32_t display = 0;
        // The size of the task's frames: the host display's size.
        int64_t width = 0;
        int64_t height = 0;
        // The size of the ColorBuffers in `targets`, at least the frame's.
        BufferSize buffers;
        // The display exists on the host connection mGeneration names.
        bool onHost = false;
        // The host has been told this task's frames go to `display`.
        bool announced = false;
        // Two ColorBuffers, so the host can show one while the next is composed.
        uint32_t targets[2] = {0, 0};
        int next = 0;
    };

    // Composes and posts every task's frame, on the pipeline's thread.
    void compose(Frame& frame);
    uint32_t sharedHandleLocked(const BufferRef& buffer);
    void releaseOutputLocked(Output& output);
    void syncGenerationLocked();
    void reconnectLoop();
    // Asks SurfaceFlinger for a new frame.
    void refreshNow();
    void send(const std::vector<std::vector<uint8_t>>& outbox);

    std::mutex mMutex;
    TaskRegistry mTasks;
    ResizeGate mResizeGate;
    // Per-stage lines when the property debug.dsa.resize_trace is 1 at start.
    ResizeTrace mTrace;
    TaskDisplays mDisplays{kFirstTaskDisplay, kTaskDisplays};
    HostRenderer mHost;
    uint64_t mGeneration = 0;
    std::map<int32_t, Output> mOutputs;
    // Shared ColorBuffers this process holds open, by gralloc buffer ID.
    std::map<uint32_t, uint32_t> mShared;
    bool mWarnedNoDisplay = false;

    std::mutex mCallbackMutex;
    Sender mSend;
    RefreshRequest mRefresh;

    // Reaches the host again after the connection broke, then asks for a frame. It also asks
    // for a frame when a task held back after a resize has waited long enough to be logged
    // (mRefreshAtMs).
    std::mutex mReconnectMutex;
    std::condition_variable mReconnectWake;
    bool mStopping = false;
    // Steady clock milliseconds; 0 for none. Guarded by mReconnectMutex.
    int64_t mRefreshAtMs = 0;
    // Starts true: the first connection is made in the background too.
    std::atomic<bool> mDisconnected{true};
    std::thread mReconnector;
    // Last, so it is stopped, and its frame finished, before anything it uses goes.
    FramePipeline mPipeline;
};

}  // namespace dsa::hwc
