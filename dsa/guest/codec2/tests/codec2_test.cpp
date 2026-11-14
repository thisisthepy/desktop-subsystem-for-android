// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The codec HAL's rules: its video port messages against the checked-in protocol vectors, so
// they stay in step with the host's, and the copy of a decoded picture into a guest buffer's
// planes. The protocol folder is the only argument.

#include <sys/socket.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <fstream>
#include <memory>
#include <mutex>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

#include "arrival_queue.h"
#include "bounded_thread.h"
#include "config_rules.h"
#include "host_link.h"
#include "output_buffer.h"
#include "output_gate.h"
#include "picture_copy.h"
#include "picture_stats.h"
#include "send_queue.h"
#include "test_main.h"
#include "video_messages.h"

using namespace dsa::codec2;

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

}  // namespace

TEST(pr4_the_open_request_encodes_as_the_protocol_vector) {
    EXPECT_EQ(encodeOpen(Codec::H264, 1280, 720), vector("video_decoder_open"));
}

TEST(pr4_a_packet_encodes_as_the_protocol_vector) {
    const uint8_t data[] = {0, 0, 0, 1, 0x65};
    EXPECT_EQ(encodePacket(3, 100000, data, sizeof(data), false, false), vector("video_packet"));
}

TEST(pr4_a_flush_encodes_as_the_protocol_vector) { EXPECT_EQ(encodeFlush(), vector("video_flush")); }

TEST(pr4_the_hosts_capabilities_decode) {
    const std::vector<uint8_t> bytes = vector("video_capabilities");
    const HostMessage message = decodeHostFrame(bytes.data(), bytes.size());
    const auto* caps = std::get_if<Capabilities>(&message);
    EXPECT_TRUE(caps != nullptr);
    if (caps) {
        EXPECT_EQ(caps->codecs.size(), 1u);
        EXPECT_TRUE(caps->codecs[0].codec == Codec::H264);
        EXPECT_EQ(caps->codecs[0].maxWidth, 4096u);
        EXPECT_EQ(caps->codecs[0].maxHeight, 2304u);
    }
}

TEST(pr4_a_decoded_picture_decodes) {
    const std::vector<uint8_t> bytes = vector("video_frame");
    const HostMessage message = decodeHostFrame(bytes.data(), bytes.size());
    const auto* frame = std::get_if<Frame>(&message);
    EXPECT_TRUE(frame != nullptr);
    if (frame) {
        EXPECT_EQ(frame->frameIndex, 3u);
        EXPECT_EQ(frame->timestampUs, 100000);
        EXPECT_EQ(frame->width, 2u);
        EXPECT_EQ(frame->height, 2u);
        EXPECT_EQ(frame->stride, 2u);
        EXPECT_EQ(frame->nv12, std::string("\x10\x10\x10\x10\x80\x80", 6));
        EXPECT_TRUE(!frame->endOfStream);
    }
}

TEST(pr4_a_frame_has_its_length_in_front) {
    EXPECT_EQ(withLength({1, 2, 3}), (std::vector<uint8_t>{3, 0, 0, 0, 1, 2, 3}));
}

TEST(pr4_a_picture_copies_into_separate_chroma_planes) {
    // 4x2 NV12: luma rows 1..4 and 5..8, one chroma row Cb 20 Cr 30, Cb 21 Cr 31.
    const uint8_t nv12[] = {1, 2, 3, 4, 5, 6, 7, 8, 20, 30, 21, 31};
    std::vector<uint8_t> y(8 * 2, 0), u(4, 0), v(4, 0);
    YuvPlanes planes{{y.data(), 1, 8}, {u.data(), 1, 4}, {v.data(), 1, 4}};
    EXPECT_TRUE(copyNv12(nv12, sizeof(nv12), 4, 2, 4, planes));
    EXPECT_EQ(std::vector<uint8_t>(y.begin(), y.begin() + 4), (std::vector<uint8_t>{1, 2, 3, 4}));
    EXPECT_EQ(std::vector<uint8_t>(y.begin() + 8, y.begin() + 12), (std::vector<uint8_t>{5, 6, 7, 8}));
    EXPECT_EQ(u[0], 20);
    EXPECT_EQ(u[1], 21);
    EXPECT_EQ(v[0], 30);
    EXPECT_EQ(v[1], 31);
}

TEST(pr4_a_picture_copies_into_interleaved_chroma) {
    const uint8_t nv12[] = {1, 2, 3, 4, 5, 6, 7, 8, 20, 30, 21, 31};
    std::vector<uint8_t> y(4 * 2, 0), uv(4, 0);
    YuvPlanes planes{{y.data(), 1, 4}, {uv.data(), 2, 4}, {uv.data() + 1, 2, 4}};
    EXPECT_TRUE(copyNv12(nv12, sizeof(nv12), 4, 2, 4, planes));
    EXPECT_EQ(uv, (std::vector<uint8_t>{20, 30, 21, 31}));
}

TEST(pr4_a_picture_too_short_for_its_size_is_not_copied) {
    const uint8_t nv12[] = {1, 2, 3};
    std::vector<uint8_t> y(8, 0), u(2, 0), v(2, 0);
    YuvPlanes planes{{y.data(), 1, 4}, {u.data(), 1, 2}, {v.data(), 1, 2}};
    EXPECT_TRUE(!copyNv12(nv12, sizeof(nv12), 4, 2, 4, planes));
    EXPECT_EQ(y, std::vector<uint8_t>(8, 0));
}

// A YV12 buffer as the gralloc lays one out: the Cr plane, then the Cb plane, each row of
// `chromaStride` bytes.
struct Yv12 {
    std::vector<uint8_t> y, v, u;
    uint32_t yStride, chromaStride;
    Yv12(uint32_t width, uint32_t height)
        : y(static_cast<size_t>(((width + 15) & ~15u)) * height, 0xee),
          v(static_cast<size_t>((((width + 1) / 2) + 15) & ~15u) * ((height + 1) / 2), 0xee),
          u(v.size(), 0xee),
          yStride((width + 15) & ~15u),
          chromaStride((((width + 1) / 2) + 15) & ~15u) {}
    YuvPlanes planes() { return {{y.data(), 1, yStride}, {u.data(), 1, chromaStride}, {v.data(), 1, chromaStride}}; }
};

// An NV12 picture of one colour: luma `luma`, chroma `cb`, `cr`.
std::vector<uint8_t> flatNv12(uint32_t width, uint32_t height, uint8_t luma, uint8_t cb, uint8_t cr) {
    const uint32_t stride = width + (width & 1);
    std::vector<uint8_t> nv12(static_cast<size_t>(stride) * (height + (height + 1) / 2), luma);
    for (size_t i = static_cast<size_t>(stride) * height; i < nv12.size(); i += 2) {
        nv12[i] = cb;
        nv12[i + 1] = cr;
    }
    return nv12;
}

TEST(fr26_a_426x240_picture_fills_its_yv12_buffer_with_its_chroma) {
    const std::vector<uint8_t> nv12 = flatNv12(426, 240, 81, 90, 240);
    Yv12 buffer(426, 240);
    EXPECT_TRUE(copyNv12(nv12.data(), nv12.size(), 426, 240, 426, buffer.planes(), 426, 240));
    for (uint32_t row = 0; row < 120; ++row) {
        for (uint32_t x = 0; x < 213; ++x) {
            if (buffer.u[row * buffer.chromaStride + x] != 90 || buffer.v[row * buffer.chromaStride + x] != 240) {
                ::dsa::test::fail(__FILE__, __LINE__, "chroma lost at row " + std::to_string(row));
                return;
            }
        }
    }
    EXPECT_EQ(buffer.y[239 * buffer.yStride + 425], 81);
}

TEST(fr26_a_buffer_larger_than_the_picture_takes_its_edge_not_zero) {
    // A 5x3 picture in a 6x4 buffer: the extra column and row repeat the edge.
    const std::vector<uint8_t> nv12 = flatNv12(5, 3, 81, 90, 240);
    Yv12 buffer(6, 4);
    std::fill(buffer.y.begin(), buffer.y.end(), 0);
    std::fill(buffer.u.begin(), buffer.u.end(), 0);
    std::fill(buffer.v.begin(), buffer.v.end(), 0);
    EXPECT_TRUE(copyNv12(nv12.data(), nv12.size(), 5, 3, 6, buffer.planes(), 6, 4));
    for (uint32_t row = 0; row < 4; ++row) {
        for (uint32_t x = 0; x < 6; ++x) {
            EXPECT_EQ(buffer.y[row * buffer.yStride + x], 81);
        }
    }
    for (uint32_t row = 0; row < 2; ++row) {
        for (uint32_t x = 0; x < 3; ++x) {
            EXPECT_EQ(buffer.u[row * buffer.chromaStride + x], 90);
            EXPECT_EQ(buffer.v[row * buffer.chromaStride + x], 240);
        }
    }
    // A buffer smaller than the picture is refused.
    EXPECT_TRUE(!copyNv12(nv12.data(), nv12.size(), 5, 3, 6, buffer.planes(), 4, 3));
}

TEST(fr26_output_buffers_are_yv12_at_the_pictures_size_rounded_to_two) {
    EXPECT_EQ(kOutputFormat, 0x32315659u);
    const PictureSize even = bufferSizeFor(360, 640);
    EXPECT_EQ(even.width, 360u);
    EXPECT_EQ(even.height, 640u);
    const PictureSize odd = bufferSizeFor(425, 239);
    EXPECT_EQ(odd.width, 426u);
    EXPECT_EQ(odd.height, 240u);
}

TEST(fr26_the_crop_is_the_visible_picture_inside_its_buffer) {
    const Crop crop = cropFor(425, 239, bufferSizeFor(425, 239));
    EXPECT_EQ(crop.left, 0u);
    EXPECT_EQ(crop.top, 0u);
    EXPECT_EQ(crop.width, 425u);
    EXPECT_EQ(crop.height, 239u);
    const Crop clamped = cropFor(1920, 1080, PictureSize{1280, 720});
    EXPECT_EQ(clamped.width, 1280u);
    EXPECT_EQ(clamped.height, 720u);
}

TEST(fr26_a_surface_with_every_buffer_in_use_is_waited_for) {
    // The framework's allocator answers with positive c2_status_t values.
    EXPECT_TRUE(classifyAllocateError(11) == AllocateFailure::Wait);    // C2_BLOCKING
    EXPECT_TRUE(classifyAllocateError(110) == AllocateFailure::Wait);   // C2_TIMED_OUT
    EXPECT_TRUE(classifyAllocateError(1) == AllocateFailure::Stopped);  // C2_BAD_STATE
    EXPECT_TRUE(classifyAllocateError(14) == AllocateFailure::Failed);  // C2_CORRUPTED
    EXPECT_TRUE(classifyAllocateError(12) == AllocateFailure::Failed);  // C2_NO_MEMORY
    // The AIDL Status enum's BLOCKING, which the allocator never sends, is not a wait.
    EXPECT_TRUE(classifyAllocateError(-9930) == AllocateFailure::Failed);
}

TEST(fr26_a_failure_on_every_picture_is_logged_once_in_a_while) {
    FailureThrottle throttle(100);
    uint32_t count = 0;
    int logged = 0;
    for (int i = 0; i < 250; ++i) {
        logged += throttle.shouldLog(&count) ? 1 : 0;
    }
    EXPECT_EQ(count, 250u);
    EXPECT_EQ(logged, 3);  // the 1st, the 100th and the 200th
    throttle.reset();
    EXPECT_TRUE(throttle.shouldLog(&count));
    EXPECT_EQ(count, 1u);
}

TEST(fr26_a_surface_mode_configure_takes_the_surfaces_pixel_format) {
    // The pixel formats CCodecConfig sets on a video decoder's output, from the app's color
    // format: COLOR_FormatSurface for a surface, the flexible YUV formats for buffers.
    struct Request {
        uint32_t value;
        bool accepted;
    };
    const Request requests[] = {
        {0x22, true},        // HAL_PIXEL_FORMAT_IMPLEMENTATION_DEFINED: a surface
        {0x23, true},        // HAL_PIXEL_FORMAT_YCBCR_420_888: buffers the app reads
        {0x36, false},       // HAL_PIXEL_FORMAT_YCBCR_P010: 10-bit, which the decoders do not give
        {0x7f000789, false}, // a framework color format that is not a pixel format
    };
    for (const Request& request : requests) {
        uint32_t kept = 0;
        const bool accepted = acceptPixelFormat(request.value, &kept);
        EXPECT_EQ(accepted, request.accepted);
        if (accepted) {
            // The decoders always output flexible YUV, which a surface takes as well.
            EXPECT_EQ(kept, kPixelFormatFlexible);
        }
    }
    EXPECT_EQ(kPixelFormatSurface, 0x22u);
    EXPECT_EQ(kPixelFormatFlexible, 0x23u);
}

// A buffer the writer is handed: YV12 planes of `width` by `height`, every sample 0xee until
// written, with what the lock and the unlock should answer.
struct FakeBuffer {
    Yv12 planes;
    bool lockOk = true;
    uint32_t planeCount = 3;
    bool unlockOk = true;
    int locks = 0;
    int unlocks = 0;
    explicit FakeBuffer(uint32_t width, uint32_t height) : planes(width, height) {}
    LockPlanes lock() {
        return [this](YuvPlanes* out, uint32_t* count) {
            ++locks;
            if (!lockOk) {
                return false;
            }
            *out = planes.planes();
            *count = planeCount;
            return true;
        };
    }
    UnlockPlanes unlock() {
        return [this] {
            ++unlocks;
            return unlockOk;
        };
    }
};

TEST(fr26_a_written_picture_fills_every_sample_of_a_larger_buffer) {
    // An allocator may give a buffer larger than asked; nothing of it is left unwritten.
    const std::vector<uint8_t> nv12 = flatNv12(6, 4, 81, 90, 240);
    FakeBuffer buffer(8, 6);
    const WriteResult result =
        writePicture(nv12.data(), nv12.size(), 6, 4, 6, BufferSize{8, 6}, buffer.lock(), buffer.unlock());
    EXPECT_TRUE(result == WriteResult::Written);
    EXPECT_EQ(buffer.unlocks, 1);
    for (uint32_t row = 0; row < 6; ++row) {
        for (uint32_t x = 0; x < 8; ++x) {
            EXPECT_EQ(buffer.planes.y[row * buffer.planes.yStride + x], 81);
        }
    }
    for (uint32_t row = 0; row < 3; ++row) {
        for (uint32_t x = 0; x < 4; ++x) {
            EXPECT_EQ(buffer.planes.u[row * buffer.planes.chromaStride + x], 90);
            EXPECT_EQ(buffer.planes.v[row * buffer.planes.chromaStride + x], 240);
        }
    }
}

TEST(fr26_a_buffer_that_cannot_be_locked_never_goes_out) {
    // Before, the buffer went to the surface anyway: zeros (green) or an older picture.
    const std::vector<uint8_t> nv12 = flatNv12(4, 2, 81, 90, 240);
    FakeBuffer buffer(4, 2);
    buffer.lockOk = false;
    EXPECT_TRUE(writePicture(nv12.data(), nv12.size(), 4, 2, 4, BufferSize{4, 2}, buffer.lock(), buffer.unlock()) ==
                WriteResult::NotLocked);
    EXPECT_EQ(buffer.unlocks, 0);
}

TEST(fr26_a_picture_too_short_for_its_size_never_goes_out) {
    const std::vector<uint8_t> nv12 = flatNv12(4, 2, 81, 90, 240);
    FakeBuffer buffer(4, 2);
    // One byte short of its chroma.
    EXPECT_TRUE(writePicture(nv12.data(), nv12.size() - 1, 4, 2, 4, BufferSize{4, 2}, buffer.lock(),
                             buffer.unlock()) == WriteResult::NotCopied);
    // Locked, so unlocked again, and nothing written.
    EXPECT_EQ(buffer.unlocks, 1);
    EXPECT_EQ(buffer.planes.y, std::vector<uint8_t>(buffer.planes.y.size(), 0xee));
}

TEST(fr26_a_buffer_smaller_than_the_picture_is_not_written) {
    const std::vector<uint8_t> nv12 = flatNv12(6, 4, 81, 90, 240);
    FakeBuffer buffer(6, 4);
    EXPECT_TRUE(writePicture(nv12.data(), nv12.size(), 6, 4, 6, BufferSize{4, 4}, buffer.lock(), buffer.unlock()) ==
                WriteResult::WrongBuffer);
    EXPECT_TRUE(writePicture(nv12.data(), nv12.size(), 6, 4, 6, BufferSize{6, 2}, buffer.lock(), buffer.unlock()) ==
                WriteResult::WrongBuffer);
    EXPECT_EQ(buffer.locks, 0);
}

TEST(fr26_a_buffer_whose_planes_do_not_fit_is_not_written) {
    const std::vector<uint8_t> nv12 = flatNv12(4, 2, 81, 90, 240);
    FakeBuffer twoPlanes(4, 2);
    twoPlanes.planeCount = 2;
    EXPECT_TRUE(writePicture(nv12.data(), nv12.size(), 4, 2, 4, BufferSize{4, 2}, twoPlanes.lock(),
                             twoPlanes.unlock()) == WriteResult::BadPlanes);
    EXPECT_EQ(twoPlanes.unlocks, 1);
    // A lock whose rows are shorter than the buffer's width.
    FakeBuffer narrow(4, 2);
    narrow.planes.yStride = 3;
    EXPECT_TRUE(writePicture(nv12.data(), nv12.size(), 4, 2, 4, BufferSize{4, 2}, narrow.lock(), narrow.unlock()) ==
                WriteResult::BadPlanes);
    EXPECT_EQ(narrow.planes.y, std::vector<uint8_t>(narrow.planes.y.size(), 0xee));
}

TEST(fr26_a_buffer_whose_unlock_failed_never_goes_out) {
    const std::vector<uint8_t> nv12 = flatNv12(4, 2, 81, 90, 240);
    FakeBuffer buffer(4, 2);
    buffer.unlockOk = false;
    EXPECT_TRUE(writePicture(nv12.data(), nv12.size(), 4, 2, 4, BufferSize{4, 2}, buffer.lock(), buffer.unlock()) ==
                WriteResult::NotUnlocked);
}

TEST(fr26_a_picture_that_arrives_during_a_flush_is_dropped_at_once) {
    OutputGate gate;
    const uint64_t before = gate.ticket();
    EXPECT_TRUE(gate.open(before));
    gate.beginFlush();
    // One that came before the flush, and one still on its way to the host's answer.
    EXPECT_TRUE(!gate.open(before));
    EXPECT_TRUE(!gate.open(gate.ticket()));
    gate.endFlush();
    // The stream after the flush goes out; the one before it never does.
    EXPECT_TRUE(gate.open(gate.ticket()));
    EXPECT_TRUE(!gate.open(before));
    const uint64_t started = gate.ticket();
    gate.stop();
    EXPECT_TRUE(!gate.open(started));
    EXPECT_TRUE(gate.open(gate.ticket()));
}

TEST(fr26_a_flush_ends_a_pictures_wait_for_a_buffer_at_once) {
    // A surface whose buffers never come back: the waitable never becomes readable.
    int fds[2];
    EXPECT_EQ(pipe(fds), 0);
    OutputGate gate;
    const uint64_t ticket = gate.ticket();
    const auto start = std::chrono::steady_clock::now();
    std::thread flush([&gate] {
        std::this_thread::sleep_for(std::chrono::milliseconds(100));
        gate.beginFlush();
    });
    const WaitResult result = waitReadable(fds[0], start + std::chrono::seconds(10), gate, ticket);
    flush.join();
    const auto took = std::chrono::steady_clock::now() - start;
    EXPECT_TRUE(result == WaitResult::Closed);
    // Before, the wait lasted its full 2 s for each picture still queued, and the flush with it.
    EXPECT_TRUE(took < std::chrono::seconds(1));
    close(fds[0]);
    close(fds[1]);
}

TEST(fr26_a_buffer_still_in_use_is_waited_for_and_then_given_up) {
    int fds[2];
    EXPECT_EQ(pipe(fds), 0);
    OutputGate gate;
    // Its release fence never signals: given up at the deadline, not written into.
    EXPECT_TRUE(waitReadable(fds[0], std::chrono::steady_clock::now() + std::chrono::milliseconds(120), gate,
                             gate.ticket()) == WaitResult::TimedOut);
    // Once it signals, the buffer is free to write.
    const char signal = 1;
    EXPECT_EQ(write(fds[1], &signal, 1), 1);
    EXPECT_TRUE(waitReadable(fds[0], std::chrono::steady_clock::now() + std::chrono::seconds(5), gate,
                             gate.ticket()) == WaitResult::Ready);
    close(fds[0]);
    close(fds[1]);
}

TEST(fr26_a_packet_queues_without_waiting_for_the_host_to_read) {
    // Nothing writes to the host: before, the framework's queue() call waited on the socket
    // while the host waited for the guest to take a picture (8 s calls on the owner's PC).
    SendQueue queue(1024);
    const auto start = std::chrono::steady_clock::now();
    for (int i = 0; i < 8; ++i) {
        EXPECT_TRUE(queue.push(std::vector<uint8_t>(100, static_cast<uint8_t>(i))));
    }
    EXPECT_TRUE(std::chrono::steady_clock::now() - start < std::chrono::milliseconds(500));
    EXPECT_EQ(queue.queuedBytes(), 800u);
    // They leave in the order they came.
    std::vector<uint8_t> frame;
    for (int i = 0; i < 8; ++i) {
        EXPECT_TRUE(queue.pop(&frame));
        EXPECT_EQ(frame[0], static_cast<uint8_t>(i));
    }
    EXPECT_EQ(queue.queuedBytes(), 0u);
}

TEST(fr26_a_closed_send_queue_wakes_its_writer_and_takes_nothing_more) {
    SendQueue queue(1024);
    std::atomic<bool> popped{true};
    std::thread writer([&] {
        std::vector<uint8_t> frame;
        popped = queue.pop(&frame);
    });
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    queue.close();
    writer.join();
    EXPECT_TRUE(!popped.load());
    EXPECT_TRUE(!queue.push({1, 2, 3}));
}

TEST(fr26_a_full_send_queue_waits_for_room_and_a_close_ends_the_wait) {
    SendQueue queue(100);
    EXPECT_TRUE(queue.push(std::vector<uint8_t>(80, 0)));
    // A frame larger than the bound still goes when nothing waits.
    SendQueue big(10);
    EXPECT_TRUE(big.push(std::vector<uint8_t>(50, 0)));
    std::atomic<bool> pushed{true};
    std::thread sender([&] { pushed = queue.push(std::vector<uint8_t>(80, 1)); });
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    queue.close();
    sender.join();
    EXPECT_TRUE(!pushed.load());
}


namespace {

// A YV12 buffer as minigbm allocates one for a surface: one allocation, the luma rows
// `ALIGN(width, 32)` bytes apart, then the Cr plane, then the Cb plane, each chroma row
// `ALIGN(lumaStride / 2, 16)` bytes apart, for `rows` rows (which may be more than the
// picture's). Every byte starts as zero, which reads as green.
struct GrallocYv12 {
    uint32_t width, rows, yStride, cStride;
    size_t crOffset, cbOffset;
    std::vector<uint8_t> memory;
    GrallocYv12(uint32_t w, uint32_t r)
        : width(w),
          rows(r),
          yStride((w + 31) & ~31u),
          cStride(((((w + 31) & ~31u) / 2) + 15) & ~15u),
          crOffset(static_cast<size_t>(yStride) * r),
          cbOffset(crOffset + static_cast<size_t>(cStride) * ((r + 1) / 2)),
          memory(cbOffset + static_cast<size_t>(cStride) * ((r + 1) / 2), 0) {}
    // As AHardwareBuffer_lockPlanes gives them: Y, Cb, Cr.
    YuvPlanes planes() {
        return {{memory.data(), 1, yStride}, {memory.data() + cbOffset, 1, cStride}, {memory.data() + crOffset, 1, cStride}};
    }
    uint8_t y(uint32_t row, uint32_t x) const { return memory[static_cast<size_t>(row) * yStride + x]; }
    uint8_t cb(uint32_t row, uint32_t x) const { return memory[cbOffset + static_cast<size_t>(row) * cStride + x]; }
    uint8_t cr(uint32_t row, uint32_t x) const { return memory[crOffset + static_cast<size_t>(row) * cStride + x]; }
};

// An NV12 picture whose every row says which row it is: luma `row % 200 + 16`, chroma Cb
// `crow % 100 + 50`, Cr `crow % 100 + 150`. A picture written a row off, or with its chroma
// taken from the wrong rows, does not match.
std::vector<uint8_t> rowNumberedNv12(uint32_t width, uint32_t height) {
    const uint32_t stride = width + (width & 1);
    const uint32_t chromaRows = (height + 1) / 2;
    std::vector<uint8_t> nv12(static_cast<size_t>(stride) * (height + chromaRows));
    for (uint32_t row = 0; row < height; ++row) {
        std::fill_n(nv12.begin() + static_cast<size_t>(row) * stride, stride, static_cast<uint8_t>(row % 200 + 16));
    }
    for (uint32_t row = 0; row < chromaRows; ++row) {
        uint8_t* line = nv12.data() + static_cast<size_t>(stride) * (height + row);
        for (uint32_t x = 0; x + 1 < stride; x += 2) {
            line[x] = static_cast<uint8_t>(row % 100 + 50);
            line[x + 1] = static_cast<uint8_t>(row % 100 + 150);
        }
    }
    return nv12;
}

// Checks every sample of `buffer` against the row-numbered picture of `width` by `height`,
// the rows past the picture repeating its last. Returns the first wrong place, or "".
std::string firstWrongSample(const GrallocYv12& buffer, uint32_t width, uint32_t height) {
    for (uint32_t row = 0; row < buffer.rows; ++row) {
        const uint32_t from = std::min(row, height - 1);
        for (uint32_t x = 0; x < buffer.width; ++x) {
            if (buffer.y(row, x) != static_cast<uint8_t>(from % 200 + 16)) {
                return "luma row " + std::to_string(row) + " column " + std::to_string(x);
            }
        }
    }
    for (uint32_t row = 0; row < (buffer.rows + 1) / 2; ++row) {
        const uint32_t from = std::min(row, (height + 1) / 2 - 1);
        for (uint32_t x = 0; x < (buffer.width + 1) / 2; ++x) {
            if (buffer.cb(row, x) != static_cast<uint8_t>(from % 100 + 50) ||
                buffer.cr(row, x) != static_cast<uint8_t>(from % 100 + 150)) {
                return "chroma row " + std::to_string(row) + " column " + std::to_string(x);
            }
        }
    }
    (void)width;
    return "";
}

#define EXPECT_ALL_SAMPLES(buffer, width, height)                                      \
    do {                                                                               \
        const std::string wrong = firstWrongSample(buffer, width, height);             \
        if (!wrong.empty()) ::dsa::test::fail(__FILE__, __LINE__, "wrong at " + wrong); \
    } while (0)

WriteResult writeInto(GrallocYv12& buffer, const std::vector<uint8_t>& nv12, uint32_t width, uint32_t height) {
    return writePicture(
        nv12.data(), nv12.size(), width, height, width + (width & 1), BufferSize{buffer.width, buffer.rows},
        [&buffer](YuvPlanes* planes, uint32_t* count) {
            *planes = buffer.planes();
            *count = 3;
            return true;
        },
        [] { return true; });
}

}  // namespace

TEST(fr26_a_portrait_short_fills_its_gralloc_yv12_buffer_from_its_top_row) {
    // A 360x640 Shorts picture: minigbm's luma rows are 384 bytes and its chroma rows 192, so
    // stride and width differ, and every row from the first must be the picture's own.
    GrallocYv12 buffer(360, 640);
    const std::vector<uint8_t> nv12 = rowNumberedNv12(360, 640);
    EXPECT_TRUE(writeInto(buffer, nv12, 360, 640) == WriteResult::Written);
    EXPECT_ALL_SAMPLES(buffer, 360, 640);
    EXPECT_EQ(buffer.y(0, 0), 16);
    EXPECT_EQ(buffer.cb(0, 0), 50);
    EXPECT_EQ(buffer.cr(0, 0), 150);
}

TEST(fr26_a_picture_in_a_taller_buffer_keeps_its_rows_and_chroma_planes) {
    // A 640x360 picture in a buffer an allocator made 368 rows tall: the chroma planes start
    // after the buffer's rows, not the picture's, and the 8 rows past the picture repeat its
    // last row rather than staying zero (green).
    GrallocYv12 buffer(640, 368);
    const std::vector<uint8_t> nv12 = rowNumberedNv12(640, 360);
    EXPECT_TRUE(writeInto(buffer, nv12, 640, 360) == WriteResult::Written);
    EXPECT_ALL_SAMPLES(buffer, 640, 360);
}

TEST(fr26_pictures_wait_in_order_while_the_one_before_waits_for_its_buffer) {
    ArrivalQueue queue(3, 1 << 20);
    const auto picture = [](uint64_t index) {
        Frame frame;
        frame.frameIndex = index;
        frame.nv12.assign(100, static_cast<char>(index));
        return Arrival{HostMessage{frame}, 0, std::chrono::steady_clock::now()};
    };
    // The reader puts three without waiting for the writer.
    const auto start = std::chrono::steady_clock::now();
    for (uint64_t i = 1; i <= 3; ++i) {
        EXPECT_TRUE(queue.push(picture(i)));
    }
    EXPECT_TRUE(std::chrono::steady_clock::now() - start < std::chrono::milliseconds(500));
    EXPECT_EQ(queue.size(), 3u);
    // A fourth waits for room, which the writer makes by taking the oldest.
    std::atomic<bool> pushed{false};
    std::thread reader([&] { pushed = queue.push(picture(4)); });
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    EXPECT_TRUE(!pushed.load());
    Arrival taken;
    EXPECT_TRUE(queue.pop(&taken));
    reader.join();
    EXPECT_TRUE(pushed.load());
    EXPECT_EQ(std::get<Frame>(taken.message).frameIndex, 1u);
    for (uint64_t i = 2; i <= 4; ++i) {
        EXPECT_TRUE(queue.pop(&taken));
        EXPECT_EQ(std::get<Frame>(taken.message).frameIndex, i);
    }
    // A stop wakes a writer that waits, and takes nothing more; a start opens it again.
    std::atomic<bool> popped{true};
    std::thread writer([&] { popped = queue.pop(&taken); });
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    queue.close();
    writer.join();
    EXPECT_TRUE(!popped.load());
    EXPECT_TRUE(!queue.push(picture(5)));
    queue.open();
    EXPECT_TRUE(queue.push(picture(6)));
    EXPECT_TRUE(queue.pop(&taken));
    EXPECT_EQ(std::get<Frame>(taken.message).frameIndex, 6u);
}

TEST(fr26_waiting_pictures_are_bounded_by_their_bytes) {
    ArrivalQueue queue(8, 250);
    const auto picture = [](size_t bytes) {
        Frame frame;
        frame.nv12.assign(bytes, 0);
        return Arrival{HostMessage{frame}, 0, std::chrono::steady_clock::now()};
    };
    // One picture larger than the bound still goes, alone.
    EXPECT_TRUE(queue.push(picture(1000)));
    std::atomic<bool> pushed{false};
    std::thread reader([&] { pushed = queue.push(picture(100)); });
    std::this_thread::sleep_for(std::chrono::milliseconds(50));
    EXPECT_TRUE(!pushed.load());
    Arrival taken;
    EXPECT_TRUE(queue.pop(&taken));
    reader.join();
    EXPECT_TRUE(pushed.load());
    // An error carries no picture and takes no bytes.
    EXPECT_TRUE(queue.push(picture(100)));
    EXPECT_TRUE(queue.push(Arrival{HostMessage{DecodeError{"bad"}}, 0, std::chrono::steady_clock::now()}));
    EXPECT_EQ(queue.size(), 3u);
}

TEST(fr26_a_decoders_numbers_go_to_the_log_every_few_seconds) {
    using namespace std::chrono;
    PictureStats stats(seconds(5));
    const auto start = steady_clock::now();
    std::string line;
    // Nothing has happened: nothing to say.
    EXPECT_TRUE(!stats.report(start + seconds(10), 0, &line));
    for (int i = 0; i < 150; ++i) {
        const auto at = start + milliseconds(33 * i);
        stats.received(at);
        if (i == 0) {
            stats.dropped(at, Drop::Flushed);
        } else if (i == 1) {
            stats.dropped(at, Drop::NoBuffer);
        } else {
            PictureTimes times;
            times.queued = milliseconds(2);
            times.buffer = milliseconds(12);
            times.write = milliseconds(1);
            times.handOff = microseconds(500);
            stats.shown(at, times);
        }
    }
    EXPECT_TRUE(!stats.report(start + seconds(4), 2, &line));
    EXPECT_TRUE(stats.report(start + seconds(5), 2, &line));
    EXPECT_EQ(line,
              std::string("over 5.0 s: 150 pictures from the host (30.0/s), 148 shown (29.6/s), 2 dropped (1 flushed, "
                          "1 with no buffer, 0 not written, 0 before a key frame); average ms: queued 2.00, buffer "
                          "wait 12.00, write 1.00, hand-off 0.50; 2 waiting"));
    // A new count starts from the report.
    EXPECT_TRUE(!stats.report(start + seconds(6), 0, &line));
}

namespace {

using std::chrono::milliseconds;
using std::chrono::steady_clock;

// Waits until `done` holds or `wait` passes; true when it held.
template <typename Done>
bool waitUntil(Done done, milliseconds wait = milliseconds(5000)) {
    const auto deadline = steady_clock::now() + wait;
    while (!done()) {
        if (steady_clock::now() > deadline) {
            return false;
        }
        std::this_thread::sleep_for(milliseconds(5));
    }
    return true;
}

// Writes one host frame with its length in front; false (and no SIGPIPE) once the other end
// has closed.
bool writeHostFrame(int fd, const std::vector<uint8_t>& frame) {
    const std::vector<uint8_t> bytes = withLength(frame);
    return send(fd, bytes.data(), bytes.size(), MSG_NOSIGNAL) == static_cast<ssize_t>(bytes.size());
}

}  // namespace

TEST(fr26_a_frame_with_no_picture_says_its_packet_was_not_decoded) {
    // The host's answer to a packet before a key frame: frame index 3 at 100000 us, nothing else.
    const std::vector<uint8_t> none = {0xe2, 0x07, 0x06, 0x08, 0x03, 0x10, 0xa0, 0x8d, 0x06};
    const HostMessage message = decodeHostFrame(none.data(), none.size());
    const auto* frame = std::get_if<Frame>(&message);
    EXPECT_TRUE(frame != nullptr);
    if (frame) {
        EXPECT_EQ(frame->frameIndex, 3u);
        EXPECT_TRUE(carriesNoPicture(*frame));
    }
    // A picture, and the end of the stream, are not that.
    const std::vector<uint8_t> picture = vector("video_frame");
    const HostMessage decoded = decodeHostFrame(picture.data(), picture.size());
    EXPECT_TRUE(!carriesNoPicture(std::get<Frame>(decoded)));
    Frame end;
    end.endOfStream = true;
    EXPECT_TRUE(!carriesNoPicture(end));
}

TEST(fr26_a_flush_takes_the_place_of_the_packets_still_unsent) {
    SendQueue queue(1 << 20);
    EXPECT_TRUE(queue.pushFor({1}, false, milliseconds(10)) == PushResult::Queued);  // configuration
    for (uint8_t i = 2; i <= 4; ++i) {
        EXPECT_TRUE(queue.pushFor({i}, true, milliseconds(10)) == PushResult::Queued);  // pictures
    }
    // The host would decode the three before it read the flush, and the flush waited for them.
    EXPECT_EQ(queue.pushReplacingDroppable({9}), 3);
    std::vector<uint8_t> frame;
    EXPECT_TRUE(queue.pop(&frame));
    EXPECT_EQ(frame[0], 1);
    EXPECT_TRUE(queue.pop(&frame));
    EXPECT_EQ(frame[0], 9);
    EXPECT_EQ(queue.queuedBytes(), 0u);
    queue.close();
    EXPECT_EQ(queue.pushReplacingDroppable({9}), -1);
}

TEST(fr26_queue_returns_when_the_host_has_stopped_reading) {
    // Full, and nothing takes from it: before, queue() waited on the binder thread for as long
    // as the host did not read, and the app with it.
    SendQueue queue(100);
    EXPECT_TRUE(queue.push(std::vector<uint8_t>(100, 0)));
    const auto start = steady_clock::now();
    EXPECT_TRUE(queue.pushFor(std::vector<uint8_t>(10, 1), true, milliseconds(150)) == PushResult::Full);
    const auto took = steady_clock::now() - start;
    EXPECT_TRUE(took >= milliseconds(140));
    EXPECT_TRUE(took < milliseconds(1000));
    queue.close();
    EXPECT_TRUE(queue.pushFor({1}, true, milliseconds(150)) == PushResult::Closed);
}

TEST(fr26_a_flush_that_returned_before_its_answer_still_drops_the_stream_before_it) {
    OutputGate gate;
    gate.beginFlush();
    gate.beginFlush();
    EXPECT_TRUE(gate.flushing());
    // One answer: the other flush is still on its way.
    gate.endFlush();
    EXPECT_TRUE(!gate.open(gate.ticket()));
    gate.endFlush();
    EXPECT_TRUE(gate.open(gate.ticket()));
    // An answer too many changes nothing.
    gate.endFlush();
    EXPECT_TRUE(gate.open(gate.ticket()));
    // A stop forgets the flushes no answer will come for, so the next start's pictures go out.
    gate.beginFlush();
    gate.stop();
    EXPECT_TRUE(!gate.flushing());
    EXPECT_TRUE(gate.open(gate.ticket()));
}

TEST(fr26_a_stop_does_not_wait_for_a_picture_stuck_in_a_call_to_the_app) {
    // The thread that writes the pictures, inside a call to the app that takes 2 s to come back.
    std::atomic<bool> finished{false};
    BoundedThread worker;
    worker.start([&finished] {
        std::this_thread::sleep_for(milliseconds(1500));
        finished = true;
    });
    EXPECT_TRUE(worker.started());
    const auto start = steady_clock::now();
    EXPECT_TRUE(!worker.stop(milliseconds(200)));
    EXPECT_TRUE(steady_clock::now() - start < milliseconds(700));
    EXPECT_TRUE(!worker.started());
    // It finishes on its own; a thread that ends in time is waited for.
    EXPECT_TRUE(waitUntil([&] { return finished.load(); }));
    BoundedThread quick;
    quick.start([] {});
    EXPECT_TRUE(quick.stop(milliseconds(1000)));
}

TEST(fr26_asking_a_surface_whose_waitable_stays_readable_is_paced) {
    // A waitable that is readable while the allocator still has no buffer (its surface being
    // replaced): each ask after a readable one waits first.
    int fds[2];
    EXPECT_EQ(pipe(fds), 0);
    const char signal = 1;
    EXPECT_EQ(write(fds[1], &signal, 1), 1);
    OutputGate gate;
    const auto start = steady_clock::now();
    bool lastReady = false;
    for (int i = 0; i < 20; ++i) {
        const WaitResult waited =
            waitForBuffer(fds[0], steady_clock::now() + milliseconds(500), gate, gate.ticket(), lastReady);
        EXPECT_TRUE(waited == WaitResult::Ready);
        lastReady = waited == WaitResult::Ready;
    }
    // 19 pauses of 10 ms; without them the 20 asks took no time at all.
    EXPECT_TRUE(steady_clock::now() - start >= milliseconds(150));
    // A flush ends a pause at once.
    const uint64_t ticket = gate.ticket();
    gate.beginFlush();
    EXPECT_TRUE(waitForBuffer(fds[0], steady_clock::now() + milliseconds(500), gate, ticket, true) ==
                WaitResult::Closed);
    close(fds[0]);
    close(fds[1]);
}

TEST(fr26_a_client_that_dies_mid_stream_ends_the_link_from_its_reader_without_a_crash) {
    // The app died: the component's last reference was the one its handler held, and the
    // component, with its link, went away on the link's own reading thread, inside the
    // handler. The reader then read freed memory and the HAL aborted (bad_function_call).
    int fds[2];
    EXPECT_EQ(socketpair(AF_UNIX, SOCK_STREAM, 0, fds), 0);
    std::mutex lock;
    std::unique_ptr<HostDecoder> link;
    std::atomic<int> handled{0};
    std::atomic<bool> gone{false};
    {
        std::lock_guard<std::mutex> guard(lock);
        link = HostDecoder::adopt(fds[0], [&](HostMessage) {
            ++handled;
            std::unique_ptr<HostDecoder> last;
            {
                std::lock_guard<std::mutex> guard(lock);
                last = std::move(link);
            }
            // The component's destructor, on this thread: it closes and frees the link.
            last.reset();
            gone = true;
        });
    }
    EXPECT_TRUE(writeHostFrame(fds[1], vector("video_frame")));
    EXPECT_TRUE(waitUntil([&] { return gone.load(); }));
    // Nothing more reaches the handler: not the next picture, not the socket's end.
    writeHostFrame(fds[1], vector("video_frame"));
    std::this_thread::sleep_for(milliseconds(100));
    close(fds[1]);
    std::this_thread::sleep_for(milliseconds(100));
    EXPECT_EQ(handled.load(), 1);
}

TEST(fr26_release_returns_while_pictures_are_queued_and_the_host_is_not_read) {
    // The reader waits for room behind pictures nobody takes (the app's surface gives no buffer),
    // and the writer waits on a host that reads no packet.
    int fds[2];
    EXPECT_EQ(socketpair(AF_UNIX, SOCK_STREAM, 0, fds), 0);
    auto arrivals = std::make_shared<ArrivalQueue>(1, 1 << 20);
    std::atomic<int> handled{0};
    std::unique_ptr<HostDecoder> link = HostDecoder::adopt(fds[0], [arrivals, &handled](HostMessage message) {
        ++handled;
        arrivals->push(Arrival{std::move(message), 0, steady_clock::now()});
    });
    for (int i = 0; i < 3; ++i) {
        EXPECT_TRUE(writeHostFrame(fds[1], vector("video_frame")));
    }
    EXPECT_TRUE(waitUntil([&] { return handled.load() >= 2; }));
    // Packets until the socket and the queue are full: queue() comes back within its wait.
    const std::vector<uint8_t> packet(256 * 1024, 7);
    PushResult sent = PushResult::Queued;
    const auto filling = steady_clock::now();
    for (int i = 0; i < 400 && sent == PushResult::Queued; ++i) {
        sent = link->send(packet, true, milliseconds(100));
    }
    EXPECT_TRUE(sent == PushResult::Full);
    EXPECT_TRUE(steady_clock::now() - filling < milliseconds(5000));
    // A release, in the component's order: the queue the reader waits on, then the link.
    const auto start = steady_clock::now();
    arrivals->close();
    link->close();
    link.reset();
    EXPECT_TRUE(steady_clock::now() - start < milliseconds(1000));
    const int after = handled.load();
    writeHostFrame(fds[1], vector("video_frame"));
    std::this_thread::sleep_for(milliseconds(100));
    EXPECT_EQ(handled.load(), after);
    close(fds[1]);
}

TEST(fr26_a_call_to_an_app_whose_process_is_gone_says_so) {
    EXPECT_TRUE(peerIsGone(kExTransactionFailed, kStatusDeadObject));
    EXPECT_TRUE(!peerIsGone(kExTransactionFailed, 0));
    // A service specific error (a surface with no buffer) is not that.
    EXPECT_TRUE(!peerIsGone(-8, kC2Blocking));
}

int main(int argc, char** argv) {
    if (argc != 2) {
        std::fprintf(stderr, "usage: %s <dsa/protocol>\n", argv[0]);
        return 2;
    }
    gProtocolDir = argv[1];
    return ::dsa::test::runAll();
}
