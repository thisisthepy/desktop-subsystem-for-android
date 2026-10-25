// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "composer_client.h"

#include "log.h"

namespace dsa::hwc {

namespace common = aidl::android::hardware::graphics::common;
using composer3::CommandResultPayload;

namespace {

// The one display SurfaceFlinger sees, and its one configuration.
constexpr int64_t kDisplayId = 0;
constexpr int32_t kConfigId = 0;

ndk::ScopedAStatus error(int32_t code) { return ndk::ScopedAStatus::fromServiceSpecificError(code); }

#define CHECK_DISPLAY(display)                                       \
    do {                                                             \
        if ((display) != kDisplayId) {                               \
            return error(composer3::IComposerClient::EX_BAD_DISPLAY); \
        }                                                            \
    } while (0)

void addError(std::vector<CommandResultPayload>* results, int32_t index, int32_t code) {
    composer3::CommandError e;
    e.commandIndex = index;
    e.errorCode = code;
    results->emplace_back(e);
}

}  // namespace

ComposerClient::ComposerClient(std::shared_ptr<TaskComposer> tasks, const DisplayConfig& config)
    : mTasks(std::move(tasks)), mDisplay(config) {
    mVsync = std::make_unique<VsyncThread>(config.vsyncPeriodNanos, [this](int64_t timestamp) {
        if (auto cb = callback()) {
            cb->onVsync(kDisplayId, timestamp, mDisplay.config().vsyncPeriodNanos);
        }
    });
}

ComposerClient::~ComposerClient() {
    mVsync.reset();
    if (mOnDestroyed) {
        mOnDestroyed();
    }
}

std::shared_ptr<composer3::IComposerCallback> ComposerClient::callback() {
    std::lock_guard<std::mutex> lock(mCallbackMutex);
    return mCallback;
}

void ComposerClient::requestRefresh() {
    if (auto cb = callback()) {
        cb->onRefresh(kDisplayId);
    }
}

ndk::ScopedAStatus ComposerClient::registerCallback(
    const std::shared_ptr<composer3::IComposerCallback>& cb) {
    bool first;
    {
        std::lock_guard<std::mutex> lock(mCallbackMutex);
        first = mCallback == nullptr;
        mCallback = cb;
    }
    if (first && cb) {
        cb->onHotplug(kDisplayId, /*connected=*/true);
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::createLayer(int64_t display, int32_t /*bufferSlotCount*/,
                                               int64_t* layer) {
    CHECK_DISPLAY(display);
    std::lock_guard<std::mutex> lock(mMutex);
    *layer = mDisplay.createLayer();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::destroyLayer(int64_t display, int64_t layer) {
    CHECK_DISPLAY(display);
    std::lock_guard<std::mutex> lock(mMutex);
    if (!mDisplay.destroyLayer(layer)) {
        return error(composer3::IComposerClient::EX_BAD_LAYER);
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::createVirtualDisplay(int32_t, int32_t, common_PixelFormat, int32_t,
                                                        composer3::VirtualDisplay*) {
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::destroyVirtualDisplay(int64_t) {
    return error(composer3::IComposerClient::EX_BAD_DISPLAY);
}

ndk::ScopedAStatus ComposerClient::executeCommands(
    const std::vector<composer3::DisplayCommand>& commands,
    std::vector<CommandResultPayload>* results) {
    std::lock_guard<std::mutex> lock(mMutex);
    int32_t index = 0;
    for (const auto& command : commands) {
        executeDisplayCommand(command, index++, results);
    }
    return ndk::ScopedAStatus::ok();
}

void ComposerClient::executeDisplayCommand(const composer3::DisplayCommand& command, int32_t index,
                                           std::vector<CommandResultPayload>* results) {
    if (command.display != kDisplayId) {
        addError(results, index, composer3::IComposerClient::EX_BAD_DISPLAY);
        return;
    }
    for (const auto& layerCommand : command.layers) {
        using composer3::LayerLifecycleBatchCommandType;
        if (layerCommand.layerLifecycleBatchCommandType == LayerLifecycleBatchCommandType::DESTROY) {
            mDisplay.destroyLayer(layerCommand.layer);
            continue;
        }
        Layer* layer = mDisplay.layer(layerCommand.layer);
        if (!layer) {
            addError(results, index, composer3::IComposerClient::EX_BAD_LAYER);
            continue;
        }
        mDisplay.applyLayerCommand(*layer, layerCommand);
    }
    if (command.clientTarget) {
        mDisplay.setClientTarget(command.clientTarget->buffer);
    }
    if (command.virtualDisplayOutputBuffer) {
        addError(results, index, composer3::IComposerClient::EX_UNSUPPORTED);
    }
    if (command.validateDisplay || command.presentOrValidateDisplay) {
        const auto changes = mDisplay.validate();
        if (!changes.empty()) {
            composer3::ChangedCompositionTypes changed;
            changed.display = kDisplayId;
            changed.layers = changes;
            results->emplace_back(changed);
        }
        if (command.presentOrValidateDisplay) {
            // Always validated, never presented straight away: SurfaceFlinger then accepts
            // the changes and presents.
            composer3::PresentOrValidate result;
            result.display = kDisplayId;
            result.result = composer3::PresentOrValidate::Result::Validated;
            results->emplace_back(result);
        }
    }
    if (command.acceptDisplayChanges) {
        mDisplay.acceptChanges();
    }
    if (command.presentDisplay) {
        if (!mDisplay.validated()) {
            addError(results, index, composer3::IComposerClient::EX_NOT_VALIDATED);
            return;
        }
        present(kDisplayId, results);
    }
}

void ComposerClient::present(int64_t displayId, std::vector<CommandResultPayload>* results) {
    Frame frame = mDisplay.takeFrame();
    if (mDisplay.powerMode() != composer3::PowerMode::OFF) {
        mTasks->present(frame);
    }
    // The host has finished with every buffer by now, so there are no fences to hand back:
    // an empty release list tells SurfaceFlinger each buffer is free already.
    composer3::ReleaseFences releases;
    releases.display = displayId;
    results->emplace_back(std::move(releases));
}

ndk::ScopedAStatus ComposerClient::getActiveConfig(int64_t display, int32_t* config) {
    CHECK_DISPLAY(display);
    *config = kConfigId;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getColorModes(int64_t display, std::vector<composer3::ColorMode>* modes) {
    CHECK_DISPLAY(display);
    *modes = {composer3::ColorMode::NATIVE};
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDataspaceSaturationMatrix(common_Dataspace dataspace,
                                                                std::vector<float>* matrix) {
    if (dataspace != common::Dataspace::SRGB_LINEAR) {
        return error(composer3::IComposerClient::EX_BAD_PARAMETER);
    }
    *matrix = {1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1};
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayAttribute(int64_t display, int32_t config,
                                                       composer3::DisplayAttribute attribute,
                                                       int32_t* value) {
    CHECK_DISPLAY(display);
    if (config != kConfigId) {
        return error(composer3::IComposerClient::EX_BAD_CONFIG);
    }
    const DisplayConfig& c = mDisplay.config();
    switch (attribute) {
        case composer3::DisplayAttribute::WIDTH:
            *value = c.width;
            break;
        case composer3::DisplayAttribute::HEIGHT:
            *value = c.height;
            break;
        case composer3::DisplayAttribute::VSYNC_PERIOD:
            *value = c.vsyncPeriodNanos;
            break;
        case composer3::DisplayAttribute::DPI_X:
        case composer3::DisplayAttribute::DPI_Y:
            // Dots per thousand inches.
            *value = c.dpi * 1000;
            break;
        case composer3::DisplayAttribute::CONFIG_GROUP:
            *value = 0;
            break;
        default:
            return error(composer3::IComposerClient::EX_BAD_PARAMETER);
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayCapabilities(int64_t display,
                                                          std::vector<composer3::DisplayCapability>* caps) {
    CHECK_DISPLAY(display);
    caps->clear();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayConfigs(int64_t display, std::vector<int32_t>* configs) {
    CHECK_DISPLAY(display);
    *configs = {kConfigId};
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayConfigurations(
    int64_t display, int32_t, std::vector<composer3::DisplayConfiguration>* configs) {
    CHECK_DISPLAY(display);
    const DisplayConfig& c = mDisplay.config();
    composer3::DisplayConfiguration config;
    config.configId = kConfigId;
    config.width = c.width;
    config.height = c.height;
    config.dpi = composer3::DisplayConfiguration::Dpi{static_cast<float>(c.dpi),
                                                      static_cast<float>(c.dpi)};
    config.configGroup = 0;
    config.vsyncPeriod = c.vsyncPeriodNanos;
    *configs = {config};
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayConnectionType(int64_t display,
                                                            composer3::DisplayConnectionType* type) {
    CHECK_DISPLAY(display);
    *type = composer3::DisplayConnectionType::INTERNAL;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayIdentificationData(int64_t display,
                                                                composer3::DisplayIdentification*) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getDisplayName(int64_t display, std::string* name) {
    CHECK_DISPLAY(display);
    *name = "DSA desktop";
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayVsyncPeriod(int64_t display, int32_t* period) {
    CHECK_DISPLAY(display);
    *period = mDisplay.config().vsyncPeriodNanos;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayedContentSample(int64_t display, int64_t, int64_t,
                                                             composer3::DisplayContentSample*) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getDisplayedContentSamplingAttributes(
    int64_t display, composer3::DisplayContentSamplingAttributes*) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getDisplayPhysicalOrientation(int64_t display,
                                                                 common_Transform* orientation) {
    CHECK_DISPLAY(display);
    *orientation = common::Transform::NONE;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getHdrCapabilities(int64_t display, composer3::HdrCapabilities* caps) {
    CHECK_DISPLAY(display);
    *caps = composer3::HdrCapabilities{};
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getMaxVirtualDisplayCount(int32_t* count) {
    *count = 0;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getPerFrameMetadataKeys(int64_t display,
                                                           std::vector<composer3::PerFrameMetadataKey>*) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getReadbackBufferAttributes(int64_t display,
                                                               composer3::ReadbackBufferAttributes*) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getReadbackBufferFence(int64_t display, ndk::ScopedFileDescriptor*) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getRenderIntents(int64_t display, composer3::ColorMode mode,
                                                    std::vector<composer3::RenderIntent>* intents) {
    CHECK_DISPLAY(display);
    if (mode != composer3::ColorMode::NATIVE) {
        return error(composer3::IComposerClient::EX_BAD_PARAMETER);
    }
    *intents = {composer3::RenderIntent::COLORIMETRIC};
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getSupportedContentTypes(int64_t display,
                                                            std::vector<composer3::ContentType>* types) {
    CHECK_DISPLAY(display);
    types->clear();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::getDisplayDecorationSupport(
    int64_t display, std::optional<common::DisplayDecorationSupport>* support) {
    CHECK_DISPLAY(display);
    support->reset();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setActiveConfig(int64_t display, int32_t config) {
    CHECK_DISPLAY(display);
    if (config != kConfigId) {
        return error(composer3::IComposerClient::EX_BAD_CONFIG);
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setActiveConfigWithConstraints(
    int64_t display, int32_t config, const composer3::VsyncPeriodChangeConstraints& constraints,
    composer3::VsyncPeriodChangeTimeline* timeline) {
    CHECK_DISPLAY(display);
    if (config != kConfigId) {
        return error(composer3::IComposerClient::EX_BAD_CONFIG);
    }
    timeline->newVsyncAppliedTimeNanos = constraints.desiredTimeNanos;
    timeline->refreshRequired = false;
    timeline->refreshTimeNanos = 0;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setBootDisplayConfig(int64_t display, int32_t) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::clearBootDisplayConfig(int64_t display) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getPreferredBootDisplayConfig(int64_t display, int32_t*) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::setAutoLowLatencyMode(int64_t display, bool) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::setClientTargetSlotCount(int64_t display, int32_t) {
    CHECK_DISPLAY(display);
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setColorMode(int64_t display, composer3::ColorMode mode,
                                                composer3::RenderIntent intent) {
    CHECK_DISPLAY(display);
    if (mode != composer3::ColorMode::NATIVE || intent != composer3::RenderIntent::COLORIMETRIC) {
        return error(composer3::IComposerClient::EX_UNSUPPORTED);
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setContentType(int64_t display, composer3::ContentType type) {
    CHECK_DISPLAY(display);
    if (type != composer3::ContentType::NONE) {
        return error(composer3::IComposerClient::EX_UNSUPPORTED);
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setDisplayedContentSamplingEnabled(int64_t display, bool,
                                                                      composer3::FormatColorComponent,
                                                                      int64_t) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::setPowerMode(int64_t display, composer3::PowerMode mode) {
    CHECK_DISPLAY(display);
    if (mode == composer3::PowerMode::DOZE || mode == composer3::PowerMode::DOZE_SUSPEND ||
        mode == composer3::PowerMode::ON_SUSPEND) {
        return error(composer3::IComposerClient::EX_UNSUPPORTED);
    }
    std::lock_guard<std::mutex> lock(mMutex);
    mDisplay.setPowerMode(mode);
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setReadbackBuffer(int64_t display,
                                                     const aidl::android::hardware::common::NativeHandle&,
                                                     const ndk::ScopedFileDescriptor&) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::setVsyncEnabled(int64_t display, bool enabled) {
    CHECK_DISPLAY(display);
    mVsync->setEnabled(enabled);
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setIdleTimerEnabled(int64_t display, int32_t) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getOverlaySupport(composer3::OverlayProperties*) {
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getHdrConversionCapabilities(
    std::vector<common::HdrConversionCapability>* caps) {
    caps->clear();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus ComposerClient::setHdrConversionStrategy(const common::HdrConversionStrategy& strategy,
                                                            common::Hdr* preferred) {
    using Tag = common::HdrConversionStrategy::Tag;
    if (strategy.getTag() == Tag::autoAllowedHdrTypes &&
        strategy.get<Tag::autoAllowedHdrTypes>().empty()) {
        *preferred = common::Hdr::INVALID;
        return ndk::ScopedAStatus::ok();
    }
    if (strategy.getTag() == Tag::passthrough) {
        *preferred = common::Hdr::INVALID;
        return ndk::ScopedAStatus::ok();
    }
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::setRefreshRateChangedCallbackDebugEnabled(int64_t display, bool) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::notifyExpectedPresent(int64_t display,
                                                         const composer3::ClockMonotonicTimestamp&,
                                                         int32_t) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getMaxLayerPictureProfiles(int64_t display, int32_t*) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::startHdcpNegotiation(int64_t display,
                                                        const aidl::android::hardware::drm::HdcpLevels&) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

ndk::ScopedAStatus ComposerClient::getLuts(int64_t display, const std::vector<composer3::Buffer>&,
                                           std::vector<composer3::Luts>*) {
    CHECK_DISPLAY(display);
    return error(composer3::IComposerClient::EX_UNSUPPORTED);
}

}  // namespace dsa::hwc
