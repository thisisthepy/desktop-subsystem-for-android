// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// Which resize trace lines the hwcomposer logs.

#include "resize_trace.h"
#include "test_main.h"

using namespace dsa::hwc;

namespace {

// 2026-10-26 06:23:03.123 UTC.
constexpr int64_t kWall = 1792995783123LL;

bool endsWith(const std::string& s, const std::string& tail) {
    return s.size() >= tail.size() && s.compare(s.size() - tail.size(), tail.size(), tail) == 0;
}

}  // namespace

TEST(fr4_trace_times_are_utc_with_milliseconds) {
    EXPECT_EQ(ResizeTrace::utcStamp(0), std::string("1970-01-01T00:00:00.000Z"));
    EXPECT_EQ(ResizeTrace::utcStamp(kWall), std::string("2026-10-26T06:23:03.123Z"));
}

TEST(fr4_a_resize_is_summarised_when_its_first_frame_posts) {
    ResizeTrace trace(false);
    EXPECT_TRUE(trace.boundsChanged(7, 800, 600, 1000.0, kWall).empty());
    EXPECT_TRUE(trace.held(7, 800, 600, 1010.0, kWall).empty());
    // A frame of another size is not the end of this resize.
    EXPECT_TRUE(trace.posted(7, 640, 480, false, 1020.0, kWall).empty());
    const auto lines = trace.posted(7, 800, 600, false, 1050.0, kWall);
    EXPECT_EQ(lines.size(), 1u);
    EXPECT_EQ(lines[0], std::string("resize-trace task=7 size=800x600 stage=hwc.summary "
                                    "utc=2026-10-26T06:23:03.123Z mono=1050.000 "
                                    "since_bounds=50.0 held=40.0 late=false"));
    // Later frames of that size are not another resize.
    EXPECT_TRUE(trace.posted(7, 800, 600, false, 1060.0, kWall).empty());
}

TEST(fr4_stage_lines_come_only_when_asked_for_and_once_per_size) {
    ResizeTrace trace(true);
    EXPECT_EQ(trace.boundsChanged(1, 10, 10, 0.0, kWall).size(), 1u);
    EXPECT_TRUE(trace.boundsChanged(1, 10, 10, 1.0, kWall).empty());
    EXPECT_EQ(trace.held(1, 10, 10, 2.0, kWall).size(), 1u);
    EXPECT_TRUE(trace.held(1, 10, 10, 3.0, kWall).empty());
    // The stage line and the summary.
    EXPECT_EQ(trace.posted(1, 10, 10, true, 4.0, kWall).size(), 2u);
}

TEST(fr4_summaries_are_rate_limited_during_a_drag) {
    ResizeTrace trace(false);
    double now = 0.0;
    std::vector<std::string> last;
    int lines = 0;
    for (int step = 0; step < 6; ++step) {
        trace.boundsChanged(3, 500 + step, 400, now, kWall);
        now += 100.0;
        last = trace.posted(3, 500 + step, 400, false, now, kWall);
        lines += static_cast<int>(last.size());
    }
    // At 100 ms the first; 200 to 500 within the interval; 600 is past it.
    EXPECT_EQ(lines, 2);
    EXPECT_EQ(last.size(), 1u);
    EXPECT_TRUE(endsWith(last[0], "skipped=4"));
}

int main() { return ::dsa::test::runAll(); }
