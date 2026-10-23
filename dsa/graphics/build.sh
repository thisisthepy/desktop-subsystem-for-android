#!/usr/bin/env bash
# Builds the guest libOpenglSystemCommon with Hyper-V socket transport using Android NDK and CMake.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch_arg="${1:-$root/.scratch}"
mkdir -p "$scratch_arg"
scratch="$(cd "$scratch_arg" && pwd)"

out_arg="${2:-$scratch/build-guest-gfxstream}"
mkdir -p "$out_arg"
out_dir="$(cd "$out_arg" && pwd)"

# Ensure source trees and dependencies are fetched
"$root/dsa/graphics/fetch.sh" "$scratch/gfxstream"

ndk_dir="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"
if [[ -z "$ndk_dir" && -d "/usr/local/lib/android/sdk/ndk" ]]; then
    ndk_dir="$(find /usr/local/lib/android/sdk/ndk -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
fi
if [[ -z "$ndk_dir" && -d "$HOME/AppData/Local/Android/Sdk/ndk" ]]; then
    ndk_dir="$(find "$HOME/AppData/Local/Android/Sdk/ndk" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
fi

if [[ -z "$ndk_dir" || ! -f "$ndk_dir/build/cmake/android.toolchain.cmake" ]]; then
    echo "Android NDK not found. Set ANDROID_NDK_HOME or ANDROID_NDK_ROOT." >&2
    exit 1
fi

cmake_bin="${CMAKE_BIN:-cmake}"
if ! command -v "$cmake_bin" >/dev/null 2>&1; then
    for candidate in \
        "$HOME/AppData/Local/Android/Sdk/cmake"/*/bin/cmake.exe \
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

"$cmake_bin" -B "$out_dir" -S "$root/dsa/graphics/cmake" \
    "${generator[@]}" \
    -DCMAKE_TOOLCHAIN_FILE="$ndk_dir/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI=x86_64 \
    -DANDROID_PLATFORM=android-34 \
    -DSCRATCH_DIR="$scratch"

"$cmake_bin" --build "$out_dir" --target OpenglSystemCommon

echo "Built library: $out_dir/libOpenglSystemCommon.so"
