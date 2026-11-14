// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "stream_out.h"

#include <android/binder_ibinder.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>

#include <cstring>

#include "log.h"
#include "stream_support.h"

namespace dsa::audio {

using core::StreamDescriptor;
using namespace support;

namespace {

constexpr int32_t kFrameBytes = 4;

class QueueSource final : public PcmSource {
   public:
    explicit QueueSource(SharedQueue& queue) : mQueue(queue) {}
    size_t read(uint8_t* out, size_t max) override { return mQueue.read(out, max); }

   private:
    SharedQueue& mQueue;
};

}  // namespace

StreamOut::StreamOut(int32_t portConfigId, int64_t bufferFrames,
                     std::function<void(int32_t)> onClosed)
    : mPortConfigId(portConfigId),
      mBufferFrames(bufferFrames),
      mOnClosed(std::move(onClosed)),
      mCommands(sizeof(StreamDescriptor::Command), sizeof(StreamDescriptor::Command), true),
      mReplies(sizeof(StreamDescriptor::Reply), sizeof(StreamDescriptor::Reply), true),
      mData(1, static_cast<size_t>(bufferFrames) * kFrameBytes, false) {}

std::shared_ptr<StreamOut> StreamOut::create(int32_t portConfigId, int64_t bufferFrames,
                                             std::string name,
                                             std::function<void(int32_t)> onClosed,
                                             StreamDescriptor* descriptor) {
    auto stream = ndk::SharedRefBase::make<StreamOut>(portConfigId, bufferFrames, std::move(onClosed));
    if (!stream->mCommands.valid() || !stream->mReplies.valid() || !stream->mData.valid()) {
        AUDIO_LOGE("cannot make the output stream's shared memory");
        // It never opened, so the module is not told it closed: the module is opening it
        // and holds its own lock.
        stream->mOnClosed = nullptr;
        return nullptr;
    }
    descriptor->command = describe<StreamDescriptor::Command>(stream->mCommands);
    descriptor->reply = describe<StreamDescriptor::Reply>(stream->mReplies);
    descriptor->frameSizeBytes = kFrameBytes;
    descriptor->bufferSizeFrames = bufferFrames;
    descriptor->audio.set<StreamDescriptor::AudioBuffer::Tag::fmq>(describe<int8_t>(stream->mData));
    std::weak_ptr<StreamOut> weak = stream;
    stream->mCommon = ndk::SharedRefBase::make<StreamCommon>([weak] {
        auto open = weak.lock();
        return open != nullptr && open->close();
    });
    stream->mHost = std::make_unique<HostLink>(std::move(name), static_cast<uint32_t>(bufferFrames),
                                               connectToHost);
    stream->mWorker = std::thread([raw = stream.get()] { raw->work(); });
    return stream;
}

StreamOut::~StreamOut() {
    close();
}

bool StreamOut::close() {
    std::lock_guard<std::mutex> lock(mCloseLock);
    if (mClosed.exchange(true)) {
        return false;
    }
    mCommands.wake(kQueueNotEmpty);
    if (mWorker.joinable()) {
        mWorker.join();
    }
    mHost.reset();
    if (mOnClosed) {
        mOnClosed(mPortConfigId);
    }
    return true;
}

void StreamOut::work() {
    setpriority(PRIO_PROCESS, 0, kUrgentAudioPriority);
    MonotonicClock clock;
    QueueSource source(mData);
    StreamFormat format;
    format.bufferFrames = static_cast<uint32_t>(mBufferFrames);
    OutputStreamLogic logic(*mHost, clock, format);
    while (!mClosed.load()) {
        if (mCommands.availableToRead() < sizeof(StreamDescriptor::Command)) {
            mCommands.wait(kQueueNotEmpty, kCommandWaitNs);
            continue;
        }
        StreamDescriptor::Command command;
        uint8_t raw[sizeof(StreamDescriptor::Command)];
        mCommands.read(raw, sizeof(raw));
        std::memcpy(static_cast<void*>(&command), raw, sizeof(raw));
        mCommands.wake(kQueueNotFull);
        const StreamDescriptor::Reply reply = toReply(logic.handle(toCommand(command), source));
        if (!mReplies.write(&reply, sizeof(reply))) {
            AUDIO_LOGE("the framework did not read the last reply; the output stream stops");
            break;
        }
        mReplies.wake(kQueueNotEmpty);
    }
}

ndk::ScopedAStatus StreamOut::getStreamCommon(std::shared_ptr<core::IStreamCommon>* _aidl_return) {
    *_aidl_return = mCommon;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus StreamOut::updateMetadata(const common::SourceMetadata&) {
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus StreamOut::updateOffloadMetadata(const common::AudioOffloadMetadata&) {
    return ndk::ScopedAStatus::fromExceptionCode(EX_ILLEGAL_STATE);
}

ndk::ScopedAStatus StreamOut::getHwVolume(std::vector<float>*) { return unsupported(); }
ndk::ScopedAStatus StreamOut::setHwVolume(const std::vector<float>&) { return unsupported(); }
ndk::ScopedAStatus StreamOut::getAudioDescriptionMixLevel(float*) { return unsupported(); }
ndk::ScopedAStatus StreamOut::setAudioDescriptionMixLevel(float) { return unsupported(); }
ndk::ScopedAStatus StreamOut::getDualMonoMode(media::AudioDualMonoMode*) { return unsupported(); }
ndk::ScopedAStatus StreamOut::setDualMonoMode(media::AudioDualMonoMode) { return unsupported(); }
ndk::ScopedAStatus StreamOut::getRecommendedLatencyModes(std::vector<media::AudioLatencyMode>*) {
    return unsupported();
}
ndk::ScopedAStatus StreamOut::setLatencyMode(media::AudioLatencyMode) { return unsupported(); }
ndk::ScopedAStatus StreamOut::getPlaybackRateParameters(media::AudioPlaybackRate*) {
    return unsupported();
}
ndk::ScopedAStatus StreamOut::setPlaybackRateParameters(const media::AudioPlaybackRate&) {
    return unsupported();
}
ndk::ScopedAStatus StreamOut::selectPresentation(int32_t, int32_t) { return unsupported(); }

ndk::ScopedAStatus StreamCommon::close() {
    if (!mClose || !mClose()) {
        return ndk::ScopedAStatus::fromExceptionCode(EX_ILLEGAL_STATE);
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus StreamCommon::prepareToClose() { return ndk::ScopedAStatus::ok(); }

ndk::ScopedAStatus StreamCommon::updateHwAvSyncId(int32_t) { return unsupported(); }

ndk::ScopedAStatus StreamCommon::getVendorParameters(const std::vector<std::string>&,
                                                     std::vector<core::VendorParameter>*) {
    return ndk::ScopedAStatus::fromExceptionCode(EX_ILLEGAL_ARGUMENT);
}

ndk::ScopedAStatus StreamCommon::setVendorParameters(const std::vector<core::VendorParameter>&, bool) {
    return ndk::ScopedAStatus::fromExceptionCode(EX_ILLEGAL_ARGUMENT);
}

ndk::ScopedAStatus StreamCommon::addEffect(const std::shared_ptr<effect::IEffect>&) {
    return unsupported();
}

ndk::ScopedAStatus StreamCommon::removeEffect(const std::shared_ptr<effect::IEffect>&) {
    return unsupported();
}

ndk::ScopedAStatus StreamCommon::createMmapBuffer(core::MmapBufferDescriptor*) {
    // The output is not a shared memory (MMAP) stream.
    return unsupported();
}

}  // namespace dsa::audio
