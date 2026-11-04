// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "decoder_interface.h"

#include <cstring>

namespace dsa::codec2 {

using namespace android;

namespace {

// HAL_PIXEL_FORMAT_YCBCR_420_888: flexible YUV 4:2:0, as the software decoders output.
constexpr uint32_t kPixelFormatYuv420Flexible = 0x23;
// The smallest input buffer the framework is told to allocate.
constexpr uint32_t kMinInputBufferSize = 2 * 1024 * 1024;
// Pictures the host's decoder may hold before the first comes out (the stream's reorder
// depth), which the framework keeps buffers for.
constexpr uint32_t kOutputDelay = 8;
constexpr uint32_t kMaxOutputDelay = 16;

const DecoderInfo kDecoders[] = {
    {Codec::H264, "c2.dsa.avc.decoder", "video/avc"},
    {Codec::Hevc, "c2.dsa.hevc.decoder", "video/hevc"},
    {Codec::Vp9, "c2.dsa.vp9.decoder", "video/x-vnd.on2.vp9"},
    {Codec::Av1, "c2.dsa.av1.decoder", "video/av01"},
};

}  // namespace

const DecoderInfo* decoderInfos(size_t* count) {
    *count = sizeof(kDecoders) / sizeof(kDecoders[0]);
    return kDecoders;
}

const DecoderInfo* findDecoder(const std::string& name) {
    for (const DecoderInfo& info : kDecoders) {
        if (name == info.name) {
            return &info;
        }
    }
    return nullptr;
}

DecoderIntf::DecoderIntf(const std::shared_ptr<C2ReflectorHelper>& helper, const DecoderInfo& info,
                         uint32_t maxWidth, uint32_t maxHeight)
    : SimpleInterface<void>::BaseParams(helper, info.name, C2Component::KIND_DECODER, C2Component::DOMAIN_VIDEO,
                                        info.mediaType) {
    noPrivateBuffers();
    noInputReferences();
    noOutputReferences();
    noInputLatency();
    noTimeStretch();

    addParameter(DefineParam(mActualOutputDelay, C2_PARAMKEY_OUTPUT_DELAY)
                     .withDefault(new C2PortActualDelayTuning::output(kOutputDelay))
                     .withFields({C2F(mActualOutputDelay, value).inRange(0, kMaxOutputDelay)})
                     .withSetter(Setter<decltype(*mActualOutputDelay)>::StrictValueWithNoDeps)
                     .build());

    addParameter(DefineParam(mAttrib, C2_PARAMKEY_COMPONENT_ATTRIBUTES)
                     .withConstValue(new C2ComponentAttributesSetting(C2Component::ATTRIB_IS_TEMPORAL))
                     .build());

    addParameter(DefineParam(mSize, C2_PARAMKEY_PICTURE_SIZE)
                     .withDefault(new C2StreamPictureSizeInfo::output(0u, 320, 240))
                     .withFields({
                         C2F(mSize, width).inRange(2, maxWidth, 2),
                         C2F(mSize, height).inRange(2, maxHeight, 2),
                     })
                     .withSetter(SizeSetter)
                     .build());

    addParameter(DefineParam(mMaxSize, C2_PARAMKEY_MAX_PICTURE_SIZE)
                     .withDefault(new C2StreamMaxPictureSizeTuning::output(0u, 320, 240))
                     .withFields({
                         C2F(mSize, width).inRange(2, maxWidth, 2),
                         C2F(mSize, height).inRange(2, maxHeight, 2),
                     })
                     .withSetter(MaxPictureSizeSetter, mSize)
                     .build());

    switch (info.codec) {
        case Codec::H264:
            addParameter(DefineParam(mProfileLevel, C2_PARAMKEY_PROFILE_LEVEL)
                             .withDefault(new C2StreamProfileLevelInfo::input(
                                 0u, C2Config::PROFILE_AVC_CONSTRAINED_BASELINE, C2Config::LEVEL_AVC_5_2))
                             .withFields({C2F(mProfileLevel, profile)
                                              .oneOf({C2Config::PROFILE_AVC_CONSTRAINED_BASELINE,
                                                      C2Config::PROFILE_AVC_BASELINE, C2Config::PROFILE_AVC_MAIN,
                                                      C2Config::PROFILE_AVC_CONSTRAINED_HIGH,
                                                      C2Config::PROFILE_AVC_PROGRESSIVE_HIGH,
                                                      C2Config::PROFILE_AVC_HIGH}),
                                          C2F(mProfileLevel, level)
                                              .oneOf({C2Config::LEVEL_AVC_1, C2Config::LEVEL_AVC_1B,
                                                      C2Config::LEVEL_AVC_1_1, C2Config::LEVEL_AVC_1_2,
                                                      C2Config::LEVEL_AVC_1_3, C2Config::LEVEL_AVC_2,
                                                      C2Config::LEVEL_AVC_2_1, C2Config::LEVEL_AVC_2_2,
                                                      C2Config::LEVEL_AVC_3, C2Config::LEVEL_AVC_3_1,
                                                      C2Config::LEVEL_AVC_3_2, C2Config::LEVEL_AVC_4,
                                                      C2Config::LEVEL_AVC_4_1, C2Config::LEVEL_AVC_4_2,
                                                      C2Config::LEVEL_AVC_5, C2Config::LEVEL_AVC_5_1,
                                                      C2Config::LEVEL_AVC_5_2})})
                             .withSetter(ProfileLevelSetter)
                             .build());
            break;
        case Codec::Hevc:
            addParameter(DefineParam(mProfileLevel, C2_PARAMKEY_PROFILE_LEVEL)
                             .withDefault(new C2StreamProfileLevelInfo::input(0u, C2Config::PROFILE_HEVC_MAIN,
                                                                              C2Config::LEVEL_HEVC_MAIN_5_1))
                             .withFields({C2F(mProfileLevel, profile).oneOf({C2Config::PROFILE_HEVC_MAIN,
                                                                             C2Config::PROFILE_HEVC_MAIN_STILL}),
                                          C2F(mProfileLevel, level)
                                              .oneOf({C2Config::LEVEL_HEVC_MAIN_1, C2Config::LEVEL_HEVC_MAIN_2,
                                                      C2Config::LEVEL_HEVC_MAIN_2_1, C2Config::LEVEL_HEVC_MAIN_3,
                                                      C2Config::LEVEL_HEVC_MAIN_3_1, C2Config::LEVEL_HEVC_MAIN_4,
                                                      C2Config::LEVEL_HEVC_MAIN_4_1, C2Config::LEVEL_HEVC_MAIN_5,
                                                      C2Config::LEVEL_HEVC_MAIN_5_1})})
                             .withSetter(ProfileLevelSetter)
                             .build());
            break;
        case Codec::Vp9:
            addParameter(
                DefineParam(mProfileLevel, C2_PARAMKEY_PROFILE_LEVEL)
                    .withDefault(new C2StreamProfileLevelInfo::input(0u, C2Config::PROFILE_VP9_0, C2Config::LEVEL_VP9_5))
                    .withFields({C2F(mProfileLevel, profile).oneOf({C2Config::PROFILE_VP9_0}),
                                 C2F(mProfileLevel, level)
                                     .oneOf({C2Config::LEVEL_VP9_1, C2Config::LEVEL_VP9_1_1, C2Config::LEVEL_VP9_2,
                                             C2Config::LEVEL_VP9_2_1, C2Config::LEVEL_VP9_3, C2Config::LEVEL_VP9_3_1,
                                             C2Config::LEVEL_VP9_4, C2Config::LEVEL_VP9_4_1, C2Config::LEVEL_VP9_5})})
                    .withSetter(ProfileLevelSetter)
                    .build());
            break;
        case Codec::Av1:
            addParameter(
                DefineParam(mProfileLevel, C2_PARAMKEY_PROFILE_LEVEL)
                    .withDefault(new C2StreamProfileLevelInfo::input(0u, C2Config::PROFILE_AV1_0, C2Config::LEVEL_AV1_5_1))
                    .withFields({C2F(mProfileLevel, profile).oneOf({C2Config::PROFILE_AV1_0}),
                                 C2F(mProfileLevel, level)
                                     .oneOf({C2Config::LEVEL_AV1_2, C2Config::LEVEL_AV1_2_1, C2Config::LEVEL_AV1_3,
                                             C2Config::LEVEL_AV1_3_1, C2Config::LEVEL_AV1_4, C2Config::LEVEL_AV1_4_1,
                                             C2Config::LEVEL_AV1_5, C2Config::LEVEL_AV1_5_1})})
                    .withSetter(ProfileLevelSetter)
                    .build());
            break;
    }

    addParameter(DefineParam(mMaxInputSize, C2_PARAMKEY_INPUT_MAX_BUFFER_SIZE)
                     .withDefault(new C2StreamMaxBufferSizeInfo::input(0u, kMinInputBufferSize))
                     .withFields({C2F(mMaxInputSize, value).any()})
                     .calculatedAs(MaxInputSizeSetter, mMaxSize)
                     .build());

    std::shared_ptr<C2StreamColorInfo::output> defaultColorInfo = C2StreamColorInfo::output::AllocShared(
        {C2ChromaOffsetStruct::ITU_YUV_420_0()}, 0u, 8u /* bitDepth */, C2Color::YUV_420);
    helper->addStructDescriptors<C2ChromaOffsetStruct>();
    addParameter(DefineParam(mColorInfo, C2_PARAMKEY_CODED_COLOR_INFO).withConstValue(defaultColorInfo).build());

    addParameter(DefineParam(mDefaultColorAspects, C2_PARAMKEY_DEFAULT_COLOR_ASPECTS)
                     .withDefault(new C2StreamColorAspectsTuning::output(
                         0u, C2Color::RANGE_UNSPECIFIED, C2Color::PRIMARIES_UNSPECIFIED,
                         C2Color::TRANSFER_UNSPECIFIED, C2Color::MATRIX_UNSPECIFIED))
                     .withFields({C2F(mDefaultColorAspects, range)
                                      .inRange(C2Color::RANGE_UNSPECIFIED, C2Color::RANGE_OTHER),
                                  C2F(mDefaultColorAspects, primaries)
                                      .inRange(C2Color::PRIMARIES_UNSPECIFIED, C2Color::PRIMARIES_OTHER),
                                  C2F(mDefaultColorAspects, transfer)
                                      .inRange(C2Color::TRANSFER_UNSPECIFIED, C2Color::TRANSFER_OTHER),
                                  C2F(mDefaultColorAspects, matrix)
                                      .inRange(C2Color::MATRIX_UNSPECIFIED, C2Color::MATRIX_OTHER)})
                     .withSetter(DefaultColorAspectsSetter)
                     .build());

    addParameter(DefineParam(mColorAspects, C2_PARAMKEY_COLOR_ASPECTS)
                     .withDefault(new C2StreamColorAspectsInfo::output(
                         0u, C2Color::RANGE_LIMITED, C2Color::PRIMARIES_UNSPECIFIED, C2Color::TRANSFER_UNSPECIFIED,
                         C2Color::MATRIX_UNSPECIFIED))
                     .withFields({C2F(mColorAspects, range).inRange(C2Color::RANGE_UNSPECIFIED, C2Color::RANGE_OTHER),
                                  C2F(mColorAspects, primaries)
                                      .inRange(C2Color::PRIMARIES_UNSPECIFIED, C2Color::PRIMARIES_OTHER),
                                  C2F(mColorAspects, transfer)
                                      .inRange(C2Color::TRANSFER_UNSPECIFIED, C2Color::TRANSFER_OTHER),
                                  C2F(mColorAspects, matrix)
                                      .inRange(C2Color::MATRIX_UNSPECIFIED, C2Color::MATRIX_OTHER)})
                     .withSetter(ColorAspectsSetter, mDefaultColorAspects)
                     .build());

    addParameter(DefineParam(mPixelFormat, C2_PARAMKEY_PIXEL_FORMAT)
                     .withConstValue(new C2StreamPixelFormatInfo::output(0u, kPixelFormatYuv420Flexible))
                     .build());

    addParameter(DefineParam(mUsage, C2_PARAMKEY_OUTPUT_STREAM_USAGE)
                     .withDefault(new C2StreamUsageTuning::output(0u, 0))
                     .withFields({C2F(mUsage, value).any()})
                     .withSetter(UsageSetter)
                     .build());
}

C2R DecoderIntf::SizeSetter(bool, const C2P<C2StreamPictureSizeInfo::output>& oldMe,
                            C2P<C2StreamPictureSizeInfo::output>& me) {
    C2R res = C2R::Ok();
    if (!me.F(me.v.width).supportsAtAll(me.v.width)) {
        res = res.plus(C2SettingResultBuilder::BadValue(me.F(me.v.width)));
        me.set().width = oldMe.v.width;
    }
    if (!me.F(me.v.height).supportsAtAll(me.v.height)) {
        res = res.plus(C2SettingResultBuilder::BadValue(me.F(me.v.height)));
        me.set().height = oldMe.v.height;
    }
    return res;
}

C2R DecoderIntf::MaxPictureSizeSetter(bool, C2P<C2StreamMaxPictureSizeTuning::output>& me,
                                      const C2P<C2StreamPictureSizeInfo::output>& size) {
    me.set().width = c2_max(me.v.width, size.v.width);
    me.set().height = c2_max(me.v.height, size.v.height);
    return C2R::Ok();
}

C2R DecoderIntf::MaxInputSizeSetter(bool, C2P<C2StreamMaxBufferSizeInfo::input>& me,
                                    const C2P<C2StreamMaxPictureSizeTuning::output>& maxSize) {
    // A compression ratio of 2 at worst, as the platform's decoders assume.
    me.set().value =
        c2_max((((maxSize.v.width + 15) / 16) * ((maxSize.v.height + 15) / 16) * 192), kMinInputBufferSize);
    return C2R::Ok();
}

C2R DecoderIntf::ProfileLevelSetter(bool, C2P<C2StreamProfileLevelInfo::input>&) { return C2R::Ok(); }

C2R DecoderIntf::DefaultColorAspectsSetter(bool, C2P<C2StreamColorAspectsTuning::output>& me) {
    if (me.v.range > C2Color::RANGE_OTHER) me.set().range = C2Color::RANGE_OTHER;
    if (me.v.primaries > C2Color::PRIMARIES_OTHER) me.set().primaries = C2Color::PRIMARIES_OTHER;
    if (me.v.transfer > C2Color::TRANSFER_OTHER) me.set().transfer = C2Color::TRANSFER_OTHER;
    if (me.v.matrix > C2Color::MATRIX_OTHER) me.set().matrix = C2Color::MATRIX_OTHER;
    return C2R::Ok();
}

C2R DecoderIntf::ColorAspectsSetter(bool, C2P<C2StreamColorAspectsInfo::output>& me,
                                    const C2P<C2StreamColorAspectsTuning::output>& def) {
    // The host's decoder outputs 8-bit 4:2:0 in limited range; what the stream says of its
    // primaries, transfer and matrix is taken from the app's defaults.
    me.set().range = def.v.range == C2Color::RANGE_UNSPECIFIED ? C2Color::RANGE_LIMITED : def.v.range;
    me.set().primaries = def.v.primaries;
    me.set().transfer = def.v.transfer;
    me.set().matrix = def.v.matrix;
    return C2R::Ok();
}

C2R DecoderIntf::UsageSetter(bool, C2P<C2StreamUsageTuning::output>&) { return C2R::Ok(); }

}  // namespace dsa::codec2
