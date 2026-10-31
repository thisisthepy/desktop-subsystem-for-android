#!/usr/bin/env bash
# Fetches what the DSA hwcomposer builds against, at pinned revisions, into .scratch/:
#
# - the frozen AIDL definitions of the composer HAL it serves (composer3 version 4, the one
#   every supported image's compatibility matrix accepts) and the packages it imports;
# - libbinder_ndk's C++ headers, which the NDK does not ship;
# - the protobuf-lite runtime matching the protoc that generated dsa/protocol/cpp/generated;
# - the gfxstream guest sources (through dsa/graphics/fetch.sh), for renderControl.
#
#   dsa/guest/hwcomposer/fetch.sh [scratch dir]
#
# None of it is kept in Git; a build always starts here so it cannot pick up another
# upstream revision by accident.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
scratch_arg="${1:-$root/.scratch}"
mkdir -p "$scratch_arg"
scratch="$(cd "$scratch_arg" && pwd)"

# android-16.0.0_r1. Frozen AIDL versions never change, so any later tag gives the same files.
interfaces_dir="$scratch/hardware_interfaces"
interfaces_revision="5688f7eb1e117ed26e642a695de300b7683acb87"
if [[ ! -d "$interfaces_dir/.git" ]]; then
    git clone --filter=blob:none --no-checkout \
        https://android.googlesource.com/platform/hardware/interfaces "$interfaces_dir"
fi
git -C "$interfaces_dir" sparse-checkout set --no-cone \
    'graphics/composer/aidl/aidl_api/android.hardware.graphics.composer3/4/' \
    'graphics/common/aidl/aidl_api/android.hardware.graphics.common/6/' \
    'common/aidl/aidl_api/android.hardware.common/2/' \
    'drm/common/aidl/aidl_api/android.hardware.drm.common/1/'
git -C "$interfaces_dir" fetch --depth=1 --no-tags origin "$interfaces_revision"
git -C "$interfaces_dir" checkout --detach "$interfaces_revision"
git -C "$interfaces_dir" reset --hard "$interfaces_revision"

# android-16.0.0_r1: the binder NDK headers the generated code and the service use.
binder_dir="$scratch/binder_ndk"
binder_revision="2827a4a16b0340ecd07c2d5a6c89991799b362bb"
if [[ ! -d "$binder_dir/.git" ]]; then
    git clone --filter=blob:none --no-checkout \
        https://android.googlesource.com/platform/frameworks/native "$binder_dir"
fi
git -C "$binder_dir" sparse-checkout set --no-cone \
    'libs/binder/ndk/include_cpp/' 'libs/binder/ndk/include_ndk/' 'libs/binder/ndk/include_platform/'
git -C "$binder_dir" fetch --depth=1 --no-tags origin "$binder_revision"
git -C "$binder_dir" checkout --detach "$binder_revision"
git -C "$binder_dir" reset --hard "$binder_revision"

# protobuf v21.12 (3.21.12), the runtime dsa/protocol/cpp/generated was generated for.
protobuf_dir="$scratch/protobuf-21.12"
protobuf_revision="f0dc78d7e6e331b8c6bb2d5283e06aa26883ca7c"
if [[ ! -d "$protobuf_dir/.git" ]]; then
    git clone --filter=blob:none --no-checkout https://github.com/protocolbuffers/protobuf "$protobuf_dir"
fi
git -C "$protobuf_dir" fetch --depth=1 --no-tags origin "$protobuf_revision"
git -C "$protobuf_dir" checkout --detach "$protobuf_revision"
git -C "$protobuf_dir" reset --hard "$protobuf_revision"

# The gfxstream guest tree with DSA's Hyper-V socket patches, and Mesa's guest IOStream. A
# graphics build in the same scratch folder has fetched them already.
if [[ ! -d "$scratch/gfxstream/.git" || ! -d "$scratch/mesa3d/.git" ]]; then
    "$root/dsa/graphics/fetch.sh" "$scratch/gfxstream"
fi
