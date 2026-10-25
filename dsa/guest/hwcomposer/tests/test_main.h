// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// A minimal test runner, so the hwcomposer's rules build and run on any C++17 compiler
// without a test framework to fetch.

#pragma once

#include <cstdio>
#include <functional>
#include <string>
#include <vector>

namespace dsa::test {

struct Case {
    const char* name;
    std::function<void()> body;
};

inline std::vector<Case>& cases() {
    static std::vector<Case> all;
    return all;
}

inline int& failures() {
    static int count = 0;
    return count;
}

struct Register {
    Register(const char* name, std::function<void()> body) { cases().push_back({name, body}); }
};

inline void fail(const char* file, int line, const std::string& what) {
    std::fprintf(stderr, "  %s:%d: %s\n", file, line, what.c_str());
    ++failures();
}

inline int runAll() {
    int failedCases = 0;
    for (const Case& c : cases()) {
        const int before = failures();
        c.body();
        const bool ok = failures() == before;
        std::printf("%s %s\n", ok ? "ok  " : "FAIL", c.name);
        if (!ok) {
            ++failedCases;
        }
    }
    std::printf("%zu tests, %d failed\n", cases().size(), failedCases);
    return failedCases == 0 ? 0 : 1;
}

}  // namespace dsa::test

#define TEST(name)                                              \
    static void name();                                         \
    static ::dsa::test::Register name##_registration(#name, name); \
    static void name()

#define EXPECT_TRUE(cond)                                                  \
    do {                                                                   \
        if (!(cond)) ::dsa::test::fail(__FILE__, __LINE__, "expected " #cond); \
    } while (0)

#define EXPECT_EQ(a, b)                                                             \
    do {                                                                            \
        if (!((a) == (b))) ::dsa::test::fail(__FILE__, __LINE__, "expected " #a " == " #b); \
    } while (0)
