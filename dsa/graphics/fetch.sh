#!/usr/bin/env bash
# Fetches the gfxstream and Mesa sources that the DSA graphics transport patches target.
#
# The upstream trees are deliberately kept out of Git. This script places them under the
# repository's ignored .scratch directory, checks out the exact revisions, and applies the
# checked-in DSA patches. A graphics build must always begin here so a patch cannot silently
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
gfxstream_patches=("$patch_dir"/0001-*.patch "$patch_dir"/0004-*.patch)
if ((${#gfxstream_patches[@]})); then
    git -C "$source" apply --recount --check "${gfxstream_patches[@]}"
    git -C "$source" apply --recount "${gfxstream_patches[@]}"
fi

git -C "$source" rev-parse HEAD

scratch_dir="$(cd "$(dirname "$source")" && pwd)"

# Pinned Mesa dependency for the guest Vulkan driver. The whole tree: Mesa's Meson build
# generates and compiles the shared Vulkan code the driver links.
mesa_dir="$scratch_dir/mesa3d"
mesa_revision="224e91e39836d3ca31f80df2e0379e55c70574f8"
if [[ ! -d "$mesa_dir/.git" ]]; then
    git clone --filter=blob:none --no-checkout https://android.googlesource.com/platform/external/mesa3d "$mesa_dir"
fi
git -C "$mesa_dir" sparse-checkout disable
git -C "$mesa_dir" fetch --depth=1 origin "$mesa_revision"
git -C "$mesa_dir" checkout --detach "$mesa_revision"
git -C "$mesa_dir" reset --hard "$mesa_revision"
git -C "$mesa_dir" clean -fdx

mesa_patches=("$patch_dir"/0002-*.patch "$patch_dir"/0003-*.patch)
if ((${#mesa_patches[@]})); then
    git -C "$mesa_dir" apply --recount --check "${mesa_patches[@]}"
    git -C "$mesa_dir" apply --recount "${mesa_patches[@]}"
fi

git -C "$mesa_dir" rev-parse HEAD

# This Mesa has no libdrm wrap, and the Android target has no system libdrm to find.
cat > "$mesa_dir/subprojects/libdrm.wrap" <<'WRAP'
[wrap-git]
url = https://gitlab.freedesktop.org/mesa/drm.git
revision = libdrm-2.4.124
depth = 1
WRAP

# Shallow dependencies required to build the guest OpenglSystemCommon and Vulkan libraries:
minigbm_dir="$scratch_dir/minigbm"
if [[ ! -d "$minigbm_dir/.git" ]]; then
    git clone --depth=1 --filter=blob:none --sparse https://android.googlesource.com/platform/external/minigbm "$minigbm_dir"
fi
git -C "$minigbm_dir" sparse-checkout set cros_gralloc

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

# The platform's AHardwareBuffer and ANativeWindow headers. Mesa's Android stubs predate
# the calls the gfxstream gralloc and window code make.
frameworks_native_dir="$scratch_dir/frameworks_native"
if [[ ! -d "$frameworks_native_dir/.git" ]]; then
    git clone --depth=1 --filter=blob:none --sparse https://android.googlesource.com/platform/frameworks/native "$frameworks_native_dir"
fi
git -C "$frameworks_native_dir" sparse-checkout set libs/nativewindow/include libs/nativebase/include libs/arect/include

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
