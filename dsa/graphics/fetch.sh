#!/usr/bin/env bash
# Fetches the gfxstream source that the DSA graphics transport patches target.
#
# The upstream tree is deliberately kept out of Git.  This script places it under the
# repository's ignored .scratch directory, checks out the exact revision, and applies the
# checked-in DSA patches.  A graphics build must always begin here so a patch cannot silently
# land on a different upstream API.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source="${1:-$root/.scratch/gfxstream}"
revision="d047a57228332d995d36600792fa9ccc26cf8ae6"
remote="https://android.googlesource.com/platform/hardware/google/gfxstream"
patch_dir="$root/dsa/graphics/patches"

if [[ -e "$source" && ! -d "$source/.git" ]]; then
    echo "gfxstream source path is not a Git checkout: $source" >&2
    exit 1
fi

if [[ ! -d "$source/.git" ]]; then
    git clone "$remote" "$source"
fi

git -C "$source" fetch --no-tags origin "$revision"
git -C "$source" checkout --detach "$revision"
git -C "$source" reset --hard "$revision"
git -C "$source" clean -fdx

shopt -s nullglob
patches=("$patch_dir"/*.patch)
if ((${#patches[@]})); then
    git -C "$source" apply --recount --check "${patches[@]}"
    git -C "$source" apply --recount "${patches[@]}"
fi

git -C "$source" rev-parse HEAD

scratch_dir="$(cd "$(dirname "$source")" && pwd)"

# Shallow dependencies required to build the guest OpenglSystemCommon library:
mesa_dir="$scratch_dir/mesa3d"
if [[ ! -d "$mesa_dir/.git" ]]; then
    git clone --depth=1 --filter=blob:none --sparse https://android.googlesource.com/platform/external/mesa3d "$mesa_dir"
fi
git -C "$mesa_dir" sparse-checkout set src/gfxstream/guest src/util

libhardware_dir="$scratch_dir/libhardware"
if [[ ! -d "$libhardware_dir/.git" ]]; then
    git clone --depth=1 https://android.googlesource.com/platform/hardware/libhardware "$libhardware_dir"
fi

libbase_dir="$scratch_dir/libbase"
if [[ ! -d "$libbase_dir/.git" ]]; then
    git clone --depth=1 https://android.googlesource.com/platform/system/libbase "$libbase_dir"
fi

system_core_dir="$scratch_dir/system_core"
if [[ ! -d "$system_core_dir/.git" ]]; then
    git clone --depth=1 --filter=blob:none --sparse https://android.googlesource.com/platform/system/core "$system_core_dir"
fi
git -C "$system_core_dir" sparse-checkout set libcutils/include libutils/include libnativebase/include libsystem/include

system_logging_dir="$scratch_dir/system_logging"
if [[ ! -d "$system_logging_dir/.git" ]]; then
    git clone --depth=1 --filter=blob:none --sparse https://android.googlesource.com/platform/system/logging "$system_logging_dir"
fi
git -C "$system_logging_dir" sparse-checkout set liblog/include

# Shallow dependencies required to build the host gfxstream renderer:
aemu_dir="$scratch_dir/aemu"
aemu_revision="3ff20aed307f55bc4ae865373d07bf76c9efa43c"
if [[ ! -d "$aemu_dir/.git" ]]; then
    git clone https://android.googlesource.com/platform/hardware/google/aemu "$aemu_dir"
fi
git -C "$aemu_dir" fetch --no-tags origin "$aemu_revision"
git -C "$aemu_dir" checkout --detach "$aemu_revision"
git -C "$aemu_dir" reset --hard "$aemu_revision"

angle_dir="$scratch_dir/angle"
if [[ ! -d "$angle_dir/.git" ]]; then
    git clone --depth=1 --filter=blob:none --sparse https://android.googlesource.com/platform/external/angle "$angle_dir"
fi
git -C "$angle_dir" sparse-checkout set include
