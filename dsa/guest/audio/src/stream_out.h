// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// An output stream: the HAL interface objects the framework holds, the shared
// queues it talks to them through, and the thread that runs each command it sends
// (OutputStreamLogic) and sends the sound to the host (HostLink).

#pragma once

#include <aidl/android/hardware/audio/core/BnStreamCommon.h>
#include <aidl/android/hardware/audio/core/BnStreamOut.h>
#include <aidl/android/hardware/audio/core/StreamDescriptor.h>

#include <atomic>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>

#include "host_link.h"
#include "output_stream_logic.h"
#include "shared_queue.h"

namespace dsa::audio {

namespace core = ::aidl::android::hardware::audio::core;
namespace common = ::aidl::android::hardware::audio::common;
namespace media = ::aidl::android::media::audio::common;
namespace effect = ::aidl::android::hardware::audio::effect;

class StreamOut;

// The part of a stream the output and the input share. `close` closes the stream and says
// whether it was open.
class StreamCommon final : public core::BnStreamCommon {
   public:
    explicit StreamCommon(std::function<bool()> close) : mClose(std::move(close)) {}

    ndk::ScopedAStatus close() override;
    ndk::ScopedAStatus prepareToClose() override;
    ndk::ScopedAStatus updateHwAvSyncId(int32_t in_hwAvSyncId) override;
    ndk::ScopedAStatus getVendorParameters(const std::vector<std::string>& in_ids,
                                           std::vector<core::VendorParameter>* _aidl_return) override;
    ndk::ScopedAStatus setVendorParameters(const std::vector<core::VendorParameter>& in_parameters,
                                           bool in_async) override;
    ndk::ScopedAStatus addEffect(const std::shared_ptr<effect::IEffect>& in_effect) override;
    ndk::ScopedAStatus removeEffect(const std::shared_ptr<effect::IEffect>& in_effect) override;
    ndk::ScopedAStatus createMmapBuffer(core::MmapBufferDescriptor* _aidl_return) override;

   private:
    std::function<bool()> mClose;
};

class StreamOut final : public core::BnStreamOut {
   public:
    // A stream of `bufferFrames` frames at a time, whose descriptor goes in `descriptor`, which
    // the host's audio port knows as `name` ("primary", "app 0"). `onClosed` runs once, when
    // the framework closes it. Returns null when the shared memory cannot be made.
    static std::shared_ptr<StreamOut> create(int32_t portConfigId, int64_t bufferFrames,
                                             std::string name,
                                             std::function<void(int32_t)> onClosed,
                                             core::StreamDescriptor* descriptor);
    ~StreamOut() override;

    // Stops the stream's thread and its connection to the host. Returns false if it was
    // closed already.
    bool close();

    ndk::ScopedAStatus getStreamCommon(std::shared_ptr<core::IStreamCommon>* _aidl_return) override;
    ndk::ScopedAStatus updateMetadata(const common::SourceMetadata& in_sourceMetadata) override;
    ndk::ScopedAStatus updateOffloadMetadata(
            const common::AudioOffloadMetadata& in_offloadMetadata) override;
    ndk::ScopedAStatus getHwVolume(std::vector<float>* _aidl_return) override;
    ndk::ScopedAStatus setHwVolume(const std::vector<float>& in_channelVolumes) override;
    ndk::ScopedAStatus getAudioDescriptionMixLevel(float* _aidl_return) override;
    ndk::ScopedAStatus setAudioDescriptionMixLevel(float in_leveldB) override;
    ndk::ScopedAStatus getDualMonoMode(media::AudioDualMonoMode* _aidl_return) override;
    ndk::ScopedAStatus setDualMonoMode(media::AudioDualMonoMode in_mode) override;
    ndk::ScopedAStatus getRecommendedLatencyModes(
            std::vector<media::AudioLatencyMode>* _aidl_return) override;
    ndk::ScopedAStatus setLatencyMode(media::AudioLatencyMode in_mode) override;
    ndk::ScopedAStatus getPlaybackRateParameters(media::AudioPlaybackRate* _aidl_return) override;
    ndk::ScopedAStatus setPlaybackRateParameters(const media::AudioPlaybackRate& in_playbackRate) override;
    ndk::ScopedAStatus selectPresentation(int32_t in_presentationId, int32_t in_programId) override;

    StreamOut(int32_t portConfigId, int64_t bufferFrames, std::function<void(int32_t)> onClosed);

   private:
    void work();

    const int32_t mPortConfigId;
    const int64_t mBufferFrames;
    std::function<void(int32_t)> mOnClosed;
    SharedQueue mCommands;
    SharedQueue mReplies;
    SharedQueue mData;
    std::unique_ptr<HostLink> mHost;
    std::shared_ptr<StreamCommon> mCommon;
    std::atomic<bool> mClosed{false};
    std::mutex mCloseLock;
    std::thread mWorker;
};

}  // namespace dsa::audio
