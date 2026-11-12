// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// When a render thread's reply buffer goes back to its normal size.
//
// The host's Vulkan decoder buffers each reply to its guest thread in one growing buffer
// (VulkanStream's write buffer, patch 0028), which grows to twice the largest reply the thread
// ever sent. A copy of host-visible memory travels back to the guest in one reply, so after a
// large readback the thread kept tens of MiB for as long as it lived.
//
// The buffer goes back to kReplyKeepBytes on a reply that fits in that much, once no larger
// reply has gone out for kReplyQuietFor. Replies that stay large, as a video's readbacks are on
// every frame, keep the buffer, and small replies between them do not shrink it either, so a
// steady stream of frames never reallocates. A thread that sends nothing more keeps the buffer
// until its next reply: nothing runs on an idle render thread to give it back sooner.
//
// Only VulkanStream.cpp includes this header. VulkanStream.h, which other libraries of the
// renderer include too, keeps the two values of the state as plain members.

#pragma once

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <vector>

namespace dsa {

// The size a reply buffer keeps: the replies of an ordinary frame fit in it.
inline constexpr size_t kReplyKeepBytes = 256 * 1024;

// How long no large reply must have gone out before a small one shrinks the buffer.
inline constexpr std::chrono::milliseconds kReplyQuietFor{2000};

using ReplyClock = std::chrono::steady_clock;

// A reply of `sent` bytes went out of a buffer of `capacity` bytes at `now`; `lastLarge` and
// `seenLarge` say when the last reply larger than kReplyKeepBytes went out, if one has, and
// are updated. Returns whether the buffer goes back to kReplyKeepBytes now. A reply of nothing
// (a read that committed no write) is not a reply and changes nothing.
inline bool replyBufferShrinks(size_t sent, size_t capacity, ReplyClock::time_point now,
                               ReplyClock::time_point& lastLarge, bool& seenLarge) {
    if (sent == 0) {
        return false;
    }
    if (sent > kReplyKeepBytes) {
        lastLarge = now;
        seenLarge = true;
        return false;
    }
    if (capacity <= kReplyKeepBytes) {
        return false;
    }
    return !seenLarge || now - lastLarge >= kReplyQuietFor;
}

// Puts `buffer` back to kReplyKeepBytes, giving its larger allocation back.
inline void shrinkReplyBuffer(std::vector<uint8_t>& buffer) {
    std::vector<uint8_t>(kReplyKeepBytes).swap(buffer);
}

}  // namespace dsa
