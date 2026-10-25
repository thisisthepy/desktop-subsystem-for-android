// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// IComposerClient for SurfaceFlinger: one display, whose frames TaskComposer splits into
// per-task frames for the host.

#pragma once

#include <functional>
#include <memory>
#include <mutex>

#include <aidl/android/hardware/graphics/composer3/BnComposerClient.h>

#include "display.h"
#include "task_composer.h"
#include "vsync_thread.h"

namespace dsa::hwc {

class ComposerClient : public composer3::BnComposerClient {
   public:
    ComposerClient(std::shared_ptr<TaskComposer> tasks, const DisplayConfig& config);
    ~ComposerClient() override;

    void setOnDestroyed(std::function<void()> onDestroyed) { mOnDestroyed = std::move(onDestroyed); }
    // Asks SurfaceFlinger for a new frame of the display.
    void requestRefresh();

    using common_Dataspace = aidl::android::hardware::graphics::common::Dataspace;
    using common_PixelFormat = aidl::android::hardware::graphics::common::PixelFormat;
    using common_Transform = aidl::android::hardware::graphics::common::Transform;

    ndk::ScopedAStatus createLayer(int64_t display, int32_t bufferSlotCount, int64_t* layer) override;
    ndk::ScopedAStatus createVirtualDisplay(int32_t width, int32_t height, common_PixelFormat formatHint,
                                            int32_t outputBufferSlotCount,
                                            composer3::VirtualDisplay* display) override;
    ndk::ScopedAStatus destroyLayer(int64_t display, int64_t layer) override;
    ndk::ScopedAStatus destroyVirtualDisplay(int64_t display) override;
    ndk::ScopedAStatus executeCommands(const std::vector<composer3::DisplayCommand>& commands,
                                       std::vector<composer3::CommandResultPayload>* results) override;
    ndk::ScopedAStatus getActiveConfig(int64_t display, int32_t* config) override;
    ndk::ScopedAStatus getColorModes(int64_t display, std::vector<composer3::ColorMode>* modes) override;
    ndk::ScopedAStatus getDataspaceSaturationMatrix(common_Dataspace dataspace,
                                                    std::vector<float>* matrix) override;
    ndk::ScopedAStatus getDisplayAttribute(int64_t display, int32_t config,
                                           composer3::DisplayAttribute attribute, int32_t* value) override;
    ndk::ScopedAStatus getDisplayCapabilities(int64_t display,
                                              std::vector<composer3::DisplayCapability>* caps) override;
    ndk::ScopedAStatus getDisplayConfigs(int64_t display, std::vector<int32_t>* configs) override;
    ndk::ScopedAStatus getDisplayConnectionType(int64_t display,
                                                composer3::DisplayConnectionType* type) override;
    ndk::ScopedAStatus getDisplayIdentificationData(int64_t display,
                                                    composer3::DisplayIdentification* id) override;
    ndk::ScopedAStatus getDisplayName(int64_t display, std::string* name) override;
    ndk::ScopedAStatus getDisplayVsyncPeriod(int64_t display, int32_t* period) override;
    ndk::ScopedAStatus getDisplayedContentSample(int64_t display, int64_t maxFrames, int64_t timestamp,
                                                 composer3::DisplayContentSample* samples) override;
    ndk::ScopedAStatus getDisplayedContentSamplingAttributes(
        int64_t display, composer3::DisplayContentSamplingAttributes* attrs) override;
    ndk::ScopedAStatus getDisplayPhysicalOrientation(int64_t display, common_Transform* orientation) override;
    ndk::ScopedAStatus getHdrCapabilities(int64_t display, composer3::HdrCapabilities* caps) override;
    ndk::ScopedAStatus getMaxVirtualDisplayCount(int32_t* count) override;
    ndk::ScopedAStatus getPerFrameMetadataKeys(int64_t display,
                                               std::vector<composer3::PerFrameMetadataKey>* keys) override;
    ndk::ScopedAStatus getReadbackBufferAttributes(int64_t display,
                                                   composer3::ReadbackBufferAttributes* attrs) override;
    ndk::ScopedAStatus getReadbackBufferFence(int64_t display, ndk::ScopedFileDescriptor* fence) override;
    ndk::ScopedAStatus getRenderIntents(int64_t display, composer3::ColorMode mode,
                                        std::vector<composer3::RenderIntent>* intents) override;
    ndk::ScopedAStatus getSupportedContentTypes(int64_t display,
                                                std::vector<composer3::ContentType>* types) override;
    ndk::ScopedAStatus getDisplayDecorationSupport(
        int64_t display,
        std::optional<aidl::android::hardware::graphics::common::DisplayDecorationSupport>* support) override;
    ndk::ScopedAStatus registerCallback(const std::shared_ptr<composer3::IComposerCallback>& callback) override;
    ndk::ScopedAStatus setActiveConfig(int64_t display, int32_t config) override;
    ndk::ScopedAStatus setActiveConfigWithConstraints(
        int64_t display, int32_t config, const composer3::VsyncPeriodChangeConstraints& constraints,
        composer3::VsyncPeriodChangeTimeline* timeline) override;
    ndk::ScopedAStatus setBootDisplayConfig(int64_t display, int32_t config) override;
    ndk::ScopedAStatus clearBootDisplayConfig(int64_t display) override;
    ndk::ScopedAStatus getPreferredBootDisplayConfig(int64_t display, int32_t* config) override;
    ndk::ScopedAStatus setAutoLowLatencyMode(int64_t display, bool on) override;
    ndk::ScopedAStatus setClientTargetSlotCount(int64_t display, int32_t count) override;
    ndk::ScopedAStatus setColorMode(int64_t display, composer3::ColorMode mode,
                                    composer3::RenderIntent intent) override;
    ndk::ScopedAStatus setContentType(int64_t display, composer3::ContentType type) override;
    ndk::ScopedAStatus setDisplayedContentSamplingEnabled(int64_t display, bool enable,
                                                          composer3::FormatColorComponent componentMask,
                                                          int64_t maxFrames) override;
    ndk::ScopedAStatus setPowerMode(int64_t display, composer3::PowerMode mode) override;
    ndk::ScopedAStatus setReadbackBuffer(int64_t display,
                                         const aidl::android::hardware::common::NativeHandle& buffer,
                                         const ndk::ScopedFileDescriptor& releaseFence) override;
    ndk::ScopedAStatus setVsyncEnabled(int64_t display, bool enabled) override;
    ndk::ScopedAStatus setIdleTimerEnabled(int64_t display, int32_t timeoutMs) override;
    ndk::ScopedAStatus getOverlaySupport(composer3::OverlayProperties* properties) override;
    ndk::ScopedAStatus getHdrConversionCapabilities(
        std::vector<aidl::android::hardware::graphics::common::HdrConversionCapability>* caps) override;
    ndk::ScopedAStatus setHdrConversionStrategy(
        const aidl::android::hardware::graphics::common::HdrConversionStrategy& strategy,
        aidl::android::hardware::graphics::common::Hdr* preferred) override;
    ndk::ScopedAStatus setRefreshRateChangedCallbackDebugEnabled(int64_t display, bool enabled) override;
    ndk::ScopedAStatus getDisplayConfigurations(int64_t display, int32_t maxFrameIntervalNs,
                                                std::vector<composer3::DisplayConfiguration>* configs) override;
    ndk::ScopedAStatus notifyExpectedPresent(int64_t display,
                                             const composer3::ClockMonotonicTimestamp& expectedPresentTime,
                                             int32_t frameIntervalNs) override;
    ndk::ScopedAStatus getMaxLayerPictureProfiles(int64_t display, int32_t* maxProfiles) override;
    ndk::ScopedAStatus startHdcpNegotiation(int64_t display,
                                            const aidl::android::hardware::drm::HdcpLevels& levels) override;
    ndk::ScopedAStatus getLuts(int64_t display, const std::vector<composer3::Buffer>& buffers,
                               std::vector<composer3::Luts>* luts) override;

   private:
    void executeDisplayCommand(const composer3::DisplayCommand& command, int32_t index,
                               std::vector<composer3::CommandResultPayload>* results);
    void present(int64_t displayId, std::vector<composer3::CommandResultPayload>* results);
    std::shared_ptr<composer3::IComposerCallback> callback();

    std::shared_ptr<TaskComposer> mTasks;
    std::mutex mMutex;
    Display mDisplay;
    // Read by the vsync thread too, which must not wait for a frame to be composed.
    std::mutex mCallbackMutex;
    std::shared_ptr<composer3::IComposerCallback> mCallback;
    std::unique_ptr<VsyncThread> mVsync;
    std::function<void()> mOnDestroyed;
};

}  // namespace dsa::hwc
