#!/usr/bin/env bash
# Fetches what DSA's codec HAL builds against, at pinned revisions, into .scratch/:
#
# - the frozen AIDL definitions of the Codec 2.0 HAL it serves (android.hardware.media.c2
#   version 1) and the packages it imports, with the two parcelables they name from the
#   platform (android.hardware.HardwareBuffer, android.view.Surface);
# - the platform's Codec 2.0 core headers and the sources of its parameter helpers and HAL
#   utilities, which the HAL builds so it describes its parameters exactly as the platform's
#   own decoders do;
# - libbinder_ndk's C++ headers, which the NDK does not ship;
# - the protobuf-lite runtime matching the protoc that generated dsa/protocol/cpp/generated.
#
#   dsa/guest/codec2/fetch.sh [scratch dir]
#
# None of it is kept in Git; a build always starts here so it cannot pick up another
# upstream revision by accident.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
scratch_arg="${1:-$root/.scratch}"
mkdir -p "$scratch_arg"
scratch="$(cd "$scratch_arg" && pwd)"

# fetch <url> <dir> <revision> <path>...: a sparse checkout of the paths at the revision.
fetch() {
    local url="$1" dir="$2" revision="$3"
    shift 3
    if [[ ! -d "$dir/.git" ]]; then
        git clone --filter=blob:none --no-checkout "$url" "$dir"
    fi
    git -C "$dir" sparse-checkout set --no-cone "$@"
    git -C "$dir" fetch --depth=1 --no-tags origin "$revision"
    git -C "$dir" checkout --detach "$revision"
    git -C "$dir" reset --hard "$revision"
}

# android-16.0.0_r1. Frozen AIDL versions never change, so any later tag gives the same files.
fetch https://android.googlesource.com/platform/hardware/interfaces "$scratch/codec2_interfaces" \
    5688f7eb1e117ed26e642a695de300b7683acb87 \
    'media/c2/aidl/aidl_api/android.hardware.media.c2/1/' \
    'media/bufferpool/aidl/aidl_api/android.hardware.media.bufferpool2/2/' \
    'common/aidl/aidl_api/android.hardware.common/2/' \
    'common/fmq/aidl/aidl_api/android.hardware.common.fmq/1/'

# android-16.0.0_r1: the Codec 2.0 core, its parameter helpers and AIDL HAL utilities, and the
# media type names.
fetch https://android.googlesource.com/platform/frameworks/av "$scratch/codec2_av" \
    4b2111885ff934799e70eddae2527a9848c3be29 \
    'media/codec2/core/include/' \
    'media/codec2/vndk/include/' \
    'media/codec2/vndk/internal/' \
    'media/codec2/vndk/util/' \
    'media/codec2/components/base/SimpleC2Interface.cpp' \
    'media/codec2/components/base/include/' \
    'media/codec2/hal/common/include/' \
    'media/codec2/hal/aidl/Configurable.cpp' \
    'media/codec2/hal/aidl/ParamTypes.cpp' \
    'media/codec2/hal/aidl/ParamTypes-specialization.h' \
    'media/codec2/hal/aidl/include/' \
    'media/module/foundation/MediaDefs.cpp' \
    'media/module/foundation/include/media/stagefright/foundation/MediaDefs.h' \
    'media/module/foundation/include/media/stagefright/foundation/AUtils.h'

# android-16.0.0_r1: the HardwareBuffer parcelable's declaration.
fetch https://android.googlesource.com/platform/frameworks/base "$scratch/codec2_base" \
    99b01a65cc4c104933788b3143285ab6bae65827 \
    'core/java/android/hardware/HardwareBuffer.aidl'

# android-16.0.0_r1: the binder NDK headers, and the Surface parcelable's declaration.
fetch https://android.googlesource.com/platform/frameworks/native "$scratch/codec2_native" \
    2827a4a16b0340ecd07c2d5a6c89991799b362bb \
    'libs/binder/ndk/include_cpp/' 'libs/binder/ndk/include_ndk/' 'libs/binder/ndk/include_platform/' \
    'aidl/gui/android/view/Surface.aidl'

# protobuf v21.12 (3.21.12), the runtime dsa/protocol/cpp/generated was generated for.
protobuf_dir="$scratch/protobuf-21.12"
protobuf_revision="f0dc78d7e6e331b8c6bb2d5283e06aa26883ca7c"
if [[ ! -d "$protobuf_dir/.git" ]]; then
    git clone --filter=blob:none --no-checkout https://github.com/protocolbuffers/protobuf "$protobuf_dir"
fi
git -C "$protobuf_dir" fetch --depth=1 --no-tags origin "$protobuf_revision"
git -C "$protobuf_dir" checkout --detach "$protobuf_revision"
git -C "$protobuf_dir" reset --hard "$protobuf_revision"
