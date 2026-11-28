// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The camera HAL's rules: its camera port messages against the checked-in protocol vectors,
// so they stay in step with the host's; what a host camera offers Android (sizes, rates and
// the streams that run together); the copy of a picture into a buffer at the buffer's size;
// and the JPEG trailer Android reads a still's length from. The protocol folder is the only
// argument.

#include <cstdio>
#include <cstring>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#include "camera_messages.h"
#include "frame_copy.h"
#include "stream_config.h"
#include "test_main.h"

using namespace dsa::camera;

namespace {

std::string gProtocolDir;

std::vector<uint8_t> vector(const std::string& name) {
    std::ifstream in(gProtocolDir + "/vectors/control.vectors");
    std::string line;
    while (std::getline(in, line)) {
        std::istringstream fields(line);
        std::string key, hex;
        fields >> key >> hex;
        if (key != name) {
            continue;
        }
        std::vector<uint8_t> bytes;
        for (size_t i = 0; hex != "-" && i + 1 < hex.size(); i += 2) {
            bytes.push_back(static_cast<uint8_t>(std::stoi(hex.substr(i, 2), nullptr, 16)));
        }
        return bytes;
    }
    ::dsa::test::fail(__FILE__, __LINE__, "the vectors have no case " + name);
    return {};
}

HostCamera webcam() {
    HostCamera camera;
    camera.id = "front";
    camera.name = "Integrated Camera";
    camera.facing = Facing::Front;
    camera.modes = {Mode{640, 480, 30, 15}, Mode{1280, 720, 30, 5}, Mode{1280, 720, 60, 60}};
    return camera;
}

}  // namespace

TEST(fr26_the_open_request_encodes_as_the_protocol_vector) {
    EXPECT_EQ(encodeOpen("front", 1280, 720, 30), vector("camera_open"));
}

TEST(fr26_the_still_request_encodes_as_the_protocol_vector) {
    EXPECT_EQ(encodeStill(4, 1920, 1080, 95), vector("camera_still"));
}

TEST(fr26_the_hosts_camera_list_decodes_with_facing_and_modes) {
    const std::vector<uint8_t> bytes = vector("camera_list");
    const HostMessage message = decodeHostFrame(bytes.data(), bytes.size());
    const auto* list = std::get_if<CameraList>(&message);
    EXPECT_TRUE(list != nullptr);
    if (list && list->cameras.size() == 1) {
        const HostCamera& camera = list->cameras[0];
        EXPECT_EQ(camera.id, std::string("front"));
        EXPECT_EQ(camera.name, std::string("Integrated Camera"));
        EXPECT_TRUE(camera.facing == Facing::Front);
        EXPECT_EQ(camera.sensorOrientation, 90u);
        EXPECT_EQ(camera.modes.size(), 1u);
        EXPECT_EQ(camera.modes[0].width, 1280u);
        EXPECT_EQ(camera.modes[0].height, 720u);
        EXPECT_EQ(camera.modes[0].maxFps, 30u);
        EXPECT_EQ(camera.modes[0].minFps, 15u);
    } else {
        ::dsa::test::fail(__FILE__, __LINE__, "expected one camera");
    }
}

TEST(fr26_a_host_picture_decodes) {
    const std::vector<uint8_t> bytes = vector("camera_frame");
    const HostMessage message = decodeHostFrame(bytes.data(), bytes.size());
    const auto* picture = std::get_if<Picture>(&message);
    EXPECT_TRUE(picture != nullptr);
    if (picture) {
        EXPECT_EQ(picture->sequence, 7u);
        EXPECT_EQ(picture->timestampNs, 33333333u);
        EXPECT_EQ(picture->width, 2u);
        EXPECT_EQ(picture->height, 2u);
        EXPECT_EQ(picture->stride, 2u);
        EXPECT_EQ(picture->nv12, std::string("\x10\x10\x10\x10\x80\x80", 6));
    }
}

TEST(fr26_a_camera_offers_each_size_once_largest_first) {
    const std::vector<StreamSize> modes = modeSizes(webcam());
    EXPECT_EQ(modes.size(), 2u);
    if (modes.size() == 2) {
        EXPECT_EQ(modes[0].width, 1280u);
        // Its fastest mode sets its shortest frame time.
        EXPECT_EQ(modes[0].minFrameDurationNs, 1000000000LL / 60);
        EXPECT_EQ(modes[1].width, 640u);
        EXPECT_EQ(modes[1].minFrameDurationNs, 1000000000LL / 30);
    }
    const std::vector<StreamSize> sizes = streamSizes(webcam());
    for (size_t i = 1; i < sizes.size(); ++i) {
        EXPECT_TRUE(static_cast<uint64_t>(sizes[i - 1].width) * sizes[i - 1].height >=
                    static_cast<uint64_t>(sizes[i].width) * sizes[i].height);
        for (size_t j = 0; j < i; ++j) {
            EXPECT_TRUE(sizes[i].width != sizes[j].width || sizes[i].height != sizes[j].height);
        }
    }
    const auto ranges = fpsRanges(webcam());
    const std::vector<std::pair<int32_t, int32_t>> expected = {{5, 30}, {15, 30}, {30, 30}, {60, 60}};
    EXPECT_EQ(ranges, expected);
}

TEST(fr26_preview_and_video_start_at_up_to_30_frames) {
    EXPECT_EQ(targetFpsRange(webcam(), false), (std::pair<int32_t, int32_t>{5, 30}));
    EXPECT_EQ(targetFpsRange(webcam(), true), (std::pair<int32_t, int32_t>{30, 30}));
}

TEST(fr26_preview_video_and_still_streams_run_together) {
    std::string why;
    EXPECT_TRUE(streamsSupported(webcam(),
                                 {{kFormatImplementationDefined, 1280, 720, true},
                                  {kFormatYcbcr420, 640, 480, true},
                                  {kFormatBlob, 1280, 720, true}},
                                 &why));
    // A size the camera does not have, an input stream, or a third picture stream do not.
    EXPECT_TRUE(!streamsSupported(webcam(), {{kFormatYcbcr420, 1920, 1080, true}}, &why));
    EXPECT_TRUE(!streamsSupported(webcam(), {{kFormatYcbcr420, 640, 480, false}}, &why));
    EXPECT_TRUE(!streamsSupported(webcam(),
                                  {{kFormatYcbcr420, 640, 480, true},
                                   {kFormatYcbcr420, 640, 480, true},
                                   {kFormatImplementationDefined, 640, 480, true}},
                                  &why));
    // The host runs the largest picture stream; a still alone runs at its own size.
    const StreamSize size =
        hostSizeFor(webcam(), {{kFormatYcbcr420, 640, 480, true}, {kFormatBlob, 1280, 720, true}});
    EXPECT_EQ(size.width, 640u);
    const StreamSize still = hostSizeFor(webcam(), {{kFormatBlob, 1280, 720, true}});
    EXPECT_EQ(still.width, 1280u);
}

namespace {

// The camera the owner's PC reported (an OV02C10 webcam): no 320x240 mode of its own.
HostCamera ov02c10() {
    HostCamera camera;
    camera.id = "ov02c10";
    camera.name = "Camera Sensor OV02C10";
    camera.facing = Facing::Front;
    camera.modes = {Mode{1920, 1080, 30, 30}, Mode{1280, 720, 30, 30}, Mode{640, 360, 30, 30}};
    return camera;
}

bool offers(const std::vector<StreamSize>& sizes, uint32_t width, uint32_t height) {
    for (const StreamSize& size : sizes) {
        if (size.width == width && size.height == height) {
            return true;
        }
    }
    return false;
}

}  // namespace

TEST(fr26_a_camera_offers_the_sizes_apps_ask_for_without_looking) {
    const std::vector<StreamSize> sizes = streamSizes(ov02c10());
    // CameraX's analysis, preview and recording sizes, cut from a larger mode.
    for (const auto& [width, height] : std::vector<std::pair<uint32_t, uint32_t>>{
             {320, 240}, {640, 480}, {1280, 720}, {1920, 1080}, {176, 144}, {800, 600}}) {
        EXPECT_TRUE(offers(sizes, width, height));
    }
    // Nothing larger than the camera's own largest mode.
    for (const StreamSize& size : sizes) {
        EXPECT_TRUE(size.width <= 1920 && size.height <= 1080);
        EXPECT_EQ(size.minFrameDurationNs, 1000000000LL / 30);
    }
    EXPECT_TRUE(!offers(streamSizes(webcam()), 1920, 1080));
    EXPECT_TRUE(offers(streamSizes(webcam()), 320, 240));
}

TEST(fr26_cameraxs_default_streams_run_on_a_camera_without_those_modes) {
    std::string why;
    // Preview 1280x720, analysis 320x240 YUV and a full size still: the session that failed.
    const std::vector<StreamAsk> camerax = {{kFormatImplementationDefined, 1280, 720, true},
                                            {kFormatYcbcr420, 320, 240, true},
                                            {kFormatBlob, 1920, 1080, true}};
    EXPECT_TRUE(streamsSupported(ov02c10(), camerax, &why));
    EXPECT_EQ(why, std::string());
    EXPECT_TRUE(streamsSupported(ov02c10(), {{kFormatYcbcr420, 640, 480, true}}, &why));
    // The host runs the smallest mode the largest picture stream fits in, of its shape first.
    const StreamSize preview = hostSizeFor(ov02c10(), camerax);
    EXPECT_EQ(preview.width, 1280u);
    EXPECT_EQ(preview.height, 720u);
    const StreamSize analysis = hostSizeFor(ov02c10(), {{kFormatYcbcr420, 320, 240, true}});
    EXPECT_EQ(analysis.width, 640u);
    const StreamSize fourThree = hostSizeFor(ov02c10(), {{kFormatYcbcr420, 640, 480, true}});
    EXPECT_EQ(fourThree.width, 1280u);
}

TEST(fr26_a_preview_the_screen_shows_is_rgba_and_video_is_yuv) {
    // The SurfaceView's buffers: GPU_TEXTURE | COMPOSER_OVERLAY, which the guest's allocator
    // cannot make in YUV.
    EXPECT_EQ(overrideFormatFor(kFormatImplementationDefined, 0x900), kFormatRgba8888);
    EXPECT_EQ(overrideFormatFor(kFormatImplementationDefined, 0x100), kFormatRgba8888);
    // A SurfaceView a video encoder shares stays RGBA: YUV is never shown by the composer.
    EXPECT_EQ(overrideFormatFor(kFormatImplementationDefined, 0x10900), kFormatRgba8888);
    // A video encoder, or a consumer that reads by CPU, gets YUV.
    EXPECT_EQ(overrideFormatFor(kFormatImplementationDefined, 0x10000), kFormatYcbcr420);
    EXPECT_EQ(overrideFormatFor(kFormatImplementationDefined, 0x10100), kFormatYcbcr420);
    EXPECT_EQ(overrideFormatFor(kFormatImplementationDefined, 0x3), kFormatYcbcr420);
    // Formats an app names stay as named.
    EXPECT_EQ(overrideFormatFor(kFormatYcbcr420, 0x900), kFormatYcbcr420);
    EXPECT_EQ(overrideFormatFor(kFormatBlob, 0x3), kFormatBlob);
}

namespace {

// The guest's allocator, minigbm (the Cuttlefish build in the com.google.cf.gralloc APEX,
// external/minigbm at android-17.0.0_r1), as far as it decides whether a buffer of a format
// and a usage can be made. The guest has no virtio-gpu: its DRM devices are vgem, which
// minigbm has no backend for, and vkms, whose backend is the dumb driver. The rules below
// follow those files line by line; a change to them means the image's minigbm changed.
namespace minigbm {

// BO_USE_* (drv.h).
constexpr uint64_t kScanout = 1ull << 0;
constexpr uint64_t kCursor = 1ull << 1;
constexpr uint64_t kRendering = 1ull << 2;
constexpr uint64_t kLinear = 1ull << 4;
constexpr uint64_t kTexture = 1ull << 5;
constexpr uint64_t kCameraWrite = 1ull << 6;
constexpr uint64_t kCameraRead = 1ull << 7;
constexpr uint64_t kProtected = 1ull << 8;
constexpr uint64_t kSwReadOften = 1ull << 9;
constexpr uint64_t kSwReadRarely = 1ull << 10;
constexpr uint64_t kSwWriteOften = 1ull << 11;
constexpr uint64_t kSwWriteRarely = 1ull << 12;
constexpr uint64_t kVideoDecoder = 1ull << 13;
constexpr uint64_t kVideoEncoder = 1ull << 14;
constexpr uint64_t kFrontRendering = 1ull << 16;
constexpr uint64_t kRenderscript = 1ull << 17;
constexpr uint64_t kGpuDataBuffer = 1ull << 18;
constexpr uint64_t kSensorDirectData = 1ull << 19;

// BO_USE_RENDER_MASK and BO_USE_TEXTURE_MASK (drv_priv.h).
constexpr uint64_t kRenderMask = kLinear | kRendering | kRenderscript | kSwReadOften | kSwWriteOften |
                                 kSwReadRarely | kSwWriteRarely | kTexture | kFrontRendering;
constexpr uint64_t kTextureMask =
    kLinear | kRenderscript | kSwReadOften | kSwWriteOften | kSwReadRarely | kSwWriteRarely | kTexture | kFrontRendering;

enum class Drm {
    None,
    Argb8888,
    Xrgb8888,
    Abgr8888,
    Xbgr8888,
    Bgr888,
    Rgb565,
    R8,
    Nv12,
    Nv21,
    Yvu420,
    Yvu420Android,
    FlexImplementationDefined,
    FlexYcbcr420,
};

// cros_gralloc_convert_format (cros_gralloc/cros_gralloc_helpers.cc), for the formats a
// camera stream or a video picture has.
Drm drmFormat(int32_t android) {
    switch (android) {
        case 0x1:  // RGBA_8888
            return Drm::Abgr8888;
        case 0x2:  // RGBX_8888
            return Drm::Xbgr8888;
        case 0x3:  // RGB_888
            return Drm::Bgr888;
        case 0x4:  // RGB_565
            return Drm::Rgb565;
        case 0x5:  // BGRA_8888
            return Drm::Argb8888;
        case 0x21:  // BLOB
            return Drm::R8;
        case 0x22:  // IMPLEMENTATION_DEFINED
            return Drm::FlexImplementationDefined;
        case 0x23:  // YCbCr_420_888
            return Drm::FlexYcbcr420;
        case 0x32315659:  // YV12
            return Drm::Yvu420Android;
        default:
            return Drm::None;
    }
}

// cros_gralloc_convert_usage (cros_gralloc/cros_gralloc_helpers.cc), in its order: a bit
// group counts only when all its bits are set, and a usage with a bit left over is refused.
uint64_t useFlags(uint64_t usage) {
    uint64_t flags = 0;
    const auto take = [&](uint64_t mask, uint64_t bo) {
        if ((usage & mask) == mask) {
            usage &= ~mask;
            flags |= bo;
        }
    };
    take(0x3, kSwReadOften);         // SW_READ_OFTEN
    take(0x2, kSwReadRarely);        // SW_READ_RARELY
    take(0x30, kSwWriteOften);       // SW_WRITE_OFTEN
    take(0x20, kSwWriteRarely);      // SW_WRITE_RARELY
    take(0x100, kTexture);           // HW_TEXTURE
    take(0x200, kRendering);         // HW_RENDER
    take(0x400, kRendering);         // HW_2D
    take(0x800, kScanout | kTexture);  // HW_COMPOSER
    take(0x1000, 0);                 // HW_FB
    take(0x2000, 0);                 // EXTERNAL_DISP
    take(0x4000, kProtected);        // PROTECTED (Android 15 and later)
    take(0x8000, kCursor);           // CURSOR (Android 15 and later)
    take(0x10000, kVideoEncoder | kSwReadOften);  // HW_VIDEO_ENCODER
    take(0x20000, kCameraWrite);     // HW_CAMERA_WRITE
    take(0x40000, kCameraRead);      // HW_CAMERA_READ
    take(0x100000, kRenderscript);   // RENDERSCRIPT
    take(1ull << 22, kVideoDecoder);  // VIDEO_DECODER
    take(1ull << 23, kSensorDirectData);
    take(1ull << 24, kGpuDataBuffer);
    return usage != 0 ? 0 : flags;
}

struct Combination {
    Drm format;
    uint64_t flags;
};

// dumb_driver_init (dumb_driver.c), the vkms backend's table, with
// drv_modify_linear_combinations (drv_helpers.c) at its end.
std::vector<Combination> dumbDriverTable() {
    std::vector<Combination> table;
    for (Drm format : {Drm::Argb8888, Drm::Xrgb8888, Drm::Abgr8888, Drm::Xbgr8888, Drm::Bgr888, Drm::Rgb565}) {
        table.push_back({format, kRenderMask | kScanout});
    }
    for (Drm format : {Drm::R8, Drm::Nv12, Drm::Nv21, Drm::Yvu420, Drm::Yvu420Android}) {
        table.push_back({format, kTextureMask});
    }
    const auto modify = [&](Drm format, uint64_t flags) {
        for (Combination& combination : table) {
            if (combination.format == format) {
                combination.flags |= flags;
            }
        }
    };
    modify(Drm::R8, kVideoEncoder | kVideoDecoder | kCameraRead | kCameraWrite | kGpuDataBuffer | kSensorDirectData);
    modify(Drm::Nv12, kVideoEncoder | kVideoDecoder | kCameraRead | kCameraWrite);
    modify(Drm::Nv21, kVideoEncoder);
    modify(Drm::Xrgb8888, kCursor | kScanout);
    modify(Drm::Argb8888, kCursor | kScanout);
    return table;
}

// drv_resolve_format_and_use_flags_helper (drv_helpers.c), the dumb driver's.
void resolve(Drm* format, uint64_t* flags) {
    switch (*format) {
        case Drm::FlexImplementationDefined:
            if ((*flags & (kCameraRead | kCameraWrite)) != 0) {
                *format = Drm::Nv12;
            } else {
                *format = Drm::Xbgr8888;
                *flags &= ~kVideoEncoder;
            }
            break;
        case Drm::FlexYcbcr420:
            *format = Drm::Nv12;
            break;
        case Drm::Yvu420Android:
            *flags &= ~kScanout;
            break;
        default:
            break;
    }
}

// drv_get_combination (drv.c): a combination of the format that has every flag asked.
bool hasCombination(Drm format, uint64_t flags) {
    if (format == Drm::None || flags == 0) {
        return false;
    }
    for (const Combination& combination : dumbDriverTable()) {
        if (combination.format == format && (combination.flags & flags) == flags) {
            return true;
        }
    }
    return false;
}

// cros_gralloc_driver::get_resolved_format_and_use_flags (cros_gralloc/cros_gralloc_driver.cc):
// whether the allocator makes a buffer of `format` with `usage`. A format other than
// YCbCr_420_888 refused with a video encoder's bit is tried again without it.
bool allocates(int32_t format, uint64_t usage) {
    Drm drm = drmFormat(format);
    uint64_t flags = useFlags(usage);
    resolve(&drm, &flags);
    if (hasCombination(drm, flags)) {
        return true;
    }
    if ((usage & 0x10000) != 0 && format != kFormatYcbcr420) {
        return hasCombination(drm, flags & ~kVideoEncoder);
    }
    return false;
}

}  // namespace minigbm

// A stream an app opens: the format it asks for and its consumer's usage.
struct AppStream {
    const char* what;
    int32_t format;
    uint64_t consumerUsage;
};

// What Aperture and CameraX open, and the usage each one's consumer sets.
const AppStream kAppStreams[] = {
    {"a SurfaceView preview", kFormatImplementationDefined, 0x900},
    {"a TextureView preview", kFormatImplementationDefined, 0x100},
    {"a SurfaceView preview a video encoder shares", kFormatImplementationDefined, 0x10900},
    {"a video encoder's input surface", kFormatImplementationDefined, 0x10000},
    {"a video encoder's input surface the GPU samples", kFormatImplementationDefined, 0x10100},
    {"a private ImageReader", kFormatImplementationDefined, 0x3},
    {"ImageAnalysis's YUV ImageReader", kFormatYcbcr420, 0x3},
    {"a JPEG still's ImageReader", kFormatBlob, 0x3},
};

}  // namespace

TEST(fr26_the_allocator_rules_refuse_what_the_guest_refused) {
    // The rules match the guest's logcat: the RGBA preview with CAMERA_OUTPUT (usage 0x20930)
    // failed to allocate, and so did YUV for the same SurfaceView before it.
    EXPECT_TRUE(!minigbm::allocates(kFormatRgba8888, 0x20930));
    EXPECT_TRUE(!minigbm::allocates(kFormatYcbcr420, 0x20930));
    // While an app's own window buffers, and the video decoder's YV12 pictures, are made.
    EXPECT_TRUE(minigbm::allocates(kFormatRgba8888, 0xB00));
    EXPECT_TRUE(minigbm::allocates(kFormatRgba8888, 0x930));
    EXPECT_TRUE(minigbm::allocates(0x32315659, 0x933));
}

TEST(fr26_every_stream_an_app_opens_gets_buffers_the_allocator_makes) {
    for (const AppStream& stream : kAppStreams) {
        const int32_t format = overrideFormatFor(stream.format, stream.consumerUsage);
        const uint64_t usage = stream.consumerUsage | producerUsageFor(format);
        if (!minigbm::allocates(format, usage)) {
            ::dsa::test::fail(__FILE__, __LINE__,
                              std::string(stream.what) + ": the allocator makes no buffer of format " +
                                  std::to_string(format) + " with usage " + std::to_string(usage));
        }
    }
}

TEST(fr26_the_hal_writes_every_stream_by_cpu) {
    for (int32_t format : {kFormatRgba8888, kFormatYcbcr420, kFormatBlob}) {
        EXPECT_EQ(producerUsageFor(format) & kUsageCpuWriteOften, kUsageCpuWriteOften);
    }
    // CAMERA_OUTPUT stays on the formats the allocator makes with it, and off RGBA.
    EXPECT_EQ(producerUsageFor(kFormatRgba8888) & kUsageCameraOutput, 0u);
    EXPECT_EQ(producerUsageFor(kFormatYcbcr420) & kUsageCameraOutput, kUsageCameraOutput);
    EXPECT_EQ(producerUsageFor(kFormatBlob) & kUsageCameraOutput, kUsageCameraOutput);
}

TEST(fr26_a_stream_of_another_shape_is_cropped_at_the_centre_not_stretched) {
    const Crop wide = centerCrop(1280, 720, 640, 480);
    EXPECT_EQ(wide.width, 960u);
    EXPECT_EQ(wide.height, 720u);
    EXPECT_EQ(wide.x, 160u);
    EXPECT_EQ(wide.y, 0u);
    const Crop same = centerCrop(1280, 720, 320, 180);
    EXPECT_EQ(same.width, 1280u);
    EXPECT_EQ(same.x, 0u);
    // 8x2 luma 0..7 in each row, into a 2x2 buffer: a 2x2 region near the middle, which starts
    // at column 2 (3 made even, so it starts on a chroma sample).
    const uint8_t data[] = {0, 1, 2, 3, 4, 5, 6, 7, 0, 1, 2, 3, 4, 5, 6, 7,
                            10, 20, 11, 21, 12, 22, 13, 23};
    const Nv12 picture{data, sizeof(data), 8, 2, 8};
    std::vector<uint8_t> y(4, 0), u(1, 0), v(1, 0);
    YuvPlanes planes{{y.data(), 1, 2}, {u.data(), 1, 1}, {v.data(), 1, 1}};
    EXPECT_TRUE(scaleToYuv(picture, 2, 2, planes));
    EXPECT_EQ(y, (std::vector<uint8_t>{2, 3, 2, 3}));
    EXPECT_EQ(u[0], 11);
    EXPECT_EQ(v[0], 21);
}

TEST(fr26_android_camera_ids_stay_with_their_host_camera) {
    CameraIds ids;
    EXPECT_EQ(ids.idFor("front"), std::string("0"));
    EXPECT_EQ(ids.idFor("back"), std::string("1"));
    EXPECT_EQ(ids.idFor("front"), std::string("0"));
    EXPECT_EQ(ids.hostIdOf("1"), std::string("back"));
    EXPECT_EQ(ids.hostIdOf("7"), std::string());
}

TEST(fr26_a_picture_scales_into_a_smaller_yuv_buffer) {
    // 4x2: luma 1, 2, 3, 4 in each row; chroma pairs (20, 30) and (21, 31).
    const uint8_t data[] = {1, 2, 3, 4, 1, 2, 3, 4, 20, 30, 21, 31};
    const Nv12 picture{data, sizeof(data), 4, 2, 4};
    // Into 2x1, the picture's own shape: every other pixel of the first row.
    std::vector<uint8_t> y(2 * 1, 0), u(1, 0), v(1, 0);
    YuvPlanes planes{{y.data(), 1, 2}, {u.data(), 1, 1}, {v.data(), 1, 1}};
    EXPECT_TRUE(scaleToYuv(picture, 2, 1, planes));
    EXPECT_EQ(y, (std::vector<uint8_t>{1, 3}));
    EXPECT_EQ(u[0], 20);
    EXPECT_EQ(v[0], 30);
}

TEST(fr26_a_picture_of_its_own_size_copies_into_interleaved_chroma) {
    const uint8_t data[] = {1, 2, 3, 4, 5, 6, 7, 8, 20, 30, 21, 31};
    const Nv12 picture{data, sizeof(data), 4, 2, 4};
    std::vector<uint8_t> y(4 * 2, 0), uv(4, 0);
    YuvPlanes planes{{y.data(), 1, 4}, {uv.data(), 2, 4}, {uv.data() + 1, 2, 4}};
    EXPECT_TRUE(scaleToYuv(picture, 4, 2, planes));
    EXPECT_EQ(y, (std::vector<uint8_t>{1, 2, 3, 4, 5, 6, 7, 8}));
    EXPECT_EQ(uv, (std::vector<uint8_t>{20, 30, 21, 31}));
}

TEST(fr26_a_short_picture_is_not_copied) {
    const uint8_t data[] = {1, 2, 3};
    const Nv12 picture{data, sizeof(data), 4, 2, 4};
    std::vector<uint8_t> y(8, 0), u(2, 0), v(2, 0);
    YuvPlanes planes{{y.data(), 1, 4}, {u.data(), 1, 2}, {v.data(), 1, 2}};
    EXPECT_TRUE(!scaleToYuv(picture, 4, 2, planes));
    EXPECT_EQ(y, std::vector<uint8_t>(8, 0));
}

TEST(fr26_a_grey_picture_stays_grey_in_rgba) {
    const uint8_t data[] = {126, 126, 126, 126, 128, 128};
    const Nv12 picture{data, sizeof(data), 2, 2, 2};
    std::vector<uint8_t> rgba(2 * 2 * 4, 0);
    EXPECT_TRUE(scaleToRgba(picture, 2, 2, rgba.data(), 8));
    for (size_t i = 0; i < rgba.size(); ++i) {
        if (i % 4 == 3) {
            EXPECT_EQ(rgba[i], 255);
        } else {
            EXPECT_TRUE(rgba[i] >= 127 && rgba[i] <= 130);
        }
    }
}

TEST(fr26_a_still_carries_its_length_at_the_buffer_end) {
    const uint8_t jpeg[] = {0xFF, 0xD8, 0xFF, 0xD9};
    std::vector<uint8_t> buffer(32, 0);
    EXPECT_TRUE(writeJpegBlob(buffer.data(), buffer.size(), jpeg, sizeof(jpeg)));
    EXPECT_EQ(std::vector<uint8_t>(buffer.begin(), buffer.begin() + 4), (std::vector<uint8_t>{0xFF, 0xD8, 0xFF, 0xD9}));
    int32_t trailer[2] = {0, 0};
    std::memcpy(trailer, buffer.data() + buffer.size() - sizeof(trailer), sizeof(trailer));
    EXPECT_EQ(trailer[0], 0x00FF);
    EXPECT_EQ(trailer[1], 4);
    std::vector<uint8_t> small(10, 0);
    EXPECT_TRUE(!writeJpegBlob(small.data(), small.size(), jpeg, sizeof(jpeg)));
}

TEST(fr26_a_frame_has_its_length_in_front) {
    EXPECT_EQ(withLength({1, 2, 3}), (std::vector<uint8_t>{3, 0, 0, 0, 1, 2, 3}));
}

int main(int argc, char** argv) {
    if (argc != 2) {
        std::fprintf(stderr, "usage: %s <dsa/protocol>\n", argv[0]);
        return 2;
    }
    gProtocolDir = argv[1];
    return ::dsa::test::runAll();
}
