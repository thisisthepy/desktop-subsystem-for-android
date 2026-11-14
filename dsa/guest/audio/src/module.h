// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The primary audio module (IModule/default): the shared output, the speaker, which is the
// host's default output device, an output for each app with a window (app_outputs.h), and
// one input, the built-in microphone, which is the host's default capture device. Each has
// one mix port of 48 kHz stereo 16-bit sound, the format the host's audio port carries, so
// the framework converts everything else on its side. There is no external device.
//
// The framework learns the module's ports and routes from here, builds audio policy from
// them, and then asks for a configuration of each mix port, a patch from it to its device,
// and a stream on the configuration. The volume is the host's: every output device has a
// gain for volume, so audio policy gives the stream volume to the device as its gain and
// leaves the mixer at full scale. The module keeps that gain and never applies it, and it
// has no master volume or mute.

#pragma once

#include <aidl/android/hardware/audio/core/BnModule.h>

#include <atomic>
#include <map>
#include <memory>
#include <mutex>
#include <vector>

namespace dsa::audio {

namespace core = ::aidl::android::hardware::audio::core;
namespace media = ::aidl::android::media::audio::common;

class StreamIn;
class StreamOut;

// The module's fixed ports and the IDs it hands out.
constexpr int32_t kMixPortId = 1;
constexpr int32_t kSpeakerPortId = 2;
// The speaker's own configuration, which always exists, has the port's ID.
constexpr int32_t kSpeakerConfigId = kSpeakerPortId;
// The input: its mix port, and the microphone with its own configuration.
constexpr int32_t kInputMixPortId = 3;
constexpr int32_t kMicPortId = 4;
constexpr int32_t kMicConfigId = kMicPortId;
// IDs of configurations the framework asks for start here.
constexpr int32_t kFirstConfigId = 100;

constexpr int32_t kSampleRate = 48000;
// The frames the mixer writes at a time: 20 ms. The host's queue is sized for it.
constexpr int32_t kBurstFrames = 960;

class Module final : public core::BnModule {
   public:
    Module();

    ndk::ScopedAStatus setModuleDebug(const core::ModuleDebug& in_debug) override;
    ndk::ScopedAStatus getTelephony(std::shared_ptr<core::ITelephony>* _aidl_return) override;
    ndk::ScopedAStatus getBluetooth(std::shared_ptr<core::IBluetooth>* _aidl_return) override;
    ndk::ScopedAStatus getBluetoothA2dp(std::shared_ptr<core::IBluetoothA2dp>* _aidl_return) override;
    ndk::ScopedAStatus getBluetoothLe(std::shared_ptr<core::IBluetoothLe>* _aidl_return) override;
    ndk::ScopedAStatus connectExternalDevice(const media::AudioPort& in_templateIdAndAdditionalData,
                                             media::AudioPort* _aidl_return) override;
    ndk::ScopedAStatus disconnectExternalDevice(int32_t in_portId) override;
    ndk::ScopedAStatus getAudioPatches(std::vector<core::AudioPatch>* _aidl_return) override;
    ndk::ScopedAStatus getAudioPort(int32_t in_portId, media::AudioPort* _aidl_return) override;
    ndk::ScopedAStatus getAudioPortConfigs(std::vector<media::AudioPortConfig>* _aidl_return) override;
    ndk::ScopedAStatus getAudioPorts(std::vector<media::AudioPort>* _aidl_return) override;
    ndk::ScopedAStatus getAudioRoutes(std::vector<core::AudioRoute>* _aidl_return) override;
    ndk::ScopedAStatus getAudioRoutesForAudioPort(int32_t in_portId,
                                                  std::vector<core::AudioRoute>* _aidl_return) override;
    ndk::ScopedAStatus openInputStream(const core::IModule::OpenInputStreamArguments& in_args,
                                       core::IModule::OpenInputStreamReturn* _aidl_return) override;
    ndk::ScopedAStatus openOutputStream(const core::IModule::OpenOutputStreamArguments& in_args,
                                        core::IModule::OpenOutputStreamReturn* _aidl_return) override;
    ndk::ScopedAStatus getSupportedPlaybackRateFactors(
            core::IModule::SupportedPlaybackRateFactors* _aidl_return) override;
    ndk::ScopedAStatus setAudioPatch(const core::AudioPatch& in_requested,
                                     core::AudioPatch* _aidl_return) override;
    ndk::ScopedAStatus setAudioPortConfig(const media::AudioPortConfig& in_requested,
                                          media::AudioPortConfig* out_suggested,
                                          bool* _aidl_return) override;
    ndk::ScopedAStatus resetAudioPatch(int32_t in_patchId) override;
    ndk::ScopedAStatus resetAudioPortConfig(int32_t in_portConfigId) override;
    ndk::ScopedAStatus getMasterMute(bool* _aidl_return) override;
    ndk::ScopedAStatus setMasterMute(bool in_mute) override;
    ndk::ScopedAStatus getMasterVolume(float* _aidl_return) override;
    ndk::ScopedAStatus setMasterVolume(float in_volume) override;
    ndk::ScopedAStatus getMicMute(bool* _aidl_return) override;
    ndk::ScopedAStatus setMicMute(bool in_mute) override;
    ndk::ScopedAStatus getMicrophones(std::vector<media::MicrophoneInfo>* _aidl_return) override;
    ndk::ScopedAStatus updateAudioMode(media::AudioMode in_mode) override;
    ndk::ScopedAStatus updateScreenRotation(core::IModule::ScreenRotation in_rotation) override;
    ndk::ScopedAStatus updateScreenState(bool in_isTurnedOn) override;
    ndk::ScopedAStatus getSoundDose(
            std::shared_ptr<core::sounddose::ISoundDose>* _aidl_return) override;
    ndk::ScopedAStatus generateHwAvSyncId(int32_t* _aidl_return) override;
    ndk::ScopedAStatus getVendorParameters(const std::vector<std::string>& in_ids,
                                           std::vector<core::VendorParameter>* _aidl_return) override;
    ndk::ScopedAStatus setVendorParameters(const std::vector<core::VendorParameter>& in_parameters,
                                           bool in_async) override;
    ndk::ScopedAStatus addDeviceEffect(
            int32_t in_portConfigId,
            const std::shared_ptr<::aidl::android::hardware::audio::effect::IEffect>& in_effect) override;
    ndk::ScopedAStatus removeDeviceEffect(
            int32_t in_portConfigId,
            const std::shared_ptr<::aidl::android::hardware::audio::effect::IEffect>& in_effect) override;
    ndk::ScopedAStatus getMmapPolicyInfos(
            media::AudioMMapPolicyType in_mmapPolicyType,
            std::vector<media::AudioMMapPolicyInfo>* _aidl_return) override;
    ndk::ScopedAStatus supportsVariableLatency(bool* _aidl_return) override;
    ndk::ScopedAStatus getAAudioMixerBurstCount(int32_t* _aidl_return) override;
    ndk::ScopedAStatus getAAudioHardwareBurstMinUsec(int32_t* _aidl_return) override;
    ndk::ScopedAStatus prepareToDisconnectExternalDevice(int32_t in_portId) override;
    ndk::ScopedAStatus getFlushFromFrameSupport(const media::AudioPortConfig& in_config,
                                                media::FlushFromFrameSupport* _aidl_return) override;

   private:
    const media::AudioPort* findPort(int32_t id) const;
    // Whether `portId` is the mix port of an output: the shared one or an app's.
    static bool isOutputMix(int32_t portId);
    // The device output mix port `mixPortId` plays on, or 0 for none.
    static int32_t deviceOfMix(int32_t mixPortId);
    // Whether a stream plays on mix port `portId` now.
    bool streamOnPort(int32_t portId);
    std::vector<media::AudioPortConfig>::iterator findConfig(int32_t id);
    bool configInUse(int32_t id) const;
    void streamClosed(int32_t portConfigId);
    void inputClosed(int32_t portConfigId);

    std::mutex mLock;
    std::vector<media::AudioPort> mPorts;
    std::vector<core::AudioRoute> mRoutes;
    std::vector<media::AudioPortConfig> mConfigs;
    std::vector<core::AudioPatch> mPatches;
    // The open streams, by the mix port configuration they play.
    std::map<int32_t, std::weak_ptr<StreamOut>> mStreams;
    // The open input streams, by the mix port configuration they record.
    std::map<int32_t, std::weak_ptr<StreamIn>> mInputs;
    int32_t mNextConfigId = kFirstConfigId;
    int32_t mNextPatchId = 1;
    // Read by the input stream's thread, which never takes the module's lock.
    std::shared_ptr<std::atomic<bool>> mMicMute = std::make_shared<std::atomic<bool>>(false);
};

}  // namespace dsa::audio
