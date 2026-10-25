#!/usr/bin/env bash
# Builds dsa-hwcomposer, the guest's composer HAL, for x86_64 Android with the NDK.
#
#   dsa/guest/hwcomposer/build.sh [scratch dir] [output dir]
#
# It fetches the pinned sources (fetch.sh), generates the binder code for the composer HAL
# and the control channel with the Android SDK's aidl, and builds with CMake. The binary
# lands at <output dir>/dsa-hwcomposer (default .scratch/build-hwcomposer).
#
# ANDROID_NDK_HOME (or ANDROID_NDK_ROOT) names the NDK, ANDROID_HOME (or ANDROID_SDK_ROOT)
# the SDK whose newest build-tools provide aidl. Both are found in their usual places when
# unset.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
scratch_arg="${1:-$root/.scratch}"
mkdir -p "$scratch_arg"
scratch="$(cd "$scratch_arg" && pwd)"
out_arg="${2:-$scratch/build-hwcomposer}"
mkdir -p "$out_arg"
out="$(cd "$out_arg" && pwd)"

"$here/fetch.sh" "$scratch"

windows=false
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) windows=true ;;
esac
# aidl on Windows compares import paths with backslashes.
native_path() {
    if $windows; then cygpath -w "$1"; else printf '%s\n' "$1"; fi
}

sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
for candidate in "$sdk" /usr/local/lib/android/sdk "$HOME/AppData/Local/Android/Sdk" "$HOME/Android/Sdk"; do
    if [[ -n "$candidate" && -d "$candidate/build-tools" ]]; then
        sdk="$candidate"
        break
    fi
done
aidl=""
if [[ -d "$sdk/build-tools" ]]; then
    for tools in $(find "$sdk/build-tools" -mindepth 1 -maxdepth 1 -type d | sort -V -r); do
        if [[ -x "$tools/aidl" || -x "$tools/aidl.exe" ]]; then
            aidl="$tools/aidl"
            [[ -x "$aidl" ]] || aidl="$tools/aidl.exe"
            break
        fi
    done
fi
if [[ -z "$aidl" ]]; then
    echo "aidl not found: install Android SDK build-tools and set ANDROID_HOME" >&2
    exit 1
fi
echo "aidl: $aidl"

# The binder code for the composer HAL (frozen version 4 and the packages it imports) and
# for the control channel's service.
gen="$out/aidl"
rm -rf "$gen"
mkdir -p "$gen/include" "$gen/src"
api="$scratch/hardware_interfaces"
composer3="$api/graphics/composer/aidl/aidl_api/android.hardware.graphics.composer3/4"
graphics_common="$api/graphics/common/aidl/aidl_api/android.hardware.graphics.common/6"
hardware_common="$api/common/aidl/aidl_api/android.hardware.common/2"
drm_common="$api/drm/common/aidl/aidl_api/android.hardware.drm.common/1"
channel="$root/dsa/guest/aidl"

# generate <frozen dir> <version> [<import dir>...]: every type in the package, or only the
# ones named in $types when it is set.
generate() {
    local dir="$1" version="$2"
    shift 2
    local args=(--lang=ndk --structured --stability=vintf --version="$version")
    if [[ -f "$dir/.hash" ]]; then
        args+=(--hash="$(tail -n 1 "$dir/.hash")")
    fi
    for import in "$@"; do
        args+=(-I "$(native_path "$import")")
    done
    local files=()
    if [[ -n "${types:-}" ]]; then
        local package
        package="$(find "$dir" -name '*.aidl' -print -quit | xargs dirname)"
        for type in $types; do
            files+=("$(native_path "$package/$type.aidl")")
        done
    else
        while IFS= read -r file; do
            files+=("$(native_path "$file")")
        done < <(find "$dir" -name '*.aidl' | sort)
    fi
    "$aidl" "${args[@]}" -I "$(native_path "$dir")" \
        -h "$(native_path "$gen/include")" -o "$(native_path "$gen/src")" "${files[@]}"
}

generate "$hardware_common" 2
generate "$drm_common" 1
# Only the graphics.common types composer3 uses. The SDK's aidl cannot evaluate the frozen
# BufferUsage (a 64-bit constant expression it takes as overflowing), and nothing here
# needs it.
types="AlphaInterpretation BlendMode Dataspace DisplayDecorationSupport DisplayHotplugEvent FRect
Hdr HdrConversionCapability HdrConversionStrategy PixelFormat Point Rect Transform"     generate "$graphics_common" 6 "$hardware_common"
generate "$composer3" 4 "$graphics_common" "$hardware_common" "$drm_common"
generate "$channel" 1

ndk_dir="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"
for candidate in /usr/local/lib/android/sdk/ndk "$HOME/AppData/Local/Android/Sdk/ndk"; do
    if [[ -z "$ndk_dir" && -d "$candidate" ]]; then
        ndk_dir="$(find "$candidate" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
    fi
done
if [[ -z "$ndk_dir" || ! -f "$ndk_dir/build/cmake/android.toolchain.cmake" ]]; then
    echo "Android NDK not found. Set ANDROID_NDK_HOME or ANDROID_NDK_ROOT." >&2
    exit 1
fi

cmake_bin="${CMAKE_BIN:-cmake}"
if ! command -v "$cmake_bin" >/dev/null 2>&1; then
    for candidate in "$HOME/AppData/Local/Android/Sdk/cmake"/*/bin/cmake.exe \
        "/usr/local/lib/android/sdk/cmake"/*/bin/cmake; do
        if [[ -f "$candidate" ]]; then
            cmake_bin="$candidate"
            break
        fi
    done
fi
generator=()
if command -v ninja >/dev/null 2>&1; then
    generator=(-G Ninja)
elif [[ -f "${cmake_bin%/*}/ninja" ]]; then
    generator=(-G Ninja -DCMAKE_MAKE_PROGRAM="${cmake_bin%/*}/ninja")
elif [[ -f "${cmake_bin%/*}/ninja.exe" ]]; then
    generator=(-G Ninja -DCMAKE_MAKE_PROGRAM="${cmake_bin%/*}/ninja.exe")
fi

"$cmake_bin" -B "$out/cmake" -S "$here" "${generator[@]}" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_TOOLCHAIN_FILE="$ndk_dir/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI=x86_64 \
    -DANDROID_PLATFORM=android-34 \
    -DSCRATCH_DIR="$scratch" \
    -DAIDL_GEN_DIR="$gen"
"$cmake_bin" --build "$out/cmake" --target dsa-hwcomposer

cp "$out/cmake/dsa-hwcomposer" "$out/dsa-hwcomposer"
echo "Built: $out/dsa-hwcomposer"
