// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The Codec 2.0 parameters of DSA's decoders, which decode on the host's GPU: one decoder
// component for each format the host decodes (H.264, HEVC, VP9, AV1). They declare what the
// platform's software decoders declare, built with the platform's own interface helper, so
// the framework configures them the same way; only the decoding happens elsewhere.

#pragma once

#include <memory>
#include <string>

#include <C2Config.h>
#include <SimpleC2Interface.h>
#include <util/C2InterfaceHelper.h>

#include "video_messages.h"

namespace dsa::codec2 {

// The component's name, media type and its pixel format of output, for a format.
struct DecoderInfo {
    Codec codec;
    const char* name;
    const char* mediaType;
};

// The four decoders, whether or not the host decodes their format.
const DecoderInfo* decoderInfos(size_t* count);
const DecoderInfo* findDecoder(const std::string& name);

class DecoderIntf : public android::SimpleInterface<void>::BaseParams {
   public:
    DecoderIntf(const std::shared_ptr<C2ReflectorHelper>& helper, const DecoderInfo& info, uint32_t maxWidth,
                uint32_t maxHeight);

    uint32_t width() const { return mSize->width; }
    uint32_t height() const { return mSize->height; }
    uint64_t outputUsage() const { return mUsage->value; }
    C2BlockPool::local_id_t outputPoolId() const {
        return mOutputPoolIds && mOutputPoolIds->flexCount() > 0 ? mOutputPoolIds->m.values[0]
                                                                 : C2BlockPool::BASIC_GRAPHIC;
    }

    static C2R SizeSetter(bool mayBlock, const C2P<C2StreamPictureSizeInfo::output>& oldMe,
                          C2P<C2StreamPictureSizeInfo::output>& me);
    static C2R MaxPictureSizeSetter(bool mayBlock, C2P<C2StreamMaxPictureSizeTuning::output>& me,
                                    const C2P<C2StreamPictureSizeInfo::output>& size);
    static C2R MaxInputSizeSetter(bool mayBlock, C2P<C2StreamMaxBufferSizeInfo::input>& me,
                                  const C2P<C2StreamMaxPictureSizeTuning::output>& maxSize);
    static C2R ProfileLevelSetter(bool mayBlock, C2P<C2StreamProfileLevelInfo::input>& me);
    static C2R ColorAspectsSetter(bool mayBlock, C2P<C2StreamColorAspectsInfo::output>& me,
                                  const C2P<C2StreamColorAspectsTuning::output>& def);
    static C2R UsageSetter(bool mayBlock, C2P<C2StreamUsageTuning::output>& me);
    static C2R DefaultColorAspectsSetter(bool mayBlock, C2P<C2StreamColorAspectsTuning::output>& me);

   private:
    std::shared_ptr<C2StreamProfileLevelInfo::input> mProfileLevel;
    std::shared_ptr<C2StreamPictureSizeInfo::output> mSize;
    std::shared_ptr<C2StreamMaxPictureSizeTuning::output> mMaxSize;
    std::shared_ptr<C2StreamMaxBufferSizeInfo::input> mMaxInputSize;
    std::shared_ptr<C2StreamColorInfo::output> mColorInfo;
    std::shared_ptr<C2StreamColorAspectsTuning::output> mDefaultColorAspects;
    std::shared_ptr<C2StreamColorAspectsInfo::output> mColorAspects;
    std::shared_ptr<C2StreamPixelFormatInfo::output> mPixelFormat;
    std::shared_ptr<C2StreamUsageTuning::output> mUsage;
};

}  // namespace dsa::codec2
