// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The codec HAL's rules: its video port messages against the checked-in protocol vectors, so
// they stay in step with the host's, and the copy of a decoded picture into a guest buffer's
// planes. The protocol folder is the only argument.

#include <cstdio>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#include "picture_copy.h"
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

int main(int argc, char** argv) {
    if (argc != 2) {
        std::fprintf(stderr, "usage: %s <dsa/protocol>\n", argv[0]);
        return 2;
    }
    gProtocolDir = argv[1];
    return ::dsa::test::runAll();
}
