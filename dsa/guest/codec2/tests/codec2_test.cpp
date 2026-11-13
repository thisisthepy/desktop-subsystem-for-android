// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The codec HAL's rules: its video port messages against the checked-in protocol vectors, so
// they stay in step with the host's, and the copy of a decoded picture into a guest buffer's
// planes. The protocol folder is the only argument.

#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdio>
#include <fstream>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

#include "config_rules.h"
#include "output_buffer.h"
#include "output_gate.h"
#include "picture_copy.h"
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

int main(int argc, char** argv) {
    if (argc != 2) {
        std::fprintf(stderr, "usage: %s <dsa/protocol>\n", argv[0]);
        return 2;
    }
    gProtocolDir = argv[1];
    return ::dsa::test::runAll();
}
