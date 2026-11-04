// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "video_messages.h"

#include <cstdlib>

#include "control.pb.h"
#include "control_schema.h"

namespace dsa::codec2 {

namespace v1 = ::dsa::control::v1;

namespace {

std::vector<uint8_t> encode(const v1::Envelope& envelope) {
    const std::string bytes = envelope.SerializeAsString();
    return std::vector<uint8_t>(bytes.begin(), bytes.end());
}

std::string schemaHash() {
    std::string hash;
    const char* hex = v1::kSchemaHashHex;
    for (size_t i = 0; hex[i] != '\0' && hex[i + 1] != '\0'; i += 2) {
        const char pair[3] = {hex[i], hex[i + 1], '\0'};
        hash.push_back(static_cast<char>(std::strtoul(pair, nullptr, 16)));
    }
    return hash;
}

}  // namespace

HostMessage decodeHostFrame(const uint8_t* data, size_t size) {
    v1::Envelope envelope;
    if (!envelope.ParseFromArray(data, static_cast<int>(size))) {
        return std::monostate{};
    }
    switch (envelope.body_case()) {
        case v1::Envelope::kHelloAccepted:
            return Accepted{};
        case v1::Envelope::kHelloRejected:
            return Rejected{envelope.hello_rejected().detail()};
        case v1::Envelope::kVideoCapabilities: {
            Capabilities caps;
            for (const v1::VideoCodecSupport& support : envelope.video_capabilities().codecs()) {
                const int32_t codec = static_cast<int32_t>(support.codec());
                if (codec < static_cast<int32_t>(Codec::H264) || codec > static_cast<int32_t>(Codec::Av1)) {
                    continue;
                }
                caps.codecs.push_back(CodecSupport{static_cast<Codec>(codec), support.max_width(),
                                                   support.max_height()});
            }
            return caps;
        }
        case v1::Envelope::kVideoDecoderOpened:
            return Opened{envelope.video_decoder_opened().ok(), envelope.video_decoder_opened().error()};
        case v1::Envelope::kVideoFrame: {
            const v1::VideoFrame& f = envelope.video_frame();
            Frame frame;
            frame.frameIndex = f.frame_index();
            frame.timestampUs = f.timestamp_us();
            frame.width = f.width();
            frame.height = f.height();
            frame.stride = f.stride();
            frame.nv12 = f.nv12();
            frame.endOfStream = f.end_of_stream();
            return frame;
        }
        case v1::Envelope::kVideoFlushed:
            return Flushed{};
        case v1::Envelope::kVideoError:
            return DecodeError{envelope.video_error().error()};
        default:
            return std::monostate{};
    }
}

std::vector<uint8_t> encodeHello(const std::string& build) {
    v1::Envelope envelope;
    v1::Hello* hello = envelope.mutable_hello();
    hello->set_protocol_version(v1::PROTOCOL_VERSION_CURRENT);
    hello->set_schema_hash(schemaHash());
    hello->set_role(v1::ROLE_GUEST_VIDEO);
    hello->set_build(build);
    return encode(envelope);
}

std::vector<uint8_t> encodeOpen(Codec codec, uint32_t width, uint32_t height) {
    v1::Envelope envelope;
    v1::VideoDecoderOpen* open = envelope.mutable_video_decoder_open();
    open->set_codec(static_cast<v1::VideoCodec>(codec));
    open->set_width(width);
    open->set_height(height);
    return encode(envelope);
}

std::vector<uint8_t> encodePacket(uint64_t frameIndex, int64_t timestampUs, const uint8_t* data, size_t size,
                                  bool codecConfig, bool endOfStream) {
    v1::Envelope envelope;
    v1::VideoPacket* packet = envelope.mutable_video_packet();
    packet->set_frame_index(frameIndex);
    packet->set_timestamp_us(timestampUs);
    if (size > 0) {
        packet->set_data(data, size);
    }
    packet->set_codec_config(codecConfig);
    packet->set_end_of_stream(endOfStream);
    return encode(envelope);
}

std::vector<uint8_t> encodeFlush() {
    v1::Envelope envelope;
    envelope.mutable_video_flush();
    return encode(envelope);
}

std::vector<uint8_t> withLength(const std::vector<uint8_t>& frame) {
    std::vector<uint8_t> out;
    out.reserve(frame.size() + 4);
    const uint32_t size = static_cast<uint32_t>(frame.size());
    for (int shift = 0; shift < 32; shift += 8) {
        out.push_back(static_cast<uint8_t>(size >> shift));
    }
    out.insert(out.end(), frame.begin(), frame.end());
    return out;
}

}  // namespace dsa::codec2
