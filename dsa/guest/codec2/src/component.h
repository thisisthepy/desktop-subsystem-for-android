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

#include <condition_variable>
#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <vector>

#include <aidl/android/hardware/media/c2/BnComponent.h>
#include <aidl/android/hardware/media/c2/BnComponentInterface.h>
#include <aidl/android/hardware/media/c2/IComponentListener.h>
#include <aidl/android/hardware/media/c2/IGraphicBufferAllocator.h>
#include <android/hardware_buffer.h>

#include <C2Component.h>
#include <codec2/aidl/Configurable.h>

#include "decoder_interface.h"
#include "host_link.h"

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

    void onHostMessage(const HostMessage& message);
    void picture(const Frame& frame);
    void endOfStream(const Frame& frame);
    void decodeError(const std::string& error);
    // Finishes `work` with `output` (an empty output when null), then tells the framework.
    void finish(c2aidl::Work work, AHardwareBuffer* output, uint32_t width, uint32_t height, bool eos);
    AHardwareBuffer* allocate(uint32_t width, uint32_t height);
    void closeHost();

    const DecoderInfo mInfo;
    const std::shared_ptr<C2ComponentInterface> mIntf;
    const std::shared_ptr<DecoderIntf> mParams;
    const std::shared_ptr<c2aidl::IComponentListener> mListener;
    std::shared_ptr<c2aidl::IComponentInterface> mInterface;

    std::mutex mLock;
    std::condition_variable mFlushed;
    bool mFlushDone = false;
    std::unique_ptr<HostDecoder> mHost;
    // The works sent to the host whose pictures have not come yet, by frame index.
    std::map<uint64_t, c2aidl::Work> mPending;
    // The size last told to the framework.
    uint32_t mWidth = 0;
    uint32_t mHeight = 0;
    std::map<int64_t, Pool> mPools;
    int64_t mNextPoolId = 0x100;
};

}  // namespace dsa::codec2
