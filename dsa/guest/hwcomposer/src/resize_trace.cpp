// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0

#include "resize_trace.h"

#include <cstdio>
#include <ctime>
#include <iterator>

namespace dsa::hwc {

namespace {

std::string ms1(double millis) {
    char buf[32];
    std::snprintf(buf, sizeof(buf), "%.1f", millis);
    return buf;
}

}  // namespace

std::string ResizeTrace::utcStamp(int64_t wallMs) {
    const time_t secs = static_cast<time_t>(wallMs / 1000);
    std::tm tm{};
    gmtime_r(&secs, &tm);
    char buf[40];
    std::snprintf(buf, sizeof(buf), "%04d-%02d-%02dT%02d:%02d:%02d.%03dZ", tm.tm_year + 1900,
                  tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec,
                  static_cast<int>(wallMs % 1000));
    return buf;
}

std::string ResizeTrace::line(int32_t task, const char* name, int64_t width, int64_t height,
                              const std::string& detail, double monoMs, int64_t wallMs) const {
    char head[160];
    std::snprintf(head, sizeof(head), "resize-trace task=%d size=%lldx%lld stage=%s utc=%s mono=%.3f",
                  task, static_cast<long long>(width), static_cast<long long>(height), name,
                  utcStamp(wallMs).c_str(), monoMs);
    std::string out = head;
    if (!detail.empty()) {
        out += ' ';
        out += detail;
    }
    return out;
}

std::optional<std::string> ResizeTrace::stage(int32_t task, const char* name, int64_t width,
                                              int64_t height, const std::string& detail,
                                              double monoMs, int64_t wallMs) {
    if (!mDetailed) {
        return std::nullopt;
    }
    auto& last = mStages[{task, name}];
    if (last == std::make_pair(width, height)) {
        return std::nullopt;
    }
    last = {width, height};
    return line(task, name, width, height, detail, monoMs, wallMs);
}

std::vector<std::string> ResizeTrace::boundsChanged(int32_t task, int64_t width, int64_t height,
                                                    double monoMs, int64_t wallMs) {
    std::vector<std::string> out;
    auto it = mPending.find(task);
    if (it != mPending.end() && it->second.width == width && it->second.height == height) {
        return out;
    }
    mPending[task] = Pending{width, height, monoMs, std::nullopt};
    if (auto l = stage(task, "hwc.bounds", width, height, "", monoMs, wallMs)) {
        out.push_back(*l);
    }
    return out;
}

std::vector<std::string> ResizeTrace::held(int32_t task, int64_t width, int64_t height,
                                           double monoMs, int64_t wallMs) {
    std::vector<std::string> out;
    auto it = mPending.find(task);
    if (it == mPending.end() || it->second.width != width || it->second.height != height ||
        it->second.heldMs) {
        return out;
    }
    it->second.heldMs = monoMs;
    if (auto l = stage(task, "hwc.held", width, height,
                       "since_bounds=" + ms1(monoMs - it->second.learnedMs), monoMs, wallMs)) {
        out.push_back(*l);
    }
    return out;
}

std::vector<std::string> ResizeTrace::posted(int32_t task, int64_t width, int64_t height,
                                             bool late, double monoMs, int64_t wallMs) {
    std::vector<std::string> out;
    auto it = mPending.find(task);
    if (it == mPending.end() || it->second.width != width || it->second.height != height) {
        return out;
    }
    const Pending pending = it->second;
    mPending.erase(it);
    const double held = pending.heldMs ? monoMs - *pending.heldMs : 0.0;
    const std::string detail = std::string("since_bounds=") + ms1(monoMs - pending.learnedMs) +
                               " held=" + ms1(held) + " late=" + (late ? "true" : "false");
    if (auto l = stage(task, "hwc.posted", width, height, detail, monoMs, wallMs)) {
        out.push_back(*l);
    }
    auto summary = mSummaries.find(task);
    int skipped = 0;
    if (summary != mSummaries.end()) {
        if (monoMs - summary->second.first < kSummaryIntervalMs) {
            ++summary->second.second;
            return out;
        }
        skipped = summary->second.second;
    }
    mSummaries[task] = {monoMs, 0};
    std::string full = detail;
    if (skipped > 0) {
        full += " skipped=" + std::to_string(skipped);
    }
    out.push_back(line(task, "hwc.summary", width, height, full, monoMs, wallMs));
    return out;
}

void ResizeTrace::forget(int32_t task) {
    mPending.erase(task);
    mSummaries.erase(task);
    for (auto it = mStages.begin(); it != mStages.end();) {
        it = it->first.first == task ? mStages.erase(it) : std::next(it);
    }
}

}  // namespace dsa::hwc
