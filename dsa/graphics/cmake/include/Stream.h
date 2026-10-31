/*
 * Copyright 2026 The DSA Authors
 * SPDX-License-Identifier: Apache-2.0
 */
#pragma once

#include "aemu/base/files/Stream.h"
#include "aemu/base/files/StreamSerializing.h"

namespace gfxstream {
namespace aemu {
using Stream = gfxstream::guest::Stream;
}
}

namespace android {
namespace base {
using Stream = gfxstream::guest::Stream;
using gfxstream::guest::saveStringArray;
}
}

using Stream = gfxstream::guest::Stream;
using gfxstream::guest::saveStringArray;
