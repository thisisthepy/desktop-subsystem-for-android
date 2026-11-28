// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// What the host renderer knows of the picture in a YUV ColorBuffer, and when it composes one.
//
// A YUV ColorBuffer holds what a guest CPU wrote into a gralloc buffer (a video decoder's
// pictures, a camera's frames). Nothing but an upload writes it: the guest packs the planes and
// the renderer copies them into the image in bounded GPU jobs, one per plane. Until a whole
// upload has landed, the image holds its allocation's zeros (or, after a failed job, some
// planes of a picture and the zeros or old planes of the rest). Read as YUV, a luma and chroma
// of 0 is solid green. So the renderer:
//
// - composes a layer of a YUV ColorBuffer only once an upload has finished in it. Otherwise it
//   refuses the whole composition, and the guest posts nothing for that frame: the task's
//   window keeps showing its last frame, with the last picture that did land.
// - refuses an upload whose planes hold no picture (a sampled luma row of zeros over chroma
//   zeros, dsa::packedHoldsNoPicture). Such planes come from a process that copied the buffer
//   up before its producer had written it; the ColorBuffer keeps the picture it had.
//
// Both are counted, and a line in the renderer's log says how often.

#pragma once

#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <string>

#include "dsa_cpu_upload.h"

namespace dsa {

// The picture in a YUV ColorBuffer, as far as the renderer's uploads go.
enum class YuvPicture {
    // No upload has finished since the image was made (or made again on a new device).
    Unwritten,
    // An upload's GPU jobs started and did not all finish: some planes are new and some not.
    Partial,
    // The last upload finished: every plane holds the same picture.
    Whole,
};

// Whether a layer of a YUV ColorBuffer whose picture is `picture` may be composed.
inline bool yuvPictureComposable(YuvPicture picture) { return picture == YuvPicture::Whole; }

// The picture after an upload of `jobs` GPU jobs into a ColorBuffer that held `before`, of
// which `started` were submitted and `finished` were waited for to the end. An upload that
// submitted nothing left the image as it was.
inline YuvPicture yuvPictureAfterUpload(YuvPicture before, size_t jobs, size_t started,
                                        size_t finished) {
    if (jobs > 0 && finished >= jobs) {
        return YuvPicture::Whole;
    }
    if (started == 0) {
        return before;
    }
    return YuvPicture::Partial;
}

// Whether `size` bytes of packed planes for a `width` x `height` YUV ColorBuffer (three planes
// or two, as the image lays them out) hold no picture: a luma row of zeros over chroma rows of
// zeros, which no encoder makes and the compositor shows as solid green. False for planes of
// another size, which the upload refuses on its own.
inline bool yuvUploadHoldsNoPicture(bool threePlanes, uint32_t width, uint32_t height,
                                    const uint8_t* bytes, size_t size) {
    const uint32_t fourcc =
        threePlanes ? uploadFourcc('Y', 'V', '1', '2') : uploadFourcc('N', 'V', '1', '2');
    const UploadLayout layout = uploadLayout(fourcc, width, height, /*hostHasYuv=*/true);
    if (!layout.valid() || !bytes || size != layout.packedSize()) {
        return false;
    }
    return packedHoldsNoPicture(bytes, layout);
}

// The renderer's count of what it kept from the screen, and the log line that reports it: at
// once for the first event, then at most once every `kLineIntervalNs`, with what happened
// since the last line. Events after a line are reported with the next event past the interval.
class YuvPictureTally {
   public:
    static constexpr uint64_t kLineIntervalNs = 5000000000ULL;

    // A composition refused because a video layer's ColorBuffer held `picture`.
    bool compositionHeld(YuvPicture picture, uint64_t nowNs, std::string* line) {
        if (picture == YuvPicture::Partial) {
            ++mHeldPartial;
        } else {
            ++mHeldNone;
        }
        ++mTotalHeld;
        return report(nowNs, line);
    }

    // An upload refused because its planes held no picture.
    bool uploadRefused(uint64_t nowNs, std::string* line) {
        ++mRefused;
        ++mTotalRefused;
        return report(nowNs, line);
    }

    uint64_t totalHeld() const { return mTotalHeld; }
    uint64_t totalRefused() const { return mTotalRefused; }

   private:
    bool report(uint64_t nowNs, std::string* line) {
        if (mReported && nowNs - mLastNs < kLineIntervalNs) {
            return false;
        }
        char text[512];
        std::snprintf(text, sizeof(text),
                      "video pictures: the task window kept its last frame instead of %llu "
                      "composition(s) whose video ColorBuffer held no finished upload (%llu never "
                      "written, %llu left part-written), and %llu upload(s) whose planes held no "
                      "picture (zeros) were refused, the ColorBuffer keeping its last picture; "
                      "%llu and %llu since the renderer started",
                      static_cast<unsigned long long>(mHeldNone + mHeldPartial),
                      static_cast<unsigned long long>(mHeldNone),
                      static_cast<unsigned long long>(mHeldPartial),
                      static_cast<unsigned long long>(mRefused),
                      static_cast<unsigned long long>(mTotalHeld),
                      static_cast<unsigned long long>(mTotalRefused));
        *line = text;
        mReported = true;
        mLastNs = nowNs;
        mHeldNone = 0;
        mHeldPartial = 0;
        mRefused = 0;
        return true;
    }

    bool mReported = false;
    uint64_t mLastNs = 0;
    uint64_t mHeldNone = 0;
    uint64_t mHeldPartial = 0;
    uint64_t mRefused = 0;
    uint64_t mTotalHeld = 0;
    uint64_t mTotalRefused = 0;
};

}  // namespace dsa
