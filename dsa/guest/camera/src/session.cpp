// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "session.h"

#include <aidl/android/hardware/camera/common/Status.h>
#include <unistd.h>

#include <ctime>

#include "characteristics.h"
#include "graphic_buffer.h"
#include "log.h"
#include "stream_config.h"

namespace dsa::camera {

namespace common = ::aidl::android::hardware::camera::common;
namespace graphics = ::aidl::android::hardware::graphics::common;

namespace {

// How long a request waits for the host camera's next picture before it fails.
constexpr std::chrono::milliseconds kPictureWait{2000};
constexpr int kFenceWaitMs = 1000;
// Buffers the framework may hand each stream at once.
constexpr int32_t kMaxBuffers = 4;

ndk::ScopedAStatus error(common::Status status) {
    return ndk::ScopedAStatus::fromServiceSpecificError(static_cast<int32_t>(status));
}

int64_t monotonicNs() {
    timespec now{};
    clock_gettime(CLOCK_MONOTONIC, &now);
    return static_cast<int64_t>(now.tv_sec) * 1000000000LL + now.tv_nsec;
}

device::StreamBuffer resultBuffer(int32_t streamId, int64_t bufferId, bool ok) {
    device::StreamBuffer buffer;
    buffer.streamId = streamId;
    buffer.bufferId = bufferId;
    buffer.status = ok ? device::BufferStatus::OK : device::BufferStatus::ERROR;
    return buffer;
}

device::CaptureResult emptyResult(int32_t frameNumber) {
    device::CaptureResult result;
    result.frameNumber = frameNumber;
    result.fmqResultSize = 0;
    result.partialResult = 0;
    result.inputBuffer.streamId = -1;
    return result;
}

}  // namespace

Session::Session(HostCamera camera, std::shared_ptr<device::ICameraDeviceCallback> callback)
    : mCamera(std::move(camera)), mCallback(std::move(callback)) {
    mWorker = std::thread([this] { work(); });
}

Session::~Session() {
    close();
}

bool Session::closed() {
    std::lock_guard<std::mutex> lock(mLock);
    return mClosed;
}

ndk::ScopedAStatus Session::close() {
    {
        std::lock_guard<std::mutex> lock(mLock);
        if (mClosed) {
            return ndk::ScopedAStatus::ok();
        }
    }
    flush();
    {
        std::lock_guard<std::mutex> lock(mLock);
        mClosed = true;
    }
    mChanged.notify_all();
    if (mWorker.joinable()) {
        if (mWorker.get_id() == std::this_thread::get_id()) {
            mWorker.detach();
        } else {
            mWorker.join();
        }
    }
    {
        std::lock_guard<std::mutex> host(mHostLock);
        if (mHost) {
            mHost->stop();
            mHost->close();
            mHost.reset();
        }
    }
    std::lock_guard<std::mutex> lock(mLock);
    releaseBuffers(std::nullopt);
    CAMERA_LOGI("%s closed; the host camera is let go", mCamera.name.c_str());
    return ndk::ScopedAStatus::ok();
}

void Session::releaseBuffers(const std::optional<int32_t>& streamId) {
    for (auto it = mBuffers.begin(); it != mBuffers.end();) {
        if (!streamId || it->first.first == *streamId) {
            AHardwareBuffer_release(it->second);
            it = mBuffers.erase(it);
        } else {
            ++it;
        }
    }
}

bool Session::startHost(uint32_t fps, std::string* error) {
    if (!mHost) {
        mHost = HostCameraLink::connect(error);
        if (!mHost) {
            return false;
        }
    }
    Opened opened;
    if (!mHost->start(mCamera.id, mHostWidth, mHostHeight, fps, &opened, error)) {
        mHost->close();
        mHost.reset();
        return false;
    }
    mHostFps = fps;
    mLastPicture.reset();
    // The host may run a size of its own choosing; the pictures are scaled to each stream.
    CAMERA_LOGI("%s runs at %ux%u, %u frames a second", mCamera.name.c_str(), opened.width, opened.height,
                opened.fps);
    return true;
}

ndk::ScopedAStatus Session::configureStreams(const device::StreamConfiguration& in_requestedConfiguration,
                                             std::vector<device::HalStream>* _aidl_return) {
    std::vector<StreamAsk> asks;
    for (const device::Stream& stream : in_requestedConfiguration.streams) {
        if (stream.width <= 0 || stream.height <= 0) {
            return error(common::Status::ILLEGAL_ARGUMENT);
        }
        asks.push_back(StreamAsk{static_cast<int32_t>(stream.format), static_cast<uint32_t>(stream.width),
                                 static_cast<uint32_t>(stream.height),
                                 stream.streamType == device::StreamType::OUTPUT});
        if (stream.rotation != device::StreamRotation::ROTATION_0) {
            CAMERA_LOGW("stream %d asks for a rotation; it is not turned", stream.id);
        }
    }
    std::string why;
    if (!streamsSupported(mCamera, asks, &why)) {
        CAMERA_LOGE("%s cannot run those streams: %s", mCamera.name.c_str(), why.c_str());
        return error(common::Status::ILLEGAL_ARGUMENT);
    }
    {
        std::unique_lock<std::mutex> lock(mLock);
        if (mClosed) {
            return error(common::Status::INTERNAL_ERROR);
        }
        // A configuration comes only once every request has its result.
        mChanged.wait(lock, [&] { return mQueue.empty() && mInFlight == 0; });
    }

    std::vector<device::HalStream> halStreams;
    std::map<int32_t, StreamInfo> streams;
    for (const device::Stream& stream : in_requestedConfiguration.streams) {
        device::HalStream hal;
        hal.id = stream.id;
        // Preview buffers the screen shows are RGBA, video buffers YUV; stills and YUV are as
        // asked. The usage is one the guest's allocator makes buffers of that format with.
        const int32_t format =
            overrideFormatFor(static_cast<int32_t>(stream.format), static_cast<uint64_t>(stream.usage));
        const uint64_t producerUsage = producerUsageFor(format);
        hal.overrideFormat = static_cast<graphics::PixelFormat>(format);
        hal.producerUsage = static_cast<graphics::BufferUsage>(producerUsage);
        hal.consumerUsage = static_cast<graphics::BufferUsage>(0);
        hal.maxBuffers = kMaxBuffers;
        hal.overrideDataSpace = stream.dataSpace;
        hal.supportOffline = false;
        halStreams.push_back(hal);

        StreamInfo info;
        info.width = static_cast<uint32_t>(stream.width);
        info.height = static_cast<uint32_t>(stream.height);
        info.format = static_cast<int32_t>(hal.overrideFormat);
        info.usage = static_cast<uint64_t>(stream.usage) | producerUsage;
        info.bufferSize = stream.bufferSize > 0 ? static_cast<size_t>(stream.bufferSize)
                                                : static_cast<size_t>(jpegMaxSize(mCamera));
        streams[stream.id] = info;
    }

    const StreamSize size = hostSizeFor(mCamera, asks);
    {
        std::lock_guard<std::mutex> host(mHostLock);
        // A close that came while the streams were read has let the host camera go already
        // (it sets mClosed before it takes mHostLock); starting it now would hold the host's
        // camera for a session nobody closes again.
        if (closed()) {
            return error(common::Status::INTERNAL_ERROR);
        }
        const bool resize = !mHost || size.width != mHostWidth || size.height != mHostHeight;
        mHostWidth = size.width;
        mHostHeight = size.height;
        if (resize) {
            const uint32_t fps = static_cast<uint32_t>(targetFpsRange(mCamera, false).second);
            if (!startHost(fps, &why)) {
                CAMERA_LOGE("%s did not start on the host: %s", mCamera.name.c_str(), why.c_str());
                return error(common::Status::INTERNAL_ERROR);
            }
        }
    }

    std::lock_guard<std::mutex> lock(mLock);
    for (const auto& [id, info] : mStreams) {
        auto kept = streams.find(id);
        if (kept == streams.end() || kept->second.width != info.width || kept->second.height != info.height ||
            kept->second.format != info.format) {
            releaseBuffers(id);
        }
    }
    mStreams = std::move(streams);
    *_aidl_return = std::move(halStreams);
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Session::constructDefaultRequestSettings(device::RequestTemplate in_type,
                                                            device::CameraMetadata* _aidl_return) {
    Metadata settings;
    if (!defaultSettings(mCamera, static_cast<int32_t>(in_type), &settings)) {
        return error(common::Status::ILLEGAL_ARGUMENT);
    }
    _aidl_return->metadata = settings.bytes();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Session::flush() {
    std::deque<Pending> dropped;
    {
        std::lock_guard<std::mutex> lock(mLock);
        mFlushing = true;
        dropped.swap(mQueue);
    }
    for (Pending& pending : dropped) {
        fail(pending, true);
    }
    std::unique_lock<std::mutex> lock(mLock);
    mChanged.wait(lock, [&] { return mInFlight == 0; });
    mFlushing = false;
    mChanged.notify_all();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Session::getCaptureRequestMetadataQueue(
    fmq::MQDescriptor<int8_t, fmq::SynchronizedReadWrite>* _aidl_return) {
    // No queue: the settings come in each request, which the framework does then.
    *_aidl_return = fmq::MQDescriptor<int8_t, fmq::SynchronizedReadWrite>();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Session::getCaptureResultMetadataQueue(
    fmq::MQDescriptor<int8_t, fmq::SynchronizedReadWrite>* _aidl_return) {
    *_aidl_return = fmq::MQDescriptor<int8_t, fmq::SynchronizedReadWrite>();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Session::isReconfigurationRequired(const device::CameraMetadata&, const device::CameraMetadata&,
                                                      bool* _aidl_return) {
    // The session parameters change nothing on a host camera.
    *_aidl_return = false;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Session::processCaptureRequest(const std::vector<device::CaptureRequest>& in_requests,
                                                  const std::vector<device::BufferCache>& in_cachesToRemove,
                                                  int32_t* _aidl_return) {
    std::lock_guard<std::mutex> lock(mLock);
    *_aidl_return = 0;
    if (mClosed) {
        return error(common::Status::INTERNAL_ERROR);
    }
    for (const device::BufferCache& cache : in_cachesToRemove) {
        auto it = mBuffers.find({cache.streamId, cache.bufferId});
        if (it != mBuffers.end()) {
            AHardwareBuffer_release(it->second);
            mBuffers.erase(it);
        }
    }
    for (const device::CaptureRequest& request : in_requests) {
        Pending pending;
        pending.frameNumber = request.frameNumber;
        if (!request.settings.metadata.empty()) {
            bool valid = false;
            Metadata settings = Metadata::fromBytes(request.settings.metadata, &valid);
            if (!valid) {
                CAMERA_LOGE("request %d carries settings that are not camera metadata", request.frameNumber);
                return error(common::Status::ILLEGAL_ARGUMENT);
            }
            mSettings = settings;
        }
        if (mSettings.empty()) {
            CAMERA_LOGE("the first request %d carries no settings", request.frameNumber);
            return error(common::Status::ILLEGAL_ARGUMENT);
        }
        if (request.inputBuffer.streamId != -1 && request.inputBuffer.bufferId != 0) {
            return error(common::Status::ILLEGAL_ARGUMENT);
        }
        pending.settings = mSettings;
        for (const device::StreamBuffer& out : request.outputBuffers) {
            auto stream = mStreams.find(out.streamId);
            if (stream == mStreams.end()) {
                return error(common::Status::ILLEGAL_ARGUMENT);
            }
            const auto key = std::make_pair(out.streamId, out.bufferId);
            auto known = mBuffers.find(key);
            if (known == mBuffers.end()) {
                HandleParts parts;
                for (const ndk::ScopedFileDescriptor& fd : out.buffer.fds) {
                    parts.fds.push_back(fd.get());
                }
                parts.ints = out.buffer.ints;
                const StreamInfo& info = stream->second;
                const bool still = info.format == static_cast<int32_t>(graphics::PixelFormat::BLOB);
                AHardwareBuffer* imported =
                    importBuffer(parts, still ? static_cast<uint32_t>(info.bufferSize) : info.width,
                                 still ? 1u : info.height, static_cast<uint32_t>(info.format), info.usage);
                if (imported == nullptr) {
                    CAMERA_LOGE("buffer %lld of stream %d is not a graphics buffer",
                                static_cast<long long>(out.bufferId), out.streamId);
                    return error(common::Status::ILLEGAL_ARGUMENT);
                }
                known = mBuffers.emplace(key, imported).first;
            }
            Output output;
            output.streamId = out.streamId;
            output.bufferId = out.bufferId;
            output.buffer = known->second;
            AHardwareBuffer_acquire(output.buffer);
            if (!out.acquireFence.fds.empty()) {
                output.acquireFence = dup(out.acquireFence.fds[0].get());
            }
            pending.outputs.push_back(output);
        }
        mQueue.push_back(std::move(pending));
        ++*_aidl_return;
    }
    mChanged.notify_all();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Session::signalStreamFlush(const std::vector<int32_t>&, int32_t) {
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Session::switchToOffline(const std::vector<int32_t>&, device::CameraOfflineSessionInfo*,
                                            std::shared_ptr<device::ICameraOfflineSession>*) {
    return error(common::Status::OPERATION_NOT_SUPPORTED);
}

ndk::ScopedAStatus Session::repeatingRequestEnd(int32_t, const std::vector<int32_t>&) {
    return ndk::ScopedAStatus::ok();
}

void Session::work() {
    for (;;) {
        Pending pending;
        {
            std::unique_lock<std::mutex> lock(mLock);
            mChanged.wait(lock, [&] { return mClosed || !mQueue.empty(); });
            if (mClosed && mQueue.empty()) {
                return;
            }
            pending = std::move(mQueue.front());
            mQueue.pop_front();
            ++mInFlight;
        }
        bool flushing = false;
        {
            std::lock_guard<std::mutex> lock(mLock);
            flushing = mFlushing;
        }
        if (flushing) {
            fail(pending, true);
        } else {
            capture(pending);
        }
        std::lock_guard<std::mutex> lock(mLock);
        --mInFlight;
        mChanged.notify_all();
    }
}

void Session::fail(Pending& pending, bool notify) {
    if (notify) {
        device::ErrorMsg failed;
        failed.frameNumber = pending.frameNumber;
        failed.errorStreamId = -1;
        failed.errorCode = device::ErrorCode::ERROR_REQUEST;
        mCallback->notify({device::NotifyMsg::make<device::NotifyMsg::error>(failed)});
    }
    device::CaptureResult result = emptyResult(pending.frameNumber);
    for (Output& output : pending.outputs) {
        waitFence(output.acquireFence, 0);
        output.acquireFence = -1;
        result.outputBuffers.push_back(resultBuffer(output.streamId, output.bufferId, false));
        AHardwareBuffer_release(output.buffer);
    }
    pending.outputs.clear();
    deliver(std::move(result));
}

void Session::capture(Pending& pending) {
    std::shared_ptr<const Picture> picture;
    std::string why;
    {
        std::lock_guard<std::mutex> host(mHostLock);
        // The app's frame rate, when it asks for one the camera has.
        std::vector<int32_t> range;
        if (pending.settings.getAll(ANDROID_CONTROL_AE_TARGET_FPS_RANGE, &range) && range.size() == 2 &&
            range[1] > 0 && static_cast<uint32_t>(range[1]) != mHostFps) {
            for (const auto& offered : fpsRanges(mCamera)) {
                if (offered.second == range[1]) {
                    std::string error;
                    if (!startHost(static_cast<uint32_t>(range[1]), &error)) {
                        CAMERA_LOGW("%s did not change its rate: %s", mCamera.name.c_str(), error.c_str());
                    }
                    break;
                }
            }
        }
        if (!mHost && !startHost(mHostFps == 0 ? 30 : mHostFps, &why)) {
            CAMERA_LOGE("%s is not running on the host: %s", mCamera.name.c_str(), why.c_str());
        } else if (mHost && mHost->nextPicture(mLastPicture, kPictureWait, &picture, &why)) {
            mLastPicture = picture->sequence;
        } else if (mHost) {
            CAMERA_LOGW("%s: %s", mCamera.name.c_str(), why.c_str());
            // A camera that failed is started again for the next request.
            mHost->close();
            mHost.reset();
        }
    }
    if (!picture) {
        fail(pending, true);
        return;
    }

    const int64_t timestamp = monotonicNs();
    device::ShutterMsg shutter;
    shutter.frameNumber = pending.frameNumber;
    shutter.timestamp = timestamp;
    shutter.readoutTimestamp = timestamp;
    mCallback->notify({device::NotifyMsg::make<device::NotifyMsg::shutter>(shutter)});

    std::map<int32_t, StreamInfo> streams;
    {
        std::lock_guard<std::mutex> lock(mLock);
        streams = mStreams;
    }
    uint8_t quality = 95;
    pending.settings.get(ANDROID_JPEG_QUALITY, &quality);
    const Nv12 nv12{reinterpret_cast<const uint8_t*>(picture->nv12.data()), picture->nv12.size(), picture->width,
                    picture->height, picture->stride};

    device::CaptureResult result = emptyResult(pending.frameNumber);
    for (Output& output : pending.outputs) {
        bool ok = waitFence(output.acquireFence, kFenceWaitMs);
        output.acquireFence = -1;
        auto stream = streams.find(output.streamId);
        std::string error;
        if (!ok) {
            error = "the buffer's fence did not signal";
        } else if (stream == streams.end()) {
            ok = false;
            error = "the stream is gone";
        } else if (stream->second.format == static_cast<int32_t>(graphics::PixelFormat::BLOB)) {
            std::string jpeg;
            {
                std::lock_guard<std::mutex> host(mHostLock);
                ok = mHost && mHost->still(stream->second.width, stream->second.height, quality, &jpeg, &error);
            }
            ok = ok && fillStill(output.buffer, stream->second.bufferSize, jpeg, &error);
        } else {
            ok = fillPicture(output.buffer, nv12, stream->second.width, stream->second.height, &error);
        }
        if (!ok) {
            CAMERA_LOGW("frame %d, stream %d: %s", pending.frameNumber, output.streamId, error.c_str());
            device::ErrorMsg failed;
            failed.frameNumber = pending.frameNumber;
            failed.errorStreamId = output.streamId;
            failed.errorCode = device::ErrorCode::ERROR_BUFFER;
            mCallback->notify({device::NotifyMsg::make<device::NotifyMsg::error>(failed)});
        }
        result.outputBuffers.push_back(resultBuffer(output.streamId, output.bufferId, ok));
        AHardwareBuffer_release(output.buffer);
    }
    pending.outputs.clear();

    const int64_t frameDuration = mHostFps > 0 ? 1000000000LL / mHostFps : 33333333LL;
    result.result.metadata = captureResult(pending.settings, timestamp, frameDuration).bytes();
    result.partialResult = 1;
    deliver(std::move(result));
}

void Session::deliver(device::CaptureResult result) {
    // A result holds handles, which are moved, never copied.
    std::vector<device::CaptureResult> results;
    results.push_back(std::move(result));
    const ndk::ScopedAStatus status = mCallback->processCaptureResult(results);
    if (!status.isOk()) {
        CAMERA_LOGW("the camera service did not take a result: %s", status.getDescription().c_str());
    }
}

}  // namespace dsa::camera
