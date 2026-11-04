// Copyright 2026 The DSA Authors
// SPDX-License-Identifier: Apache-2.0
//
// The part of libbase's logging the platform's Codec 2.0 sources use (LOG, CHECK), on the
// NDK's liblog. libbase is a platform library an NDK build cannot link, so DSA provides its
// own, with the same meaning.

#pragma once

#include <android/log.h>

#include <cstdlib>
#include <sstream>
#include <string>

namespace android {
namespace base {

enum LogSeverity { VERBOSE, DEBUG, INFO, WARNING, ERROR, FATAL_WITHOUT_ABORT, FATAL };

class LogMessage {
   public:
    LogMessage(LogSeverity severity, const char* tag) : mSeverity(severity), mTag(tag) {}
    ~LogMessage() {
        static const int kPriority[] = {ANDROID_LOG_VERBOSE, ANDROID_LOG_DEBUG, ANDROID_LOG_INFO, ANDROID_LOG_WARN,
                                        ANDROID_LOG_ERROR,   ANDROID_LOG_FATAL, ANDROID_LOG_FATAL};
        __android_log_write(kPriority[mSeverity], mTag, mStream.str().c_str());
        if (mSeverity == FATAL) {
            abort();
        }
    }
    std::ostream& stream() { return mStream; }

   private:
    LogSeverity mSeverity;
    const char* mTag;
    std::ostringstream mStream;
};

}  // namespace base
}  // namespace android

#ifndef LOG_TAG
#define LOG_TAG "dsa-codec2"
#endif

// Takes ERROR as well as ::android::base::ERROR.
#define DSA_LOG_SEVERITY(severity)                \
    ([&] {                                        \
        using ::android::base::VERBOSE;           \
        using ::android::base::DEBUG;             \
        using ::android::base::INFO;              \
        using ::android::base::WARNING;           \
        using ::android::base::ERROR;             \
        using ::android::base::FATAL_WITHOUT_ABORT; \
        using ::android::base::FATAL;             \
        return (severity);                        \
    }())

#define LOG(severity) ::android::base::LogMessage(DSA_LOG_SEVERITY(severity), LOG_TAG).stream()

#define CHECK(condition) \
    if (condition)       \
        ;                \
    else                 \
        ::android::base::LogMessage(::android::base::FATAL, LOG_TAG).stream() << "Check failed: " #condition " "

#define CHECK_EQ(a, b) CHECK((a) == (b))
#define CHECK_NE(a, b) CHECK((a) != (b))
#define CHECK_LT(a, b) CHECK((a) < (b))
#define CHECK_LE(a, b) CHECK((a) <= (b))
#define CHECK_GT(a, b) CHECK((a) > (b))
#define CHECK_GE(a, b) CHECK((a) >= (b))

#define DCHECK(condition) \
    if (true)             \
        ;                 \
    else                  \
        ::android::base::LogMessage(::android::base::FATAL, LOG_TAG).stream()
#define DCHECK_EQ(a, b) DCHECK((a) == (b))
#define DCHECK_NE(a, b) DCHECK((a) != (b))
#define DCHECK_LT(a, b) DCHECK((a) < (b))
#define DCHECK_LE(a, b) DCHECK((a) <= (b))
#define DCHECK_GT(a, b) DCHECK((a) > (b))
#define DCHECK_GE(a, b) DCHECK((a) >= (b))
