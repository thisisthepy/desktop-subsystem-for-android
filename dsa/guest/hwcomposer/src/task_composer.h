// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Turns each frame of the one freeform display into one frame per task: the layers of each
// task are composed on the host GPU into the top-left corner of a ColorBuffer, at the size of
// the task's window, which becomes the current frame of that task's own host display and is
// posted. The host never receives the display as a whole.
//
// The ColorBuffers follow the task's size in steps (taskBufferSize), so most steps of a resize
// remake none of them: only the host display's size, which tells the host how much of the
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
#include "cpu_upload.h"
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

// A layer whose buffer a CPU writes, for the copy of its picture up to the host.
struct CpuLayer {
    int64_t id = 0;
    BufferRef buffer;
    // The layer's buffer changed since the last copy pass: it holds a new picture.
    bool fresh = false;
    UniqueFd acquireFence;
};

struct Frame {
    Rect display;
    // In no particular order; z decides.
    std::vector<FrameLayer> layers;
    BufferRef clientTarget;
    UniqueFd clientTargetFence;
    // Every gralloc buffer SurfaceFlinger may still show again: the ones in the layers' slots.
    std::set<uint32_t> liveBuffers;
    // The buffers in the client target's slots, held only while SurfaceFlinger composes into
    // them (clientTargetHeld).
    std::set<uint32_t> clientBuffers;
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
    // Copies what a CPU wrote into these layers' buffers up to their host ColorBuffers, and
    // returns once the host holds it: SurfaceFlinger draws its client layers from them next,
    // and the host composes the others.
    void uploadCpuWritten(std::vector<CpuLayer>& layers);
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
        // A frame has been sent on that connection, so the window has one to keep.
        bool posted = false;
        // The host has been told this task's frames go to `display`.
        bool announced = false;
        // Two ColorBuffers, so the host can show one while the next is composed.
        uint32_t targets[2] = {0, 0};
        int next = 0;
        // Steady clock milliseconds since the task has had no layer (minimized, or its app
        // hidden); 0 while it has one.
        int64_t withoutLayersSinceMs = 0;
    };

   public:
    // How long a task may have no layer before its ColorBuffers are let go. The window keeps
    // its last frame in its own buffers, so the host needs them only for the next frame, and
    // a minimized window kept two frames of its size on the host for nothing. A layer gone
    // for a moment (an activity being replaced) keeps them.
    static constexpr int64_t kReleaseWithoutLayersMs = 2000;
    // How long after SurfaceFlinger last composed something itself the client target's
    // ColorBuffers are kept. A frame is asked for when it is up, on which SurfaceFlinger lets
    // go of its own buffers too.
    static constexpr int64_t kReleaseClientTargetMs = 2000;

   private:
    // Lets go of the ColorBuffers of tasks without a layer for kReleaseWithoutLayersMs, and
    // returns when the next one is due (steady clock milliseconds), if any.
    std::optional<int64_t> releaseTasksWithoutLayersLocked(const Layout& layout, int64_t now);

    // Composes and posts every task's frame, on the pipeline's thread. `presentedMs` is when
    // SurfaceFlinger handed the frame over, on the trace's clock.
    void compose(Frame& frame, double presentedMs);
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
    // Buffers copied up for a frame not yet composed: a frame still being composed does not
    // know them, and must not let go of their ColorBuffers.
    std::set<uint32_t> mUploadedAhead;
    // When SurfaceFlinger last composed into the client target, and when the frame that lets
    // go of it is due.
    ClientTargetUse mClientTarget{kReleaseClientTargetMs};
    CpuUploads mUploads;
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
