// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// One of DSA's decoders, as the framework sees it: a Codec 2.0 component (IComponent) whose
// decoding happens on the host's GPU.
//
// Each work the framework queues carries one access unit. The component copies it out of
// the framework's input buffer, sends it to the host and gives the buffer back at once. The
// host sends back each picture, in presentation order; the component copies it into a
// graphics buffer from the framework's output pool (the app's surface, or a buffer of its
// own when the app reads the pictures itself) and finishes the work the picture came from.

#pragma once

#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include <aidl/android/hardware/media/c2/BnComponent.h>
#include <aidl/android/hardware/media/c2/BnComponentInterface.h>
#include <aidl/android/hardware/media/c2/IComponentListener.h>
#include <aidl/android/hardware/media/c2/IGraphicBufferAllocator.h>
#include <android/hardware_buffer.h>

#include <C2Component.h>
#include <codec2/aidl/Configurable.h>

#include "arrival_queue.h"
#include "bounded_thread.h"
#include "decoder_interface.h"
#include "host_link.h"
#include "output_buffer.h"
#include "output_gate.h"
#include "picture_stats.h"

namespace dsa::codec2 {

namespace c2aidl = ::aidl::android::hardware::media::c2;

// The framework's view of a C2 interface, for its IConfigurable.
std::shared_ptr<c2aidl::IConfigurable> makeConfigurable(const std::shared_ptr<C2ComponentInterface>& intf);

// IComponentInterface over a C2 interface.
class ComponentInterface : public c2aidl::BnComponentInterface {
   public:
    explicit ComponentInterface(const std::shared_ptr<C2ComponentInterface>& intf);
    ndk::ScopedAStatus getConfigurable(std::shared_ptr<c2aidl::IConfigurable>* configurable) override;

   private:
    std::shared_ptr<c2aidl::IConfigurable> mConfigurable;
};

// Makes the C2 interface of `info`'s decoder.
std::shared_ptr<C2ComponentInterface> makeDecoderInterface(const std::shared_ptr<C2ReflectorHelper>& reflector,
                                                           const DecoderInfo& info, const CodecSupport& support,
                                                           std::shared_ptr<DecoderIntf>* params);

class Component : public c2aidl::BnComponent {
   public:
    Component(const DecoderInfo& info, const std::shared_ptr<C2ComponentInterface>& intf,
              const std::shared_ptr<DecoderIntf>& params, const std::shared_ptr<c2aidl::IComponentListener>& listener);
    ~Component() override;

    ndk::ScopedAStatus configureVideoTunnel(int32_t avSyncHwId,
                                            ::aidl::android::hardware::common::NativeHandle* handle) override;
    ndk::ScopedAStatus createBlockPool(const BlockPoolAllocator& allocator, BlockPool* pool) override;
    ndk::ScopedAStatus destroyBlockPool(int64_t blockPoolId) override;
    ndk::ScopedAStatus drain(bool withEos) override;
    ndk::ScopedAStatus flush(c2aidl::WorkBundle* flushedWorkBundle) override;
    ndk::ScopedAStatus getInterface(std::shared_ptr<c2aidl::IComponentInterface>* intf) override;
    ndk::ScopedAStatus queue(const c2aidl::WorkBundle& workBundle) override;
    ndk::ScopedAStatus release() override;
    ndk::ScopedAStatus reset() override;
    ndk::ScopedAStatus start() override;
    ndk::ScopedAStatus stop() override;
    ndk::ScopedAStatus connectToInputSurface(const std::shared_ptr<c2aidl::IInputSurface>& inputSurface,
                                             std::shared_ptr<c2aidl::IInputSurfaceConnection>* connection) override;
    ndk::ScopedAStatus asInputSink(std::shared_ptr<c2aidl::IInputSink>* sink) override;

   private:
    struct Pool {
        int32_t allocatorId = 0;
        std::shared_ptr<c2aidl::IGraphicBufferAllocator> igba;
        ndk::ScopedFileDescriptor waitableFd;
    };

    // On the host link's reading thread: a picture, an end or an error goes to the queue for
    // the component's own thread, in order; a flush's answer is taken at once.
    void onHostMessage(HostMessage message);
    // On the component's own thread: one message from the queue.
    void handle(Arrival& arrival);
    void picture(const Frame& frame, uint64_t ticket, std::chrono::steady_clock::time_point arrived);
    // The host did not decode the packet of `frame` (it came before a key frame): its work
    // finishes with no picture.
    void noPicture(const Frame& frame, uint64_t ticket);
    // The app's process is gone (a call to it failed with DEAD_OBJECT): nothing it held is
    // called again, the stream stops, and the component waits for its last reference to go.
    void peerGone(const char* what);
    // The queue of the host's messages, read once: a stop and a start replace it.
    std::shared_ptr<ArrivalQueue> arrivals() const;
    // Starts the thread that writes the host's pictures into the app's buffers, and stops it;
    // what still waits is dropped.
    void startWorker();
    void stopWorker();
    // Logs the decoder's numbers when they are due (picture_stats.h).
    void logStats();
    void endOfStream(const Frame& frame);
    void decodeError(const std::string& error);
    // Finishes `work` with `output` (an empty output when null), then tells the framework.
    void finish(c2aidl::Work work, AHardwareBuffer* output, uint32_t width, uint32_t height, bool eos);
    // A buffer for a picture that arrived with `ticket`, once its consumer has stopped reading
    // it; null when none came in time or the gate closed. `from` is the surface's allocator
    // it came from, when it did.
    AHardwareBuffer* allocate(uint32_t width, uint32_t height, uint64_t ticket,
                              std::shared_ptr<c2aidl::IGraphicBufferAllocator>* from);
    // Gives back a buffer that will not go out, so its surface can hand it out again.
    void discard(AHardwareBuffer* buffer, const std::shared_ptr<c2aidl::IGraphicBufferAllocator>& from);
    void closeHost();

    const DecoderInfo mInfo;
    const std::shared_ptr<C2ComponentInterface> mIntf;
    const std::shared_ptr<DecoderIntf> mParams;
    const std::shared_ptr<c2aidl::IComponentListener> mListener;
    std::shared_ptr<c2aidl::IComponentInterface> mInterface;

    std::mutex mLock;
    // Signalled under mLock each time the host answers a flush.
    std::condition_variable mFlushed;
    std::unique_ptr<HostDecoder> mHost;
    // Set once a call to the app failed because its process is gone.
    std::atomic<bool> mPeerGone{false};
    // The works sent to the host whose pictures have not come yet, by frame index.
    std::map<uint64_t, c2aidl::Work> mPending;
    // The size last told to the framework.
    uint32_t mWidth = 0;
    uint32_t mHeight = 0;
    std::map<int64_t, Pool> mPools;
    int64_t mNextPoolId = 0x100;
    // Closes at every flush, stop and reset, which ends a picture's waits for its buffer. A
    // flush and a stop close it under mLock, so a picture that takes its work from mPending
    // under mLock with the gate open is never one the flush hands back as well.
    OutputGate mGate;
    // Allocation and write failures in a row, so one on every picture does not flood the log.
    // Used on the component's own thread only.
    FailureThrottle mAllocateFailures;
    FailureThrottle mWriteFailures;
    // The host's messages waiting for the component's own thread (arrival_queue.h), shared
    // with that thread, which may outlive the component by the message it is on. Each start
    // makes a new one, since the thread of a stop that did not wait for it may still hold the
    // old one; read and replaced through std::atomic_load and std::atomic_store.
    std::shared_ptr<ArrivalQueue> mArrivals;
    BoundedThread mWorker;
    // Held while the component's own thread handles a message: a thread a stop let go and the
    // thread of the next start never handle two at once.
    std::mutex mHandling;
    PictureStats mStats;
    // The size of the last picture, for the log line. The component's own thread only.
    uint32_t mLastWidth = 0;
    uint32_t mLastHeight = 0;
};

}  // namespace dsa::codec2
