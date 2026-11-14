// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "module.h"

#include <aidl/android/media/audio/common/AudioGainMode.h>
#include <aidl/android/media/audio/common/AudioOutputFlags.h>
#include <android/binder_ibinder.h>
#include <android/binder_ibinder_platform.h>
#include <sched.h>

#include <algorithm>
#include <string>

#include "app_outputs.h"
#include "log.h"
#include "stream_in.h"
#include "stream_out.h"

namespace dsa::audio {

using media::AudioChannelLayout;
using media::AudioDeviceType;
using media::AudioFormatDescription;
using media::AudioFormatType;
using media::AudioGain;
using media::AudioGainMode;
using media::AudioIoFlags;
using media::AudioOutputFlags;
using media::AudioPort;
using media::AudioPortConfig;
using media::AudioPortDeviceExt;
using media::AudioPortExt;
using media::AudioPortMixExt;
using media::AudioProfile;
using media::PcmType;

namespace {

// The platform's ANDROID_PRIORITY_AUDIO, the least the stream's binder calls run at.
constexpr int kAudioPriority = -16;

ndk::ScopedAStatus error(int32_t code) { return ndk::ScopedAStatus::fromExceptionCode(code); }

AudioFormatDescription pcm16() {
    AudioFormatDescription format;
    format.type = AudioFormatType::PCM;
    format.pcm = PcmType::INT_16_BIT;
    return format;
}

AudioChannelLayout stereo() {
    return AudioChannelLayout::make<AudioChannelLayout::layoutMask>(AudioChannelLayout::LAYOUT_STEREO);
}

media::Int hostRate() {
    media::Int rate;
    rate.value = kSampleRate;
    return rate;
}

AudioProfile hostProfile() {
    AudioProfile profile;
    profile.format = pcm16();
    profile.channelMasks = {stereo()};
    profile.sampleRates = {kSampleRate};
    return profile;
}

AudioIoFlags outputFlags(int32_t flags) { return AudioIoFlags::make<AudioIoFlags::Tag::output>(flags); }

AudioIoFlags inputFlags(int32_t flags) { return AudioIoFlags::make<AudioIoFlags::Tag::input>(flags); }

AudioPortExt speakerExt() {
    AudioPortDeviceExt device;
    device.device.type.type = AudioDeviceType::OUT_SPEAKER;
    device.flags = 1 << AudioPortDeviceExt::FLAG_INDEX_DEFAULT_DEVICE;
    return AudioPortExt::make<AudioPortExt::Tag::device>(device);
}

// An app output's bus device, at the address the agent routes the app's uid to.
AudioPortExt busExt(int32_t slot) {
    AudioPortDeviceExt device;
    device.device.type.type = AudioDeviceType::OUT_BUS;
    device.device.address =
            media::AudioDeviceAddress::make<media::AudioDeviceAddress::Tag::id>(appDeviceAddress(slot));
    return AudioPortExt::make<AudioPortExt::Tag::device>(device);
}

// The gain every output device has, which audio policy sets the volume as, so its mixer stays
// at full scale and the host's volume is the only one the sound has.
AudioGain volumeGain() {
    AudioGain gain;
    gain.mode = 1 << static_cast<int32_t>(AudioGainMode::JOINT);
    gain.minValue = kGainMinMb;
    gain.maxValue = kGainMaxMb;
    gain.defaultValue = kGainMaxMb;
    gain.stepValue = kGainStepMb;
    gain.useForVolume = true;
    return gain;
}

// Whether `requested` is a gain the device port `port` takes: one value in range, by its one
// gain's index and mode.
bool takesGain(const AudioPort& port, const media::AudioGainConfig& requested) {
    if (port.gains.empty() || requested.index != 0 || requested.values.empty()) return false;
    if ((requested.mode & port.gains[0].mode) == 0) return false;
    for (int32_t value : requested.values) {
        if (!gainInRange(value)) return false;
    }
    return true;
}

AudioPortExt micExt() {
    AudioPortDeviceExt device;
    device.device.type.type = AudioDeviceType::IN_MICROPHONE;
    device.flags = 1 << AudioPortDeviceExt::FLAG_INDEX_DEFAULT_DEVICE;
    return AudioPortExt::make<AudioPortExt::Tag::device>(device);
}

// Whether the fields `requested` sets match the host's one format.
bool matchesHostFormat(const AudioPortConfig& requested) {
    if (requested.format.has_value() && requested.format.value() != pcm16()) return false;
    if (requested.channelMask.has_value() && requested.channelMask.value() != stereo()) return false;
    if (requested.sampleRate.has_value() && requested.sampleRate.value().value != kSampleRate) {
        return false;
    }
    return true;
}

bool fullySpecified(const AudioPortConfig& requested) {
    return requested.format.has_value() && requested.channelMask.has_value() &&
           requested.sampleRate.has_value() && requested.flags.has_value();
}

}  // namespace

Module::Module() {
    AudioPort mix;
    mix.id = kMixPortId;
    mix.name = "primary output";
    mix.profiles = {hostProfile()};
    mix.flags = outputFlags(1 << static_cast<int32_t>(AudioOutputFlags::PRIMARY));
    AudioPortMixExt mixExt;
    mixExt.maxOpenStreamCount = 1;
    mixExt.maxActiveStreamCount = 1;
    mix.ext = AudioPortExt::make<AudioPortExt::Tag::mix>(mixExt);

    AudioPort speaker;
    speaker.id = kSpeakerPortId;
    speaker.name = "Speaker";
    speaker.profiles = {hostProfile()};
    speaker.flags = outputFlags(0);
    speaker.gains = {volumeGain()};
    speaker.ext = speakerExt();

    // The input: the microphone records into its mix port.
    AudioPort inputMix;
    inputMix.id = kInputMixPortId;
    inputMix.name = "primary input";
    inputMix.profiles = {hostProfile()};
    inputMix.flags = inputFlags(0);
    AudioPortMixExt inputMixExt;
    inputMixExt.maxOpenStreamCount = 1;
    inputMixExt.maxActiveStreamCount = 1;
    inputMix.ext = AudioPortExt::make<AudioPortExt::Tag::mix>(inputMixExt);

    AudioPort mic;
    mic.id = kMicPortId;
    mic.name = "Built-In Mic";
    mic.profiles = {hostProfile()};
    mic.flags = inputFlags(0);
    mic.ext = micExt();

    mPorts = {mix, speaker, inputMix, mic};

    core::AudioRoute route;
    route.sourcePortIds = {kMixPortId};
    route.sinkPortId = kSpeakerPortId;
    core::AudioRoute inputRoute;
    inputRoute.sourcePortIds = {kMicPortId};
    inputRoute.sinkPortId = kInputMixPortId;
    mRoutes = {route, inputRoute};

    // An output for each app with a window: a mix port and a bus device of its own, which the
    // agent routes the app to. Each is its own stream on the host, played as its app.
    std::vector<AudioPort> busDevices;
    for (int32_t slot = 0; slot < kAppOutputs; ++slot) {
        AudioPort appMix;
        appMix.id = appMixPortId(slot);
        appMix.name = streamName(appMix.id) + " output";
        appMix.profiles = {hostProfile()};
        appMix.flags = outputFlags(0);
        AudioPortMixExt appMixExt;
        appMixExt.maxOpenStreamCount = 1;
        appMixExt.maxActiveStreamCount = 1;
        appMix.ext = AudioPortExt::make<AudioPortExt::Tag::mix>(appMixExt);

        AudioPort bus;
        bus.id = appDevicePortId(slot);
        bus.name = appDeviceAddress(slot);
        bus.profiles = {hostProfile()};
        bus.flags = outputFlags(0);
        bus.gains = {volumeGain()};
        bus.ext = busExt(slot);

        core::AudioRoute appRoute;
        appRoute.sourcePortIds = {appMix.id};
        appRoute.sinkPortId = bus.id;
        mRoutes.push_back(appRoute);
        mPorts.push_back(appMix);
        mPorts.push_back(bus);
        busDevices.push_back(bus);
    }

    AudioPortConfig speakerConfig;
    speakerConfig.id = kSpeakerConfigId;
    speakerConfig.portId = kSpeakerPortId;
    speakerConfig.format = pcm16();
    speakerConfig.channelMask = stereo();
    speakerConfig.sampleRate = hostRate();
    speakerConfig.flags = speaker.flags;
    speakerConfig.ext = speaker.ext;
    AudioPortConfig micConfig = speakerConfig;
    micConfig.id = kMicConfigId;
    micConfig.portId = kMicPortId;
    micConfig.flags = mic.flags;
    micConfig.ext = mic.ext;
    mConfigs = {speakerConfig, micConfig};
    // Each bus device's own configuration, like the speaker's, has the port's ID.
    for (const auto& bus : busDevices) {
        AudioPortConfig busConfig = speakerConfig;
        busConfig.id = bus.id;
        busConfig.portId = bus.id;
        busConfig.flags = bus.flags;
        busConfig.ext = bus.ext;
        mConfigs.push_back(busConfig);
    }
}

bool Module::isOutputMix(int32_t portId) {
    return portId == kMixPortId || appSlotOfMixPort(portId) >= 0;
}

int32_t Module::deviceOfMix(int32_t mixPortId) {
    if (mixPortId == kMixPortId) return kSpeakerPortId;
    const int32_t slot = appSlotOfMixPort(mixPortId);
    return slot < 0 ? 0 : appDevicePortId(slot);
}

bool Module::streamOnPort(int32_t portId) {
    for (const auto& entry : mStreams) {
        auto config = findConfig(entry.first);
        if (config != mConfigs.end() && config->portId == portId) return true;
    }
    return false;
}

const AudioPort* Module::findPort(int32_t id) const {
    for (const auto& port : mPorts) {
        if (port.id == id) return &port;
    }
    return nullptr;
}

std::vector<AudioPortConfig>::iterator Module::findConfig(int32_t id) {
    return std::find_if(mConfigs.begin(), mConfigs.end(),
                        [id](const AudioPortConfig& c) { return c.id == id; });
}

bool Module::configInUse(int32_t id) const {
    if (mStreams.count(id) != 0 || mInputs.count(id) != 0) return true;
    for (const auto& patch : mPatches) {
        const auto& sources = patch.sourcePortConfigIds;
        const auto& sinks = patch.sinkPortConfigIds;
        if (std::find(sources.begin(), sources.end(), id) != sources.end() ||
            std::find(sinks.begin(), sinks.end(), id) != sinks.end()) {
            return true;
        }
    }
    return false;
}

void Module::streamClosed(int32_t portConfigId) {
    std::lock_guard<std::mutex> lock(mLock);
    mStreams.erase(portConfigId);
}

void Module::inputClosed(int32_t portConfigId) {
    std::lock_guard<std::mutex> lock(mLock);
    mInputs.erase(portConfigId);
}

ndk::ScopedAStatus Module::setModuleDebug(const core::ModuleDebug&) { return ndk::ScopedAStatus::ok(); }

ndk::ScopedAStatus Module::getTelephony(std::shared_ptr<core::ITelephony>* _aidl_return) {
    *_aidl_return = nullptr;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getBluetooth(std::shared_ptr<core::IBluetooth>* _aidl_return) {
    *_aidl_return = nullptr;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getBluetoothA2dp(std::shared_ptr<core::IBluetoothA2dp>* _aidl_return) {
    *_aidl_return = nullptr;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getBluetoothLe(std::shared_ptr<core::IBluetoothLe>* _aidl_return) {
    *_aidl_return = nullptr;
    return ndk::ScopedAStatus::ok();
}

// The speaker is always there; there is nothing to connect.
ndk::ScopedAStatus Module::connectExternalDevice(const AudioPort&, AudioPort*) {
    return error(EX_ILLEGAL_ARGUMENT);
}

ndk::ScopedAStatus Module::disconnectExternalDevice(int32_t) { return error(EX_ILLEGAL_ARGUMENT); }

ndk::ScopedAStatus Module::prepareToDisconnectExternalDevice(int32_t) {
    return error(EX_ILLEGAL_ARGUMENT);
}

ndk::ScopedAStatus Module::getAudioPatches(std::vector<core::AudioPatch>* _aidl_return) {
    std::lock_guard<std::mutex> lock(mLock);
    *_aidl_return = mPatches;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getAudioPort(int32_t in_portId, AudioPort* _aidl_return) {
    const AudioPort* port = findPort(in_portId);
    if (port == nullptr) return error(EX_ILLEGAL_ARGUMENT);
    *_aidl_return = *port;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getAudioPortConfigs(std::vector<AudioPortConfig>* _aidl_return) {
    std::lock_guard<std::mutex> lock(mLock);
    *_aidl_return = mConfigs;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getAudioPorts(std::vector<AudioPort>* _aidl_return) {
    *_aidl_return = mPorts;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getAudioRoutes(std::vector<core::AudioRoute>* _aidl_return) {
    *_aidl_return = mRoutes;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getAudioRoutesForAudioPort(int32_t in_portId,
                                                      std::vector<core::AudioRoute>* _aidl_return) {
    if (findPort(in_portId) == nullptr) return error(EX_ILLEGAL_ARGUMENT);
    _aidl_return->clear();
    for (const auto& route : mRoutes) {
        const auto& sources = route.sourcePortIds;
        if (route.sinkPortId == in_portId ||
            std::find(sources.begin(), sources.end(), in_portId) != sources.end()) {
            _aidl_return->push_back(route);
        }
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::openInputStream(const core::IModule::OpenInputStreamArguments& in_args,
                                           core::IModule::OpenInputStreamReturn* _aidl_return) {
    std::lock_guard<std::mutex> lock(mLock);
    auto config = findConfig(in_args.portConfigId);
    if (config == mConfigs.end() || config->portId != kInputMixPortId) {
        AUDIO_LOGE("openInputStream: %d is not a configuration of the input", in_args.portConfigId);
        return error(EX_ILLEGAL_ARGUMENT);
    }
    if (!mInputs.empty()) {
        AUDIO_LOGE("openInputStream: the input already has a stream");
        return error(EX_ILLEGAL_STATE);
    }
    if (in_args.bufferSizeFrames < kBurstFrames) {
        AUDIO_LOGE("openInputStream: a buffer of %lld frames is under the %d the input needs",
                   static_cast<long long>(in_args.bufferSizeFrames), kBurstFrames);
        return error(EX_ILLEGAL_ARGUMENT);
    }
    const int64_t bufferFrames = std::min<int64_t>(in_args.bufferSizeFrames, kSampleRate);
    std::weak_ptr<Module> self = ref<Module>();
    std::shared_ptr<std::atomic<bool>> muted = mMicMute;
    std::string why;
    // Opening the stream opens the host's capture device, so the device is open only while
    // a stream is.
    auto stream = StreamIn::create(
            in_args.portConfigId, bufferFrames, [muted] { return muted->load(); },
            [self](int32_t id) {
                if (auto module = self.lock()) module->inputClosed(id);
            },
            &_aidl_return->desc, &why);
    if (stream == nullptr) {
        // The host lets no program record, or has no microphone: the app's recording fails.
        AUDIO_LOGE("openInputStream: the host does not record: %s", why.c_str());
        return error(EX_ILLEGAL_STATE);
    }
    ndk::SpAIBinder binder = stream->asBinder();
    AIBinder_setMinSchedulerPolicy(binder.get(), SCHED_NORMAL, kAudioPriority);
    AIBinder_setInheritRt(binder.get(), true);
    mInputs[in_args.portConfigId] = stream;
    _aidl_return->stream = stream;
    AUDIO_LOGI("opened the input stream on configuration %d, %lld frames", in_args.portConfigId,
               static_cast<long long>(bufferFrames));
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::openOutputStream(const core::IModule::OpenOutputStreamArguments& in_args,
                                            core::IModule::OpenOutputStreamReturn* _aidl_return) {
    std::lock_guard<std::mutex> lock(mLock);
    auto config = findConfig(in_args.portConfigId);
    if (config == mConfigs.end() || !isOutputMix(config->portId)) {
        AUDIO_LOGE("openOutputStream: %d is not a configuration of an output", in_args.portConfigId);
        return error(EX_ILLEGAL_ARGUMENT);
    }
    const int32_t mixPortId = config->portId;
    if (mStreams.count(in_args.portConfigId) != 0 || streamOnPort(mixPortId)) {
        AUDIO_LOGE("openOutputStream: %s already has a stream", streamName(mixPortId).c_str());
        return error(EX_ILLEGAL_STATE);
    }
    if (in_args.bufferSizeFrames < kBurstFrames) {
        AUDIO_LOGE("openOutputStream: a buffer of %lld frames is under the %d the output needs",
                   static_cast<long long>(in_args.bufferSizeFrames), kBurstFrames);
        return error(EX_ILLEGAL_ARGUMENT);
    }
    const int64_t bufferFrames = std::min<int64_t>(in_args.bufferSizeFrames, kSampleRate);
    std::weak_ptr<Module> self = ref<Module>();
    auto stream = StreamOut::create(
            in_args.portConfigId, bufferFrames, streamName(mixPortId),
            [self](int32_t id) {
                if (auto module = self.lock()) module->streamClosed(id);
            },
            &_aidl_return->desc);
    if (stream == nullptr) {
        return error(EX_ILLEGAL_STATE);
    }
    ndk::SpAIBinder binder = stream->asBinder();
    AIBinder_setMinSchedulerPolicy(binder.get(), SCHED_NORMAL, kAudioPriority);
    AIBinder_setInheritRt(binder.get(), true);
    mStreams[in_args.portConfigId] = stream;
    _aidl_return->stream = stream;
    AUDIO_LOGI("opened the %s stream on configuration %d, %lld frames", streamName(mixPortId).c_str(),
               in_args.portConfigId, static_cast<long long>(bufferFrames));
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getSupportedPlaybackRateFactors(core::IModule::SupportedPlaybackRateFactors*) {
    return error(EX_UNSUPPORTED_OPERATION);
}

ndk::ScopedAStatus Module::setAudioPatch(const core::AudioPatch& in_requested,
                                         core::AudioPatch* _aidl_return) {
    std::lock_guard<std::mutex> lock(mLock);
    const auto& sources = in_requested.sourcePortConfigIds;
    const auto& sinks = in_requested.sinkPortConfigIds;
    if (sources.size() != 1 || sinks.size() != 1) {
        return error(EX_ILLEGAL_ARGUMENT);
    }
    auto source = findConfig(sources[0]);
    auto sink = findConfig(sinks[0]);
    const bool output = source != mConfigs.end() && sink != mConfigs.end() &&
                        isOutputMix(source->portId) && sink->portId == deviceOfMix(source->portId);
    const bool input = source != mConfigs.end() && sink != mConfigs.end() &&
                       source->portId == kMicPortId && sink->portId == kInputMixPortId;
    if (!output && !input) {
        // The routes are each output to its device and the microphone to the input.
        return error(EX_ILLEGAL_ARGUMENT);
    }
    auto existing = mPatches.end();
    if (in_requested.id != 0) {
        existing = std::find_if(mPatches.begin(), mPatches.end(),
                                [&](const core::AudioPatch& p) { return p.id == in_requested.id; });
        if (existing == mPatches.end()) return error(EX_ILLEGAL_ARGUMENT);
    }
    *_aidl_return = in_requested;
    _aidl_return->minimumStreamBufferSizeFrames = kBurstFrames;
    _aidl_return->latenciesMs = {kBurstFrames * 1000 / kSampleRate};
    if (existing == mPatches.end()) {
        _aidl_return->id = mNextPatchId++;
        mPatches.push_back(*_aidl_return);
    } else {
        *existing = *_aidl_return;
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::setAudioPortConfig(const AudioPortConfig& in_requested,
                                              AudioPortConfig* out_suggested, bool* _aidl_return) {
    std::lock_guard<std::mutex> lock(mLock);
    auto existing = mConfigs.end();
    if (in_requested.id != 0) {
        existing = findConfig(in_requested.id);
        if (existing == mConfigs.end()) return error(EX_ILLEGAL_ARGUMENT);
    }
    const int32_t portId = existing != mConfigs.end() ? existing->portId : in_requested.portId;
    const AudioPort* port = findPort(portId);
    if (port == nullptr) return error(EX_ILLEGAL_ARGUMENT);
    // A device has one configuration, its own, with the port's ID: a request for the device
    // changes that one.
    if (existing == mConfigs.end() && port->ext.getTag() == AudioPortExt::Tag::device) {
        existing = findConfig(port->id);
    }

    // What the configuration becomes: the existing one, or the port's one format.
    if (existing != mConfigs.end()) {
        *out_suggested = *existing;
    } else {
        *out_suggested = AudioPortConfig{};
        out_suggested->portId = port->id;
        out_suggested->format = pcm16();
        out_suggested->channelMask = stereo();
        out_suggested->sampleRate = hostRate();
        out_suggested->flags = port->flags;
        out_suggested->ext = port->ext;
    }

    bool valid = matchesHostFormat(in_requested);
    if (in_requested.flags.has_value() && in_requested.flags.value() != port->flags) valid = false;
    if (in_requested.gain.has_value()) {
        // The volume audio policy gives an output device. It is kept as the device's gain and
        // never applied: the sound leaves the guest at full scale, and the host's volume,
        // which the agent tells the host, is the only one it has.
        if (takesGain(*port, in_requested.gain.value())) {
            out_suggested->gain = in_requested.gain;
        } else {
            valid = false;
        }
    }
    if (in_requested.ext.getTag() != AudioPortExt::Tag::unspecified) {
        if (in_requested.ext.getTag() != port->ext.getTag()) {
            valid = false;
        } else if (port->ext.getTag() == AudioPortExt::Tag::mix) {
            // The framework names its own handle and use case.
            const auto& from = in_requested.ext.get<AudioPortExt::Tag::mix>();
            auto& to = out_suggested->ext.get<AudioPortExt::Tag::mix>();
            to.handle = from.handle;
            to.usecase = from.usecase;
        }
    }

    if (existing == mConfigs.end() && valid && fullySpecified(in_requested)) {
        out_suggested->id = mNextConfigId++;
        mConfigs.push_back(*out_suggested);
        *_aidl_return = true;
    } else if (existing != mConfigs.end() && valid) {
        *existing = *out_suggested;
        *_aidl_return = true;
    } else {
        *_aidl_return = false;
    }
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::resetAudioPatch(int32_t in_patchId) {
    std::lock_guard<std::mutex> lock(mLock);
    auto it = std::find_if(mPatches.begin(), mPatches.end(),
                           [&](const core::AudioPatch& p) { return p.id == in_patchId; });
    if (it == mPatches.end()) return error(EX_ILLEGAL_ARGUMENT);
    mPatches.erase(it);
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::resetAudioPortConfig(int32_t in_portConfigId) {
    std::lock_guard<std::mutex> lock(mLock);
    auto it = findConfig(in_portConfigId);
    if (it == mConfigs.end()) return error(EX_ILLEGAL_ARGUMENT);
    if (configInUse(in_portConfigId)) return error(EX_ILLEGAL_STATE);
    // The devices' own configurations stay; they never change.
    const AudioPort* port = findPort(it->portId);
    const bool device = port != nullptr && port->ext.getTag() == AudioPortExt::Tag::device &&
                        in_portConfigId == port->id;
    if (!device) {
        mConfigs.erase(it);
    }
    return ndk::ScopedAStatus::ok();
}

// The module has no master volume or mute: the host's volume is the only one the sound has.
ndk::ScopedAStatus Module::getMasterMute(bool*) { return error(EX_UNSUPPORTED_OPERATION); }
ndk::ScopedAStatus Module::setMasterMute(bool) { return error(EX_UNSUPPORTED_OPERATION); }
ndk::ScopedAStatus Module::getMasterVolume(float*) { return error(EX_UNSUPPORTED_OPERATION); }
ndk::ScopedAStatus Module::setMasterVolume(float) { return error(EX_UNSUPPORTED_OPERATION); }

ndk::ScopedAStatus Module::getMicMute(bool* _aidl_return) {
    *_aidl_return = mMicMute->load();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::setMicMute(bool in_mute) {
    mMicMute->store(in_mute);
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getMicrophones(std::vector<media::MicrophoneInfo>* _aidl_return) {
    _aidl_return->clear();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::updateAudioMode(media::AudioMode) { return ndk::ScopedAStatus::ok(); }

ndk::ScopedAStatus Module::updateScreenRotation(core::IModule::ScreenRotation) {
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::updateScreenState(bool) { return ndk::ScopedAStatus::ok(); }

ndk::ScopedAStatus Module::getSoundDose(std::shared_ptr<core::sounddose::ISoundDose>* _aidl_return) {
    *_aidl_return = nullptr;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::generateHwAvSyncId(int32_t*) { return error(EX_UNSUPPORTED_OPERATION); }

// The module has no vendor parameters: asked for any, it has none to give.
ndk::ScopedAStatus Module::getVendorParameters(const std::vector<std::string>&,
                                               std::vector<core::VendorParameter>* _aidl_return) {
    _aidl_return->clear();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::setVendorParameters(const std::vector<core::VendorParameter>&, bool) {
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::addDeviceEffect(
        int32_t, const std::shared_ptr<::aidl::android::hardware::audio::effect::IEffect>&) {
    return error(EX_UNSUPPORTED_OPERATION);
}

ndk::ScopedAStatus Module::removeDeviceEffect(
        int32_t, const std::shared_ptr<::aidl::android::hardware::audio::effect::IEffect>&) {
    return error(EX_UNSUPPORTED_OPERATION);
}

ndk::ScopedAStatus Module::getMmapPolicyInfos(media::AudioMMapPolicyType,
                                              std::vector<media::AudioMMapPolicyInfo>* _aidl_return) {
    // No port plays from shared memory the app writes directly.
    _aidl_return->clear();
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::supportsVariableLatency(bool* _aidl_return) {
    *_aidl_return = false;
    return ndk::ScopedAStatus::ok();
}

ndk::ScopedAStatus Module::getAAudioMixerBurstCount(int32_t*) { return error(EX_UNSUPPORTED_OPERATION); }

ndk::ScopedAStatus Module::getAAudioHardwareBurstMinUsec(int32_t*) {
    return error(EX_UNSUPPORTED_OPERATION);
}

ndk::ScopedAStatus Module::getFlushFromFrameSupport(const AudioPortConfig&,
                                                    media::FlushFromFrameSupport* _aidl_return) {
    *_aidl_return = media::FlushFromFrameSupport::UNSUPPORTED;
    return ndk::ScopedAStatus::ok();
}

}  // namespace dsa::audio
