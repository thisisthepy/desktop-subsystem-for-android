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
// The render thread's read buffer (ReadBuffer, patch 0027), which a batch of guest commands is
// read into and which grows the same way, follows the same rule with its own first size as the
// size it keeps (bufferShrinks): a video's large uploads on every frame keep it, and it goes back
// on a small read once none larger has come for kReplyQuietFor.
//
// Only VulkanStream.cpp and ReadBuffer.cpp include this header. Their headers, which other
// libraries of the renderer include too, keep the values of the state as plain members.

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

// A transfer of `used` bytes went through a buffer of `capacity` bytes at `now`, in a buffer
// that keeps `keep` bytes; `lastLarge` and `seenLarge` say when the last transfer larger than
// `keep` went through, if one has, and are updated. Returns whether the buffer goes back to
// `keep` now: on a transfer that fits in it, once none larger has gone through for
// kReplyQuietFor. A transfer of nothing changes nothing.
inline bool bufferShrinks(size_t used, size_t capacity, size_t keep, ReplyClock::time_point now,
                          ReplyClock::time_point& lastLarge, bool& seenLarge) {
    if (used == 0) {
        return false;
    }
    if (used > keep) {
        lastLarge = now;
        seenLarge = true;
        return false;
    }
    if (capacity <= keep) {
        return false;
    }
    return !seenLarge || now - lastLarge >= kReplyQuietFor;
}

// A reply of `sent` bytes went out of a buffer of `capacity` bytes at `now` (bufferShrinks with
// kReplyKeepBytes). A reply of nothing (a read that committed no write) is not a reply.
inline bool replyBufferShrinks(size_t sent, size_t capacity, ReplyClock::time_point now,
                               ReplyClock::time_point& lastLarge, bool& seenLarge) {
    return bufferShrinks(sent, capacity, kReplyKeepBytes, now, lastLarge, seenLarge);
}

// Puts `buffer` back to kReplyKeepBytes, giving its larger allocation back.
inline void shrinkReplyBuffer(std::vector<uint8_t>& buffer) {
    std::vector<uint8_t>(kReplyKeepBytes).swap(buffer);
}

}  // namespace dsa
