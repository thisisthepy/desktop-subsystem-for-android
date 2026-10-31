#!/usr/bin/env bash
# Builds the guest libOpenglSystemCommon and vulkan.ranchu with Hyper-V socket transport using the
# Android NDK, Mesa's Meson build for its shared Vulkan code, and CMake for the gfxstream parts.
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

# Mesa's shared Vulkan code (the runtime, util, WSI and their generated sources), from
# Mesa's own Meson build for the Android target.
host_tag="linux-x86_64"
exe=""
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) host_tag="windows-x86_64"; exe=".exe" ;;
esac
llvm_bin="$ndk_dir/toolchains/llvm/prebuilt/$host_tag/bin"
mesa_build="$scratch/build-mesa"
mkdir -p "$mesa_build"
cat > "$mesa_build/android-x86_64.ini" <<CROSS
[binaries]
ar = '$llvm_bin/llvm-ar$exe'
nm = '$llvm_bin/llvm-nm$exe'
strip = '$llvm_bin/llvm-strip$exe'
c = ['$llvm_bin/clang$exe', '--target=x86_64-linux-android34']
cpp = ['$llvm_bin/clang++$exe', '--target=x86_64-linux-android34', '-fno-exceptions', '-fno-unwind-tables', '-fno-asynchronous-unwind-tables', '-static-libstdc++']
c_ld = 'lld'
cpp_ld = 'lld'
[host_machine]
system = 'android'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
CROSS

mesa_venv="$scratch/build-mesa-venv"
if [[ ! -d "$mesa_venv" ]]; then
    uv venv -q "$mesa_venv"
fi
mesa_python="$mesa_venv/bin/python"
[[ -x "$mesa_python" ]] || mesa_python="$mesa_venv/Scripts/python.exe"
uv pip install -q --python "$mesa_python" meson ninja mako pyyaml packaging
mesa_bin="$(dirname "$mesa_python")"

if [[ ! -f "$mesa_build/out/build.ninja" ]]; then
    PATH="$mesa_bin:$PATH" meson setup "$mesa_build/out" "$scratch/mesa3d" \
        --cross-file "$mesa_build/android-x86_64.ini" --buildtype=release \
        --force-fallback-for=libdrm \
        -Dlibdrm:intel=disabled -Dlibdrm:radeon=disabled -Dlibdrm:amdgpu=disabled \
        -Dlibdrm:nouveau=disabled -Dlibdrm:vmwgfx=disabled -Dlibdrm:tests=false \
        -Dlibdrm:man-pages=disabled -Dlibdrm:valgrind=disabled -Dlibdrm:cairo-tests=disabled \
        -Dplatforms=android -Dplatform-sdk-version=34 -Dandroid-stub=true \
        -Dgallium-drivers= -Dvulkan-drivers=gfxstream -Dllvm=disabled -Dgbm=disabled \
        -Degl=disabled -Dopengl=false -Dglx=disabled -Dgles1=disabled -Dgles2=disabled \
        -Dzstd=disabled -Dandroid-libbacktrace=disabled
fi
PATH="$mesa_bin:$PATH" ninja -C "$mesa_build/out" \
    src/c11/impl/libmesa_util_c11.a src/util/libmesa_util.a src/util/libmesa_util_sse41.a \
    src/util/libxmlconfig.a src/util/blake3/libblake3.a src/util/u_gralloc/lib_mesa_u_gralloc.a \
    src/compiler/libcompiler.a src/vulkan/util/libvulkan_util.a \
    src/vulkan/runtime/libvulkan_lite_runtime.a src/vulkan/runtime/libvulkan_lite_instance.a \
    src/vulkan/wsi/libvulkan_wsi.a src/gfxstream/aemu/libaemu.a \
    src/gfxstream/guest/vulkan_enc/gfxstream_vk_entrypoints.c \
    src/android_stub/libcutils.so src/android_stub/libhardware.so src/android_stub/libsync.so \
    src/android_stub/liblog.so subprojects/libdrm/libdrm.so

"$cmake_bin" -B "$out_dir" -S "$root/dsa/graphics/cmake" \
    "${generator[@]}" \
    -DCMAKE_TOOLCHAIN_FILE="$ndk_dir/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI=x86_64 \
    -DANDROID_PLATFORM=android-34 \
    -DSCRATCH_DIR="$scratch" \
    -DMESA_BUILD_DIR="$mesa_build/out"

"$cmake_bin" --build "$out_dir" --target OpenglSystemCommon vulkan.ranchu

echo "Built library: $out_dir/libOpenglSystemCommon.so"
echo "Built library: $out_dir/vulkan.ranchu.so"
