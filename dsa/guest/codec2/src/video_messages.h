// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The video port's messages, through the codec generated from dsa/protocol/control.proto.
// Each frame is one encoded Envelope; on the socket each goes with its length in front.

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <variant>
#include <vector>

namespace dsa::codec2 {

// A coding format, as the protocol numbers them.
enum class Codec : int32_t { H264 = 1, Hevc = 2, Vp9 = 3, Av1 = 4 };

struct CodecSupport {
    Codec codec = Codec::H264;
    uint32_t maxWidth = 0;
    uint32_t maxHeight = 0;
};

// What the host sends on the video port.
struct Accepted {};
struct Rejected {
    std::string detail;
};
struct Capabilities {
    std::vector<CodecSupport> codecs;
};
struct Opened {
    bool ok = false;
    std::string error;
};
struct Frame {
    uint64_t frameIndex = 0;
    int64_t timestampUs = 0;
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t stride = 0;
    std::string nv12;
    bool endOfStream = false;
};
// Whether `frame` says its packet gives no picture: the host did not decode it, since it came
// after a flush or the stream's start and before a key frame (its picture would be green).
// Such a frame has no size and no planes, and does not end the stream.
inline bool carriesNoPicture(const Frame& frame) {
    return !frame.endOfStream && frame.width == 0 && frame.height == 0 && frame.nv12.empty();
}

struct Flushed {};
struct DecodeError {
    std::string error;
};

using HostMessage =
    std::variant<std::monostate, Accepted, Rejected, Capabilities, Opened, Frame, Flushed, DecodeError>;

// What a frame from the host says; std::monostate for one that cannot be decoded or that the
// video port does not carry.
HostMessage decodeHostFrame(const uint8_t* data, size_t size);

// The codec HAL's Hello on the video port.
std::vector<uint8_t> encodeHello(const std::string& build);
std::vector<uint8_t> encodeOpen(Codec codec, uint32_t width, uint32_t height);
std::vector<uint8_t> encodePacket(uint64_t frameIndex, int64_t timestampUs, const uint8_t* data, size_t size,
                                  bool codecConfig, bool endOfStream);
std::vector<uint8_t> encodeFlush();

// `frame` with its length in front, as the socket carries it.
std::vector<uint8_t> withLength(const std::vector<uint8_t>& frame);

}  // namespace dsa::codec2
