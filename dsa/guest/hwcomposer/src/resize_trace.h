// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The hwcomposer's part of the resize trace: when it learned a task's new bounds, when it
// first held the task's frame back waiting for the app to draw at that size, and when the
// first frame of that size went to the host, with how long it was held.
//
// Lines look like the host's, so the logs can be merged on their `utc` time:
//   resize-trace task=7 size=800x600 stage=hwc.posted utc=2026-10-26T06:23:03.123Z mono=1234.000 ...
// `mono` is the steady clock in milliseconds. Per-stage lines come only when detail is on,
// once per task, stage and size in a row. A summary line (stage=hwc.summary) comes for
// every resize that posts, at most once per task every kSummaryIntervalMs, with the number
// left out.

#pragma once

#include <cstdint>
#include <map>
#include <optional>
#include <string>
#include <utility>
#include <vector>

namespace dsa::hwc {

class ResizeTrace {
   public:
    static constexpr double kSummaryIntervalMs = 500.0;

    explicit ResizeTrace(bool detailed) : mDetailed(detailed) {}

    // Each returns the lines to log, often none.

    // The task's bounds are now `width` x `height`.
    std::vector<std::string> boundsChanged(int32_t task, int64_t width, int64_t height,
                                           double monoMs, int64_t wallMs);
    // A frame of the task at its new size was held back: the app had not drawn at it.
    std::vector<std::string> held(int32_t task, int64_t width, int64_t height, double monoMs,
                                  int64_t wallMs);
    // A frame of the task was posted at `width` x `height`; `late` when the hold ran out.
    std::vector<std::string> posted(int32_t task, int64_t width, int64_t height, bool late,
                                    double monoMs, int64_t wallMs);
    void forget(int32_t task);

    // `wallMs` since the Unix epoch as 2026-10-26T06:23:03.123Z.
    static std::string utcStamp(int64_t wallMs);

   private:
    struct Pending {
        int64_t width = 0;
        int64_t height = 0;
        double learnedMs = 0;
        std::optional<double> heldMs;
    };

    std::optional<std::string> stage(int32_t task, const char* name, int64_t width,
                                     int64_t height, const std::string& detail, double monoMs,
                                     int64_t wallMs);
    std::string line(int32_t task, const char* name, int64_t width, int64_t height,
                     const std::string& detail, double monoMs, int64_t wallMs) const;

    bool mDetailed;
    std::map<int32_t, Pending> mPending;
    std::map<std::pair<int32_t, std::string>, std::pair<int64_t, int64_t>> mStages;
    // When each task's last summary went out, and how many were left out since.
    std::map<int32_t, std::pair<double, int>> mSummaries;
};

}  // namespace dsa::hwc
