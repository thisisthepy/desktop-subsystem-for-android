// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Handing back the spare pages of a reused transfer buffer (dsa_spare_pages.h), run on the
// build machine.

#include "dsa_spare_pages.h"

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

static int failures = 0;

#define EXPECT(cond)                                                                \
    do {                                                                            \
        if (!(cond)) {                                                              \
            std::fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond); \
            ++failures;                                                             \
        }                                                                           \
    } while (0)

constexpr size_t kPage = 4096;
constexpr size_t kMiB = 1024 * 1024;

// nfr1: an ordinary transfer, within the part kept warm, hands back nothing.
static void nfr1_a_transfer_within_the_warm_part_hands_back_nothing() {
    const dsa::SpareRange range = dsa::spareRange(0x100000, 200 * 1024, dsa::kSpareKeepBytes, kPage);
    EXPECT(range.length == 0);
    const dsa::SpareRange exact =
        dsa::spareRange(0x100000, dsa::kSpareKeepBytes, dsa::kSpareKeepBytes, kPage);
    EXPECT(exact.length == 0);
}

// nfr1: a large transfer (a 2880x2880 picture, 32 MiB) hands back every whole page past the
// warm part, and never one the warm part or the bytes past the transfer share.
static void nfr1_a_large_transfer_hands_back_the_whole_pages_beyond_the_warm_part() {
    const uintptr_t address = 0x7f0000001010;  // not page aligned, as a heap block need not be
    const size_t used = 32 * kMiB + 123;
    const dsa::SpareRange range = dsa::spareRange(address, used, dsa::kSpareKeepBytes, kPage);
    EXPECT(range.start % kPage == 0);
    EXPECT(range.length % kPage == 0);
    EXPECT(range.start >= address + dsa::kSpareKeepBytes);
    EXPECT(range.start < address + dsa::kSpareKeepBytes + kPage);
    EXPECT(range.start + range.length <= address + used);
    EXPECT(range.start + range.length > address + used - kPage);
    EXPECT(range.length > 31 * kMiB);
}

// nfr1: the part past the warm one shorter than a page, or no page size, hands back nothing.
static void nfr1_less_than_a_whole_page_past_the_warm_part_hands_back_nothing() {
    const dsa::SpareRange range =
        dsa::spareRange(0x100010, dsa::kSpareKeepBytes + 100, dsa::kSpareKeepBytes, kPage);
    EXPECT(range.length == 0);
    EXPECT(dsa::spareRange(0x100000, 8 * kMiB, dsa::kSpareKeepBytes, 0).length == 0);
}

// nfr1: on the build machine the buffer stays usable after its pages were marked: it is
// written and read again as before, and a null buffer is left alone.
static void nfr1_a_buffer_handed_back_is_written_and_read_again() {
    std::vector<unsigned char> buffer(4 * kMiB, 0xab);
    const size_t marked = dsa::releaseSparePages(buffer.data(), buffer.size());
#if defined(__linux__) && defined(MADV_FREE)
    EXPECT(marked > 3 * kMiB);
#else
    EXPECT(marked == 0);
#endif
    EXPECT(buffer[0] == 0xab);
    std::memset(buffer.data(), 0x5c, buffer.size());
    EXPECT(buffer[buffer.size() - 1] == 0x5c);
    EXPECT(buffer[2 * kMiB] == 0x5c);
    EXPECT(dsa::releaseSparePages(nullptr, 8 * kMiB) == 0);
}

int main() {
    nfr1_a_transfer_within_the_warm_part_hands_back_nothing();
    nfr1_a_large_transfer_hands_back_the_whole_pages_beyond_the_warm_part();
    nfr1_less_than_a_whole_page_past_the_warm_part_hands_back_nothing();
    nfr1_a_buffer_handed_back_is_written_and_read_again();
    if (failures) {
        std::fprintf(stderr, "%d failures\n", failures);
        return 1;
    }
    std::puts("spare pages: all passed");
    return 0;
}
