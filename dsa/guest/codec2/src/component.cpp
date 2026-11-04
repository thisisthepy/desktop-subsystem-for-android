// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "component.h"

#include <poll.h>
#include <sys/mman.h>
#include <unistd.h>

#include <atomic>
#include <chrono>
#include <cstring>

#include <android/hardware_buffer_aidl.h>

#include <C2ParamDef.h>
#include <SimpleC2Interface.h>
#include <codec2/aidl/ParamTypes.h>

#include "log.h"
#include "picture_copy.h"

namespace dsa::codec2 {

using ::aidl::android::hardware::HardwareBuffer;
using ::aidl::android::hardware::common::NativeHandle;
using c2aidl::BaseBlock;
using c2aidl::Block;
using c2aidl::Buffer;
using c2aidl::FrameData;
using c2aidl::IComponentListener;
using c2aidl::IGraphicBufferAllocator;
using c2aidl::Params;
using c2aidl::Status;
using c2aidl::Work;
using c2aidl::WorkBundle;
using c2aidl::Worklet;
using c2aidl::utils::CachedConfigurable;
using c2aidl::utils::ConfigurableC2Intf;
using c2aidl::utils::CreateParamsBlob;
using c2aidl::utils::ParameterCache;
using c2aidl::utils::ParseParamsBlob;

namespace {

// The metadata of a block, as the framework's HAL client writes and reads it.
struct C2Hal_Range {
    uint32_t offset;
    uint32_t length;
};
typedef C2GlobalParam<C2Info, C2Hal_Range, 0> C2Hal_RangeInfo;

struct C2Hal_Rect {
    uint32_t left;
    uint32_t top;
    uint32_t width;
    uint32_t height;
};
typedef C2GlobalParam<C2Info, C2Hal_Rect, 1> C2Hal_RectInfo;

// C2PlatformAllocatorStore::IGBA: the framework hands its surface's buffers through an
// IGraphicBufferAllocator.
constexpr int32_t kAllocatorIgba = 0x14;
// AHARDWAREBUFFER_FORMAT_Y8Cb8Cr8_420, HAL_PIXEL_FORMAT_YCBCR_420_888.
constexpr uint32_t kFormatYuv420 = 0x23;
// How long a picture waits for a buffer of the app's surface before it is dropped.
constexpr auto kAllocateWait = std::chrono::seconds(2);
// The status IGraphicBufferAllocator::allocate fails with while every buffer is in use.
constexpr int32_t kBlocking = -9930;

ndk::ScopedAStatus status(c2_status_t error) { return ndk::ScopedAStatus::fromServiceSpecificError(error); }

ndk::ScopedAStatus unsupported() { return ndk::ScopedAStatus::fromExceptionCode(EX_UNSUPPORTED_OPERATION); }

// The C2 interface the framework reaches through IConfigurable.
struct CompIntf : public ConfigurableC2Intf {
    explicit CompIntf(const std::shared_ptr<C2ComponentInterface>& intf)
        : ConfigurableC2Intf(intf->getName(), intf->getId()), mIntf(intf) {}

    c2_status_t query(const std::vector<C2Param::Index>& indices, c2_blocking_t mayBlock,
                      std::vector<std::unique_ptr<C2Param>>* const params) const override {
        return mIntf->query_vb({}, indices, mayBlock, params);
    }
    c2_status_t config(const std::vector<C2Param*>& params, c2_blocking_t mayBlock,
                       std::vector<std::unique_ptr<C2SettingResult>>* const failures) override {
        return mIntf->config_vb(params, mayBlock, failures);
    }
    c2_status_t querySupportedParams(std::vector<std::shared_ptr<C2ParamDescriptor>>* const params) const override {
        return mIntf->querySupportedParams_nb(params);
    }
    c2_status_t querySupportedValues(std::vector<C2FieldSupportedValuesQuery>& fields,
                                     c2_blocking_t mayBlock) const override {
        return mIntf->querySupportedValues_vb(fields, mayBlock);
    }

    std::shared_ptr<C2ComponentInterface> mIntf;
};

struct NoCache : public ParameterCache {
    c2_status_t validate(const std::vector<std::shared_ptr<C2ParamDescriptor>>&) override { return C2_OK; }
};

// The bytes of a work's input: its first block, a buffer of the framework's mapped here.
bool readInput(const WorkBundle& bundle, const FrameData& input, std::vector<uint8_t>* data) {
    data->clear();
    if (input.buffers.empty() || input.buffers[0].blocks.empty()) {
        return true;
    }
    const Block& block = input.buffers[0].blocks[0];
    if (block.index < 0 || static_cast<size_t>(block.index) >= bundle.baseBlocks.size()) {
        return false;
    }
    const BaseBlock& base = bundle.baseBlocks[block.index];
    if (base.getTag() != BaseBlock::nativeBlock) {
        CODEC_LOGE("an input buffer came from a buffer pool; the guest image disables those for codecs");
        return false;
    }
    const NativeHandle& handle = base.get<BaseBlock::nativeBlock>();
    if (handle.fds.empty()) {
        return false;
    }
    uint32_t offset = 0;
    uint32_t length = 0;
    std::vector<C2Param*> meta;
    if (ParseParamsBlob(&meta, block.meta)) {
        for (C2Param* param : meta) {
            if (param && param->index() == C2Hal_RangeInfo::PARAM_TYPE) {
                const auto* range = static_cast<const C2Hal_RangeInfo*>(param);
                offset = range->offset;
                length = range->length;
            }
        }
    }
    if (length == 0) {
        return true;
    }
    const int fd = handle.fds[0].get();
    const size_t mapped = static_cast<size_t>(offset) + length;
    void* memory = mmap(nullptr, mapped, PROT_READ, MAP_SHARED, fd, 0);
    if (memory == MAP_FAILED) {
        CODEC_LOGE("cannot map an input buffer of %zu bytes", mapped);
        return false;
    }
    const auto* bytes = static_cast<const uint8_t*>(memory);
    data->assign(bytes + offset, bytes + mapped);
    munmap(memory, mapped);
    return true;
}

Work workFor(const c2aidl::WorkOrdinal& ordinal, int32_t inputFlags) {
    Work work;
    work.input.ordinal = ordinal;
    work.input.flags = inputFlags;
    work.result.status = Status::OK;
    return work;
}

bool waitFd(int fd, std::chrono::milliseconds timeout) {
    if (fd < 0) {
        return true;
    }
    pollfd p{fd, POLLIN, 0};
    return poll(&p, 1, static_cast<int>(timeout.count())) > 0;
}

}  // namespace

std::shared_ptr<c2aidl::IConfigurable> makeConfigurable(const std::shared_ptr<C2ComponentInterface>& intf) {
    auto configurable = ndk::SharedRefBase::make<CachedConfigurable>(std::make_unique<CompIntf>(intf));
    configurable->init(std::make_shared<NoCache>());
    return configurable;
}

ComponentInterface::ComponentInterface(const std::shared_ptr<C2ComponentInterface>& intf)
    : mConfigurable(makeConfigurable(intf)) {}

ndk::ScopedAStatus ComponentInterface::getConfigurable(std::shared_ptr<c2aidl::IConfigurable>* configurable) {
    *configurable = mConfigurable;
    return ndk::ScopedAStatus::ok();
}

std::shared_ptr<C2ComponentInterface> makeDecoderInterface(const std::shared_ptr<C2ReflectorHelper>& reflector,
                                                           const DecoderInfo& info, const CodecSupport& support,
                                                           std::shared_ptr<DecoderIntf>* params) {
    *params = std::make_shared<DecoderIntf>(reflector, info, support.maxWidth, support.maxHeight);
    static std::atomic<c2_node_id_t> nextId{1};
    return std::make_shared<android::SimpleInterface<DecoderIntf>>(info.name, nextId++, *params);
}

Component::Component(const DecoderInfo& info, const std::shared_ptr<C2ComponentInterface>& intf,
                     const std::shared_ptr<DecoderIntf>& params, const std::shared_ptr<IComponentListener>& listener)
    : mInfo(info),
      mIntf(intf),
      mParams(params),
      mListener(listener),
      mInterface(ndk::SharedRefBase::make<ComponentInterface>(intf)) {}

Component::~Component() { closeHost(); }

void Component::closeHost() {
    std::unique_ptr<HostDecoder> host;
    {
        std::lock_guard<std::mutex> lock(mLock);
        host = std::move(mHost);
    }
    if (host) {
        host->close();
    }
}

ndk::ScopedAStatus Component::configureVideoTunnel(int32_t, NativeHandle*) { return unsupported(); }

ndk::ScopedAStatus Component::createBlockPool(const BlockPoolAllocator& allocator, BlockPool* pool) {
    Pool entry;
    entry.allocatorId = allocator.allocatorId;
    if (allocator.allocatorId == kAllocatorIgba && allocator.gbAllocator) {
        entry.igba = allocator.gbAllocator->igba;
        entry.waitableFd = allocator.gbAllocator->waitableFd.dup();
    }
    std::lock_guard<std::mutex> lock(mLock);
    const int64_t id = mNextPoolId++;
    mPools[id] = std::move(entry);
    pool->blockPoolId = id;
    pool->configurable = makeConfigurable(mIntf);
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Component::destroyBlockPool(int64_t blockPoolId) {
    std::lock_guard<std::mutex> lock(mLock);
    if (mPools.erase(blockPoolId) == 0) {
        return status(C2_NOT_FOUND);
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Component::start() {
    std::lock_guard<std::mutex> lock(mLock);
    if (mHost) {
        return ndk::ScopedAStatus::ok();
    }
    std::string error;
    std::weak_ptr<Component> self = ref<Component>();
    mHost = HostDecoder::open(
        mInfo.codec, mParams->width(), mParams->height(),
        [self](const HostMessage& message) {
            if (auto component = self.lock()) {
                component->onHostMessage(message);
            }
        },
        &error);
    if (!mHost) {
        CODEC_LOGE("%s: no decoder on the host: %s", mInfo.name, error.c_str());
        return status(C2_NO_INIT);
    }
    mWidth = 0;
    mHeight = 0;
    CODEC_LOGI("%s: decoding %ux%u on the host's GPU", mInfo.name, mParams->width(), mParams->height());
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Component::queue(const WorkBundle& bundle) {
    for (const Work& work : bundle.works) {
        const FrameData& input = work.input;
        const bool eos = (input.flags & FrameData::END_OF_STREAM) != 0;
        const bool config = (input.flags & FrameData::CODEC_CONFIG) != 0;
        std::vector<uint8_t> data;
        if (!readInput(bundle, input, &data)) {
            return status(C2_BAD_VALUE);
        }
        Work pending = workFor(input.ordinal, input.flags);
        const uint64_t frameIndex = static_cast<uint64_t>(input.ordinal.frameIndex);
        HostDecoder* host = nullptr;
        {
            std::lock_guard<std::mutex> lock(mLock);
            host = mHost.get();
            if (host && !data.empty() && !config) {
                mPending[frameIndex] = std::move(pending);
            } else if (host && eos) {
                mPending[frameIndex] = std::move(pending);
            }
        }
        if (host == nullptr) {
            return status(C2_BAD_STATE);
        }
        if (!data.empty() || eos) {
            if (!host->send(encodePacket(frameIndex, input.ordinal.timestampUs, data.data(), data.size(), config,
                                         eos))) {
                CODEC_LOGE("%s: the host's decoder is gone", mInfo.name);
                Status corrupted;
                corrupted.status = C2_CORRUPTED;
                mListener->onError(corrupted, C2_CORRUPTED);
                return status(C2_CORRUPTED);
            }
        }
        // The input is copied out: the framework may reuse its buffer now.
        if (!input.buffers.empty()) {
            IComponentListener::InputBuffer released;
            released.frameIndex = input.ordinal.frameIndex;
            released.arrayIndex = 0;
            mListener->onInputBuffersReleased({released});
        }
        // Codec configuration and empty inputs have no picture of their own.
        if (config || (data.empty() && !eos)) {
            finish(workFor(input.ordinal, input.flags), nullptr, 0, 0, false);
        }
    }
    return ndk::ScopedAStatus::ok();
}

void Component::onHostMessage(const HostMessage& message) {
    if (const auto* frame = std::get_if<Frame>(&message)) {
        if (frame->endOfStream) {
            endOfStream(*frame);
        } else {
            picture(*frame);
        }
    } else if (std::holds_alternative<Flushed>(message)) {
        std::lock_guard<std::mutex> lock(mLock);
        mFlushDone = true;
        mFlushed.notify_all();
    } else if (const auto* error = std::get_if<DecodeError>(&message)) {
        decodeError(error->error);
    }
}

AHardwareBuffer* Component::allocate(uint32_t width, uint32_t height) {
    std::shared_ptr<IGraphicBufferAllocator> igba;
    int waitable = -1;
    {
        std::lock_guard<std::mutex> lock(mLock);
        auto pool = mPools.find(mParams->outputPoolId());
        if (pool != mPools.end() && pool->second.igba) {
            igba = pool->second.igba;
            waitable = pool->second.waitableFd.get();
        }
    }
    const uint64_t usage = mParams->outputUsage() | AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN |
                           AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE;
    if (igba) {
        IGraphicBufferAllocator::Description desc;
        desc.width = static_cast<int32_t>(width);
        desc.height = static_cast<int32_t>(height);
        desc.format = static_cast<int32_t>(kFormatYuv420);
        desc.usage = static_cast<int64_t>(usage);
        const auto deadline = std::chrono::steady_clock::now() + kAllocateWait;
        while (std::chrono::steady_clock::now() < deadline) {
            IGraphicBufferAllocator::Allocation allocation;
            const ndk::ScopedAStatus result = igba->allocate(desc, &allocation);
            if (result.isOk()) {
                waitFd(allocation.fence.get(), std::chrono::milliseconds(1000));
                return allocation.buffer.release();
            }
            if (result.getServiceSpecificError() != kBlocking) {
                CODEC_LOGE("%s: the surface gave no buffer: %s", mInfo.name, result.getDescription().c_str());
                return nullptr;
            }
            // Every buffer of the surface is shown or queued; one comes back soon.
            waitFd(waitable, std::chrono::milliseconds(100));
        }
        CODEC_LOGW("%s: no buffer of the surface came back in time; a picture is dropped", mInfo.name);
        return nullptr;
    }
    AHardwareBuffer_Desc desc{};
    desc.width = width;
    desc.height = height;
    desc.layers = 1;
    desc.format = kFormatYuv420;
    desc.usage = usage | AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN;
    AHardwareBuffer* buffer = nullptr;
    if (AHardwareBuffer_allocate(&desc, &buffer) != 0) {
        CODEC_LOGE("%s: cannot allocate a %ux%u picture buffer", mInfo.name, width, height);
        return nullptr;
    }
    return buffer;
}

void Component::picture(const Frame& frame) {
    std::optional<Work> work;
    {
        std::lock_guard<std::mutex> lock(mLock);
        auto pending = mPending.find(frame.frameIndex);
        if (pending != mPending.end()) {
            work = std::move(pending->second);
            mPending.erase(pending);
        }
    }
    if (!work) {
        // A picture of a work already finished, or flushed: it goes out as a work of its own.
        c2aidl::WorkOrdinal ordinal;
        ordinal.frameIndex = static_cast<int64_t>(frame.frameIndex);
        ordinal.timestampUs = frame.timestampUs;
        work = workFor(ordinal, 0);
    }
    AHardwareBuffer* buffer = allocate(frame.width, frame.height);
    if (buffer != nullptr) {
        AHardwareBuffer_Planes planes{};
        if (AHardwareBuffer_lockPlanes(buffer, AHARDWAREBUFFER_USAGE_CPU_WRITE_OFTEN, -1, nullptr, &planes) == 0) {
            YuvPlanes yuv;
            yuv.y = Plane{static_cast<uint8_t*>(planes.planes[0].data), planes.planes[0].pixelStride,
                          planes.planes[0].rowStride};
            yuv.u = Plane{static_cast<uint8_t*>(planes.planes[1].data), planes.planes[1].pixelStride,
                          planes.planes[1].rowStride};
            yuv.v = Plane{static_cast<uint8_t*>(planes.planes[2].data), planes.planes[2].pixelStride,
                          planes.planes[2].rowStride};
            if (planes.planeCount < 3 ||
                !copyNv12(reinterpret_cast<const uint8_t*>(frame.nv12.data()), frame.nv12.size(), frame.width,
                          frame.height, frame.stride, yuv)) {
                CODEC_LOGE("%s: a %ux%u picture did not fit its buffer", mInfo.name, frame.width, frame.height);
            }
            AHardwareBuffer_unlock(buffer, nullptr);
        } else {
            CODEC_LOGE("%s: cannot write a picture buffer", mInfo.name);
        }
    }
    finish(std::move(*work), buffer, frame.width, frame.height, false);
    if (buffer != nullptr) {
        AHardwareBuffer_release(buffer);
    }
}

void Component::endOfStream(const Frame& frame) {
    std::map<uint64_t, Work> left;
    {
        std::lock_guard<std::mutex> lock(mLock);
        left.swap(mPending);
    }
    // Works whose pictures never came (the decoder dropped them) finish empty; the last
    // carries the end of the stream.
    Work eos;
    bool haveEos = false;
    for (auto& [index, work] : left) {
        if (index == frame.frameIndex && (work.input.flags & FrameData::END_OF_STREAM) != 0) {
            eos = std::move(work);
            haveEos = true;
        } else {
            finish(std::move(work), nullptr, 0, 0, false);
        }
    }
    if (!haveEos) {
        c2aidl::WorkOrdinal ordinal;
        ordinal.frameIndex = static_cast<int64_t>(frame.frameIndex);
        ordinal.timestampUs = frame.timestampUs;
        eos = workFor(ordinal, FrameData::END_OF_STREAM);
    }
    finish(std::move(eos), nullptr, 0, 0, true);
}

void Component::decodeError(const std::string& error) {
    CODEC_LOGW("%s: %s", mInfo.name, error.c_str());
    // The oldest work waiting is the likeliest to have lost its picture; it finishes empty,
    // so the framework's pipeline keeps moving.
    std::optional<Work> oldest;
    {
        std::lock_guard<std::mutex> lock(mLock);
        if (!mPending.empty()) {
            oldest = std::move(mPending.begin()->second);
            mPending.erase(mPending.begin());
        }
    }
    if (oldest) {
        finish(std::move(*oldest), nullptr, 0, 0, false);
    }
}

void Component::finish(Work work, AHardwareBuffer* output, uint32_t width, uint32_t height, bool eos) {
    WorkBundle bundle;
    Worklet worklet;
    worklet.componentId = 0;
    worklet.output.ordinal = work.input.ordinal;
    worklet.output.flags = eos ? FrameData::END_OF_STREAM : 0;
    if (output != nullptr) {
        // A new size goes to the framework with the first picture of that size.
        bool resized = false;
        {
            std::lock_guard<std::mutex> lock(mLock);
            if (width != mWidth || height != mHeight) {
                mWidth = width;
                mHeight = height;
                resized = true;
            }
        }
        if (resized) {
            C2StreamPictureSizeInfo::output size(0u, width, height);
            std::vector<std::unique_ptr<C2SettingResult>> failures;
            mIntf->config_vb({&size}, C2_MAY_BLOCK, &failures);
            CreateParamsBlob(&worklet.output.configUpdate, std::vector<C2Param*>{&size});
        }
        HardwareBuffer hardwareBuffer;
        AHardwareBuffer_acquire(output);
        hardwareBuffer.reset(output);
        bundle.baseBlocks.push_back(BaseBlock::make<BaseBlock::hwbBlock>(std::move(hardwareBuffer)));
        Block block;
        block.index = 0;
        C2Hal_RectInfo crop;
        crop.left = 0;
        crop.top = 0;
        crop.width = width;
        crop.height = height;
        CreateParamsBlob(&block.meta, std::vector<C2Param*>{&crop});
        Buffer buffer;
        buffer.blocks.push_back(std::move(block));
        worklet.output.buffers.push_back(std::move(buffer));
    }
    work.worklets.clear();
    work.worklets.push_back(std::move(worklet));
    work.workletsProcessed = 1;
    work.result.status = Status::OK;
    bundle.works.push_back(std::move(work));
    const ndk::ScopedAStatus result = mListener->onWorkDone(bundle);
    if (!result.isOk()) {
        CODEC_LOGW("%s: the framework did not take a finished work: %s", mInfo.name,
                   result.getDescription().c_str());
    }
}

ndk::ScopedAStatus Component::flush(WorkBundle* flushed) {
    HostDecoder* host = nullptr;
    {
        std::unique_lock<std::mutex> lock(mLock);
        host = mHost.get();
        mFlushDone = false;
    }
    if (host != nullptr && host->send(encodeFlush())) {
        std::unique_lock<std::mutex> lock(mLock);
        if (!mFlushed.wait_for(lock, std::chrono::seconds(2), [this] { return mFlushDone; })) {
            CODEC_LOGW("%s: the host did not answer a flush", mInfo.name);
        }
    }
    std::map<uint64_t, Work> left;
    {
        std::lock_guard<std::mutex> lock(mLock);
        left.swap(mPending);
    }
    for (auto& [index, work] : left) {
        work.workletsProcessed = 0;
        flushed->works.push_back(std::move(work));
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Component::drain(bool withEos) {
    if (!withEos) {
        return ndk::ScopedAStatus::ok();
    }
    HostDecoder* host = nullptr;
    {
        std::lock_guard<std::mutex> lock(mLock);
        host = mHost.get();
    }
    if (host == nullptr || !host->send(encodePacket(0, 0, nullptr, 0, false, true))) {
        return status(C2_BAD_STATE);
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Component::getInterface(std::shared_ptr<c2aidl::IComponentInterface>* intf) {
    *intf = mInterface;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Component::stop() {
    closeHost();
    std::lock_guard<std::mutex> lock(mLock);
    mPending.clear();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Component::reset() {
    closeHost();
    std::lock_guard<std::mutex> lock(mLock);
    mPending.clear();
    mPools.clear();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Component::release() { return reset(); }

ndk::ScopedAStatus Component::connectToInputSurface(const std::shared_ptr<c2aidl::IInputSurface>&,
                                                    std::shared_ptr<c2aidl::IInputSurfaceConnection>*) {
    return unsupported();
}

ndk::ScopedAStatus Component::asInputSink(std::shared_ptr<c2aidl::IInputSink>*) { return unsupported(); }

}  // namespace dsa::codec2
