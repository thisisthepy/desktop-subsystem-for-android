// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// A guard on how long a resized window waits for its app's first frame at the new size, for
// the part of it the graphics transport adds: what crosses the socket for host-visible memory
// and the hashing that decides it.
//
// A Hyper-V VM has no memory the guest and the host share, so the guest driver keeps a copy of
// each mapped host-visible allocation. Before each submit it sends the blocks the app changed
// (dsa_shadow_blocks.h), and at each fence the app sees signalled it reads the device's writes
// back. In v0.2.1 and v0.2.2 that read back sent every mapped byte at every fence, so each
// frame of an app waited for its mapped memory to cross the socket: Google Play drew at about
// 7 frames a second (150 ms median, almost all of it issuing draw commands), and a resized
// window waited 0.8 to 3 s for a frame of its new size, against 80 ms in v0.1.
//
// This runs the guest's and the host's halves as the driver and the decoder do, over an
// in-memory socket that counts bytes, for frames as Skia on ANGLE makes them (two submits,
// each with a fence the app sees signalled). The cost is modelled from what was counted, so
// the guard does not depend on the machine it runs on: bytes over the socket at
// kSocketBytesPerSecond and bytes hashed at kHashBytesPerSecond. A resize costs
// kFramesPerResize frames: the one the app lays out in, and the one after. The guard fails
// when the transport's share of that is over kResizeBudgetMs, and checks that the old whole
// read back would have been over it, so the guard catches it if it comes back. The measured
// time on the machine that ran it is printed beside the model.

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <random>
#include <vector>

#include "dsa_shadow_blocks.h"
#include "dsa_shadow_record.h"

namespace {

using dsa::kShadowBlockSize;
using dsa::ShadowBlocks;
using dsa::ShadowRecord;

int failures = 0;

void check(bool ok, const char* test, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "%s: %s\n", test, what);
        failures++;
    }
}

// The app's mapped memory: Skia's and ANGLE's uniform, vertex and staging buffers, in the
// suballocator's blocks.
constexpr int kAllocations = 8;
constexpr uint64_t kAllocationSize = 2 * 1024 * 1024;
// What the app writes in a frame, in runs as a ring buffer takes them (uniforms, vertices, a
// glyph upload), and how many submits with a fence each frame makes.
constexpr int kRunsPerFrame = 4;
constexpr uint64_t kRunSize = 32 * 1024;
constexpr int kSubmitsPerFrame = 2;
// The pieces the guest asks for (patches/guest/mesa/0010).
constexpr uint64_t kReadBackPiece = 64 * 1024;
// A Hyper-V socket carries less than this, and a guest processor hashes faster: the model
// is generous to the old read back on both counts.
constexpr double kSocketBytesPerSecond = 1024.0 * 1024.0 * 1024.0;
constexpr double kHashBytesPerSecond = 4.0 * 1024.0 * 1024.0 * 1024.0;
constexpr int kFramesPerResize = 2;
// The transport's share of a resize. The whole wait was 80 ms in v0.1 on the owner's PC.
constexpr double kResizeBudgetMs = 50.0;

struct Allocation {
    std::vector<uint8_t> guest;
    std::vector<uint8_t> host;
    ShadowBlocks blocks{kAllocationSize};
};

class Pipeline {
   public:
    explicit Pipeline(bool wholeReadBack) : whole_(wholeReadBack), random_(7) {
        for (int i = 0; i < kAllocations; i++) {
            allocations_[i + 1].guest.assign(kAllocationSize, 0);
            allocations_[i + 1].host.assign(kAllocationSize, 0);
        }
        // The first map reads each allocation back once (0007), which the host sends whole.
        fence();
        reset();
    }

    void appWrites(int runs) {
        std::uniform_int_distribution<uint64_t> where(0, kAllocationSize - kRunSize);
        std::uniform_int_distribution<int> which(1, kAllocations);
        for (int run = 0; run < runs; run++) {
            std::vector<uint8_t>& guest = allocations_[which(random_)].guest;
            const uint64_t at = where(random_);
            for (uint64_t i = 0; i < kRunSize; i++) guest[at + i] = static_cast<uint8_t>(random_());
        }
    }

    void deviceWrites(uint64_t memory, uint64_t offset, uint64_t bytes) {
        std::vector<uint8_t>& host = allocations_[memory].host;
        for (uint64_t i = 0; i < bytes; i++) host[offset + i] = static_cast<uint8_t>(i * 31 + 1);
    }

    // vkQueueSubmit: the guest flushes what the app changed, which the host takes and records.
    void submit() {
        const auto started = std::chrono::steady_clock::now();
        for (auto& [memory, allocation] : allocations_) {
            hashed_ += kAllocationSize;
            for (const ShadowBlocks::Range& range : allocation.blocks.changed(allocation.guest.data())) {
                sent_ += 8 + range.size;
                std::memcpy(allocation.host.data() + range.offset,
                            allocation.guest.data() + range.offset, range.size);
                if (!whole_) {
                    hashed_ += range.size;
                    record_.flushed(memory, kAllocationSize, range.offset,
                                    allocation.guest.data() + range.offset, range.size);
                }
            }
        }
        measuredMs_ += msSince(started);
    }

    // The app sees a fence signalled: the guest reads the device's writes back.
    void fence() {
        const auto started = std::chrono::steady_clock::now();
        for (auto& [memory, allocation] : allocations_) {
            if (whole_) {
                // One range for all of it, sent whole; the guest took all of it and hashed
                // all of it again as the host's.
                sent_ += 8 + kAllocationSize;
                readBack_ += kAllocationSize;
                hashed_ += kAllocationSize;
                std::memcpy(allocation.guest.data(), allocation.host.data(), kAllocationSize);
                allocation.blocks.refresh(allocation.guest.data());
                continue;
            }
            for (uint64_t offset = 0; offset < kAllocationSize; offset += kReadBackPiece) {
                sent_ += 8;
                hashed_ += kReadBackPiece;
                if (!record_.mustSend(memory, allocation.host.data(), kAllocationSize, offset,
                                      kReadBackPiece)) {
                    continue;
                }
                sent_ += kReadBackPiece;
                readBack_ += kReadBackPiece;
                hashed_ += kReadBackPiece;
                allocation.blocks.receive(allocation.guest.data(), offset,
                                          allocation.host.data() + offset, kReadBackPiece);
            }
        }
        measuredMs_ += msSince(started);
    }

    void frame() {
        appWrites(kRunsPerFrame);
        for (int i = 0; i < kSubmitsPerFrame; i++) {
            submit();
            fence();
        }
    }

    double modelledMs() const {
        return 1000.0 * (sent_ / kSocketBytesPerSecond + hashed_ / kHashBytesPerSecond);
    }
    double measuredMs() const { return measuredMs_ + 1000.0 * sent_ / kSocketBytesPerSecond; }
    uint64_t sent() const { return sent_; }
    uint64_t readBack() const { return readBack_; }
    void reset() {
        sent_ = readBack_ = hashed_ = 0;
        measuredMs_ = 0;
    }
    std::map<uint64_t, Allocation>& allocations() { return allocations_; }

   private:
    static double msSince(std::chrono::steady_clock::time_point started) {
        return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started)
            .count();
    }

    bool whole_;
    std::mt19937_64 random_;
    std::map<uint64_t, Allocation> allocations_;
    ShadowRecord record_;
    uint64_t sent_ = 0;
    uint64_t readBack_ = 0;
    uint64_t hashed_ = 0;
    double measuredMs_ = 0;
};

// The guest's copy and the host's memory agree after each fence, whatever each side wrote.
void fr4_guest_and_host_agree_after_each_fence() {
    const char* test = __func__;
    Pipeline pipeline(false);
    for (int frame = 0; frame < 20; frame++) {
        pipeline.appWrites(2);
        pipeline.submit();
        if (frame % 3 == 0) pipeline.deviceWrites(1 + frame % kAllocations, 4096 * frame, 3000);
        pipeline.fence();
        for (auto& [memory, allocation] : pipeline.allocations()) {
            if (allocation.guest != allocation.host) {
                std::fprintf(stderr, "%s: allocation %llu differs after frame %d\n", test,
                             static_cast<unsigned long long>(memory), frame);
                failures++;
                return;
            }
        }
    }
}

// What the app wrote after its submit and before the fence stays, and reaches the host at the
// next submit, though the device wrote elsewhere in the same piece.
void fr4_an_app_write_before_the_fence_is_not_lost() {
    const char* test = __func__;
    Pipeline pipeline(false);
    Allocation& allocation = pipeline.allocations()[1];
    allocation.guest[10] = 0x77;
    pipeline.deviceWrites(1, 2 * kShadowBlockSize, 10);
    pipeline.fence();
    check(allocation.guest[2 * kShadowBlockSize] == 1, test, "the device's write came in");
    check(allocation.guest[10] == 0x77, test, "the app's write stays in the guest's copy");
    pipeline.submit();
    check(allocation.host[10] == 0x77, test, "and goes to the host at the next submit");
}

void fr4_a_frame_reads_back_only_what_the_device_wrote() {
    const char* test = __func__;
    Pipeline pipeline(false);
    pipeline.frame();
    pipeline.reset();
    pipeline.frame();
    check(pipeline.readBack() == 0, test, "nothing comes back when the device wrote nothing");
    pipeline.reset();
    pipeline.deviceWrites(3, 5 * kReadBackPiece + 100, 200);
    pipeline.fence();
    check(pipeline.readBack() == kReadBackPiece, test, "one write brings back its piece");
}

void fr4_a_resize_waits_on_the_transport_for_less_than_its_budget() {
    const char* test = __func__;
    Pipeline now(false);
    Pipeline old(true);
    now.frame();
    old.frame();
    now.reset();
    old.reset();
    for (int i = 0; i < kFramesPerResize; i++) {
        now.frame();
        old.frame();
    }
    std::printf(
        "resize transport cost (%d frames, %d MiB mapped, %d submits a frame, socket %.0f MiB/s, "
        "hashing %.0f MiB/s): %.1f ms modelled, %.1f ms measured here, %.0f KiB sent; the whole "
        "read back: %.1f ms modelled, %.1f ms measured here, %.0f KiB sent\n",
        kFramesPerResize, static_cast<int>(kAllocations * kAllocationSize / (1024 * 1024)),
        kSubmitsPerFrame, kSocketBytesPerSecond / (1024 * 1024),
        kHashBytesPerSecond / (1024 * 1024), now.modelledMs(), now.measuredMs(),
        now.sent() / 1024.0, old.modelledMs(), old.measuredMs(), old.sent() / 1024.0);
    check(now.modelledMs() <= kResizeBudgetMs, test,
          "the transport's share of a resize is over its budget");
    check(old.modelledMs() > kResizeBudgetMs, test,
          "the whole read back is under the budget: the guard would not catch it coming back");
}

}  // namespace

int main() {
    fr4_guest_and_host_agree_after_each_fence();
    fr4_an_app_write_before_the_fence_is_not_lost();
    fr4_a_frame_reads_back_only_what_the_device_wrote();
    fr4_a_resize_waits_on_the_transport_for_less_than_its_budget();
    if (failures != 0) {
        std::fprintf(stderr, "%d failed\n", failures);
        return EXIT_FAILURE;
    }
    std::puts("read back latency: all passed");
    return EXIT_SUCCESS;
}
