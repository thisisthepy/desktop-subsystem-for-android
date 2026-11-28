// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Which layers the host composes, and when a layer holds a new picture, for the copies of
// what a CPU wrote into a buffer (cpu_upload.h). Free of Android headers, so the build
// machine tests it.

#pragma once

#include <cstdint>
#include <map>
#include <optional>
#include <string>

#include "dsa_cpu_upload.h"

namespace dsa::hwc {

// How a YUV layer's samples become RGB: the matrix, the range, and the order of its chroma
// planes. The host reads it from the layer's colour (encodeYuvColor), which it ignores for a
// buffer layer otherwise.
enum class YuvMatrix : uint8_t { Bt601 = 1, Bt709 = 2, Bt2020 = 3 };
enum class YuvRange : uint8_t { Full = 1, Limited = 2 };
struct YuvColor {
    YuvMatrix matrix = YuvMatrix::Bt601;
    YuvRange range = YuvRange::Limited;
    // The first chroma plane (or the first byte of each pair) is V: YV12 and NV21.
    bool vuOrder = false;
};

// The marker in the colour's alpha that says the other three bytes describe the YUV.
constexpr uint8_t kYuvColorMarker = 0x59;  // 'Y'

// The colour of a YUV layer of `dataspace` (an Android Dataspace) whose buffer is `height`
// rows high, or nothing when the host cannot show it right: an HDR transfer (PQ, HLG) needs
// tone mapping, BT.2020 constant luminance and extended range have no YCbCr model, and RGB
// standards (DCI-P3, Adobe RGB, film) mean nothing for YUV. A dataspace without a standard or
// range is taken as a player leaves it: BT.709 from 720 rows up, BT.601 below, limited range.
std::optional<YuvColor> yuvColorOf(int32_t dataspace, uint32_t height);

// Whether a buffer of this 4:2:0 DRM format has its chroma in V, U order.
bool chromaVuOrder(uint32_t drmFourcc);

// The layer colour the host reads `color` from (red the matrix, green the range, blue the
// order, alpha the marker).
void encodeYuvColor(const YuvColor& color, uint8_t out[4]);

// The RGB the host shows for one 8-bit YUV sample of `color`, each channel 0 to 1 before
// clamping: the reference for what its sampler conversion computes.
struct Rgb {
    double r, g, b;
};
Rgb yuvToRgb(uint8_t y, uint8_t u, uint8_t v, const YuvColor& color);

// Whether the host composes YUV layers (it lists the YUV composition extension). Set when the
// hwcomposer learns it from the host; false until then and after the connection goes.
void setHostComposesYuv(bool composes);
bool hostComposesYuv();

// Whether the host may compose a layer of this DRM format, size and dataspace itself. RGB
// always. A 4:2:0 YUV buffer (a video's pictures) only when the host composes YUV, the
// buffer's ColorBuffer is YUV (even width and height), and its colour is one the host shows
// right (yuvColorOf). SurfaceFlinger draws any other layer into the client target instead,
// from the same ColorBuffer.
bool hostComposes(uint32_t drmFourcc, uint32_t width, uint32_t height, int32_t dataspace,
                  bool hostComposesYuv);

// The buffer each layer last showed. A layer whose buffer changes holds a new picture: a
// queue hands a buffer back to its producer only after it is shown, and the producer writes
// the next picture into it before queueing it again.
class LayerPictures {
   public:
    // Records that `layer` now shows `buffer`. True when that is another buffer than before.
    bool show(int64_t layer, uint32_t buffer);
    void forget(int64_t layer) { mShown.erase(layer); }

   private:
    std::map<int64_t, uint32_t> mShown;
};

// Says when a failure to copy a buffer up is logged: the first of a run, then one in `every`,
// so a failure on every picture neither floods the log nor goes quiet after one line.
class UploadFailureLog {
   public:
    explicit UploadFailureLog(uint32_t every = 600) : mEvery(every) {}
    // True when this failure should be logged; `*count` is how many there have been in a row.
    bool shouldLog(uint32_t* count);
    // An upload succeeded: the next failure starts a new run.
    void reset() { mCount = 0; }

   private:
    uint32_t mEvery;
    uint32_t mCount = 0;
};

// The hwcomposer's upload line, once a second: the bytes and copies that reached the host, and
// the pictures it kept from the host. The bytes are the planes' packed size, whichever way
// they went (the shared graphics memory or the stream).
class UploadTally {
   public:
    // A copy of `layout`'s planes reached the host at `nowNs`. True, with the line in `*line`,
    // when a second or more has passed since the window began; the counts start again then.
    bool uploaded(uint64_t nowNs, const dsa::UploadLayout& layout, std::string* line);
    // A buffer's planes held no picture (luma and chroma zeros, solid green) and were not sent.
    void heldNoPicture() { ++mNoPicture; }
    // Planes packed for the host changed before the host had read them, so its ColorBuffer may
    // not hold them; the layer keeps its last picture.
    void changedUnderHost() { ++mChanged; }

   private:
    dsa::UploadRate mRate;
    uint32_t mNoPicture = 0;
    uint32_t mChanged = 0;
};

}  // namespace dsa::hwc
