// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// When the renderer gives back what a guest's device left behind (dsa_device_gone_trim.h), run
// on the build machine.

#include "dsa_device_gone_trim.h"

#include <chrono>
#include <cstdio>

static int failures = 0;

#define EXPECT(cond)                                                                \
    do {                                                                            \
        if (!(cond)) {                                                              \
            std::fprintf(stderr, "%s:%d: expected %s\n", __FILE__, __LINE__, #cond); \
            ++failures;                                                             \
        }                                                                           \
    } while (0)

using Clock = dsa::DeviceGoneTrim::Clock;
using std::chrono::milliseconds;

// nfr1: with no device gone, nothing is trimmed, however long the renderer runs.
static void nfr1_nothing_is_trimmed_while_no_device_goes() {
    dsa::DeviceGoneTrim trim;
    const Clock::time_point start = Clock::now();
    EXPECT(!trim.due(start));
    EXPECT(!trim.due(start + std::chrono::hours(1)));
}

// nfr1: a device that goes away while windows stay open is trimmed after once, a moment later,
// and not again until another device goes.
static void nfr1_an_app_closing_while_windows_stay_open_is_trimmed_once() {
    dsa::DeviceGoneTrim trim;
    const Clock::time_point gone = Clock::now();
    trim.deviceGone(gone);
    EXPECT(!trim.due(gone));
    EXPECT(!trim.due(gone + milliseconds(1999)));
    EXPECT(trim.due(gone + dsa::DeviceGoneTrim::kSettle));
    EXPECT(trim.takeGone() == 1);
    EXPECT(!trim.due(gone + std::chrono::seconds(10)));
    EXPECT(trim.takeGone() == 0);
}

// nfr1: devices that go away together (an app and its other processes) cost one trim, after
// the last of them.
static void nfr1_devices_that_go_together_are_trimmed_after_the_last() {
    dsa::DeviceGoneTrim trim;
    const Clock::time_point first = Clock::now();
    trim.deviceGone(first);
    trim.deviceGone(first + milliseconds(1500));
    trim.deviceGone(first + milliseconds(3000));
    EXPECT(!trim.due(first + milliseconds(4000)));
    EXPECT(trim.due(first + milliseconds(5000)));
    EXPECT(trim.takeGone() == 3);
    EXPECT(!trim.due(first + milliseconds(9000)));
}

int main() {
    nfr1_nothing_is_trimmed_while_no_device_goes();
    nfr1_an_app_closing_while_windows_stay_open_is_trimmed_once();
    nfr1_devices_that_go_together_are_trimmed_after_the_last();
    if (failures) {
        std::fprintf(stderr, "%d failures\n", failures);
        return 1;
    }
    std::puts("device gone trim: all passed");
    return 0;
}
