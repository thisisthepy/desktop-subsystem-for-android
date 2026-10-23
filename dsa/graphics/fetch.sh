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
gfxstream_patches=("$patch_dir"/0001-*.patch)
if ((${#gfxstream_patches[@]})); then
    git -C "$source" apply --recount --check "${gfxstream_patches[@]}"
    git -C "$source" apply --recount "${gfxstream_patches[@]}"
fi

git -C "$source" rev-parse HEAD

scratch_dir="$(cd "$(dirname "$source")" && pwd)"

# Pinned Mesa dependency for the guest Vulkan driver and connection manager:
mesa_dir="$scratch_dir/mesa3d"
mesa_revision="224e91e39836d3ca31f80df2e0379e55c70574f8"
if [[ ! -d "$mesa_dir/.git" ]]; then
    git clone --filter=blob:none --sparse https://android.googlesource.com/platform/external/mesa3d "$mesa_dir"
fi
git -C "$mesa_dir" fetch --depth=1 origin "$mesa_revision"
git -C "$mesa_dir" checkout --detach "$mesa_revision"
git -C "$mesa_dir" reset --hard "$mesa_revision"
git -C "$mesa_dir" clean -fdx
git -C "$mesa_dir" sparse-checkout set include src/gfxstream/guest src/util src/vulkan

mesa_patches=("$patch_dir"/0002-*.patch)
if ((${#mesa_patches[@]})); then
    git -C "$mesa_dir" apply --recount --check "${mesa_patches[@]}"
    git -C "$mesa_dir" apply --recount "${mesa_patches[@]}"
fi

git -C "$mesa_dir" rev-parse HEAD

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

# Generate Vulkan headers if generator script is present
gen_dir="$scratch_dir/gen"
mkdir -p "$gen_dir"
if [[ -f "$mesa_dir/src/vulkan/util/vk_entrypoints_gen.py" ]]; then
    python_cmd=()
    if command -v uv >/dev/null 2>&1; then
        python_cmd=(uv run --with mako python)
    elif python3 -c "import mako" >/dev/null 2>&1; then
        python_cmd=(python3)
    fi

    if ((${#python_cmd[@]})); then
        "${python_cmd[@]}" "$mesa_dir/src/vulkan/util/vk_entrypoints_gen.py" \
            --xml "$mesa_dir/src/vulkan/registry/vk.xml" \
            --proto --weak \
            --out-h "$gen_dir/gfxstream_vk_entrypoints.h" \
            --out-c "$gen_dir/gfxstream_vk_entrypoints.c" \
            --prefix gfxstream_vk --beta false

        "${python_cmd[@]}" "$mesa_dir/src/vulkan/util/vk_dispatch_table_gen.py" \
            --xml "$mesa_dir/src/vulkan/registry/vk.xml" \
            --out-h "$gen_dir/vk_dispatch_table.h" \
            --out-c "$gen_dir/vk_dispatch_table.c" \
            --beta false

        "${python_cmd[@]}" "$mesa_dir/src/vulkan/util/vk_extensions_gen.py" \
            --xml "$mesa_dir/src/vulkan/registry/vk.xml" \
            --out-h "$gen_dir/vk_extensions.h" \
            --out-c "$gen_dir/vk_extensions.c"

        "${python_cmd[@]}" "$mesa_dir/src/vulkan/util/vk_cmd_queue_gen.py" \
            --xml "$mesa_dir/src/vulkan/registry/vk.xml" \
            --out-h "$gen_dir/vk_cmd_queue.h" \
            --out-c "$gen_dir/vk_cmd_queue.c" \
            --beta false

        "${python_cmd[@]}" "$mesa_dir/src/vulkan/util/vk_physical_device_features_gen.py" \
            --xml "$mesa_dir/src/vulkan/registry/vk.xml" \
            --out-h "$gen_dir/vk_physical_device_features.h" \
            --out-c "$gen_dir/vk_physical_device_features.c" \
            --beta false

        "${python_cmd[@]}" "$mesa_dir/src/vulkan/util/vk_physical_device_properties_gen.py" \
            --xml "$mesa_dir/src/vulkan/registry/vk.xml" \
            --out-h "$gen_dir/vk_physical_device_properties.h" \
            --out-c "$gen_dir/vk_physical_device_properties.c" \
            --beta false
    fi
fi
