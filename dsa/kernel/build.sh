#!/usr/bin/env bash
# Builds the guest kernel: the WSL kernel source at the tag in wsl-kernel.tag, configured
# with WSL's configuration for the architecture plus android.config:
#
#   x86_64  Microsoft/config-wsl + android.config + virtio.config + dxg.config, built as
#           bzImage
#   arm64   Microsoft/config-wsl-arm64 + android.config + android-arm64.config +
#           virtio.config + dxg.config, built as Image (the EFI stub Hyper-V's UEFI
#           firmware starts on ARM64 Windows hosts, and the uncompressed Image Apple's
#           Virtualization framework boots directly on Apple silicon Macs)
#
#   dsa/kernel/build.sh                         configure, check, and build for x86_64
#   dsa/kernel/build.sh --arch arm64            the same for arm64
#   dsa/kernel/build.sh [--arch A] --config-only  configure and check only (about a minute)
#
# The architecture can also come from DSA_KERNEL_ARCH. An arm64 kernel is built natively on
# an arm64 machine and cross-compiled with aarch64-linux-gnu- anywhere else.
#
# The source is fetched into .scratch/kernel/<tag>/src unless DSA_KERNEL_SRC names an
# existing checkout of that tag. Each architecture builds out of tree in
# .scratch/kernel/<tag>/build-<arch>, so both can come from one checkout. The kernel and its
# .config are copied to .scratch/kernel/<tag>/out/ (out-arm64/ for arm64), or to
# DSA_KERNEL_OUT.
#
# A full build takes tens of minutes and most of the machine; CI runs it ("Kernel test").
# Build dependencies are those of the WSL kernel README: gcc, make, flex, bison, bc,
# libelf, openssl headers. TOOLS_ROOT points at an extracted root (usr/bin, usr/include,
# usr/lib/<triplet>) if they are not installed system wide.
set -euo pipefail

kernel_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$kernel_dir/../.." && pwd)"
tag="$(tr -d '[:space:]' < "$kernel_dir/wsl-kernel.tag")"
work="$repo_root/.scratch/kernel/$tag"
src="${DSA_KERNEL_SRC:-$work/src}"
arch="${DSA_KERNEL_ARCH:-x86_64}"
config_only=false
while (( $# > 0 )); do
    case "$1" in
        --config-only) config_only=true ;;
        --arch) arch="${2:?--arch needs x86_64 or arm64}"; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

case "$arch" in
    x86_64)
        base_config="Microsoft/config-wsl"
        fragments=("$kernel_dir/android.config" "$kernel_dir/virtio.config" "$kernel_dir/dxg.config")
        image_target=bzImage
        image_path=arch/x86/boot/bzImage
        kernel_arch=x86_64
        native_machine=x86_64
        cross_compile=x86_64-linux-gnu-
        default_out="$work/out"
        ;;
    arm64)
        base_config="Microsoft/config-wsl-arm64"
        fragments=("$kernel_dir/android.config" "$kernel_dir/android-arm64.config" "$kernel_dir/virtio.config" "$kernel_dir/dxg.config")
        image_target=Image
        image_path=arch/arm64/boot/Image
        kernel_arch=arm64
        native_machine=aarch64
        cross_compile=aarch64-linux-gnu-
        default_out="$work/out-arm64"
        ;;
    *) echo "unknown architecture $arch (x86_64 or arm64)" >&2; exit 2 ;;
esac
build="$work/build-$arch"
out="${DSA_KERNEL_OUT:-$default_out}"

make_flags=(-C "$src" O="$build" ARCH="$kernel_arch" -j"$(nproc)" LOCALVERSION=)
if [[ "$(uname -m)" != "$native_machine" ]]; then
    make_flags+=(CROSS_COMPILE="$cross_compile")
fi
if [[ -n "${TOOLS_ROOT:-}" ]]; then
    triplet="$(gcc -dumpmachine)"
    export PATH="$TOOLS_ROOT/usr/bin:$PATH"
    export LD_LIBRARY_PATH="$TOOLS_ROOT/usr/lib/$triplet${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    make_flags+=(HOSTCFLAGS="-I$TOOLS_ROOT/usr/include" HOSTLDFLAGS="-L$TOOLS_ROOT/usr/lib/$triplet")
fi

if [[ ! -f "$src/Makefile" ]]; then
    echo "fetching $tag into $src" >&2
    mkdir -p "$src"
    git -C "$src" init -q
    git -C "$src" fetch -q --depth 1 https://github.com/microsoft/WSL2-Linux-Kernel tag "$tag"
    git -C "$src" checkout -q FETCH_HEAD
fi

shopt -s nullglob
for patch in "$kernel_dir"/patches/*.patch; do
    if ! git -C "$src" apply --reverse --check "$patch" >/dev/null 2>&1; then
        git -C "$src" apply "$patch"
    fi
done

# An out-of-tree build needs a source tree with no configuration of its own; an older
# checkout that was built in place has one.
if [[ -f "$src/.config" ]]; then
    make -s -C "$src" ARCH="$kernel_arch" mrproper
fi

# Merge: WSL's configuration, then the fragments, then fill in every option the fragments'
# choices make newly visible with its default.
mkdir -p "$build"
config="$build/.config"
cp "$src/$base_config" "$config"
(cd "$src" && ARCH="$kernel_arch" KCONFIG_CONFIG="$config" \
    ./scripts/kconfig/merge_config.sh -m -Q "$config" "${fragments[@]}" >/dev/null)
make -s "${make_flags[@]}" olddefconfig

# Check that the merge kept every line of each fragment.
failed=0
for fragment in "${fragments[@]}"; do
    while IFS= read -r line; do
        [[ "$line" =~ ^CONFIG_[A-Z0-9_]+= ]] || continue
        if ! grep -qxF -- "$line" "$config"; then
            name="${line%%=*}"
            actual="$(grep -E "^(# )?$name[= ]" "$config" || echo "$name missing")"
            echo "not applied ($arch, $(basename "$fragment")): $line (the kernel has: $actual); a dependency is missing" >&2
            failed=1
        fi
    done < "$fragment"
done

# Android must not need ashmem: it left the kernel in 6.6 and a guest that relies on it
# breaks on newer kernels. memfd replaces it.
if grep -qE '^CONFIG_ASHMEM=' "$config"; then
    echo "CONFIG_ASHMEM is set; the guest must run without ashmem" >&2
    failed=1
fi
grep -qx 'CONFIG_MEMFD_CREATE=y' "$config" || { echo "CONFIG_MEMFD_CREATE is not set" >&2; failed=1; }
(( failed == 0 )) || exit 1

# `make kernelrelease` reads a file only a build regenerates, so derive the release from
# the version and the configured local version.
release="$(make -s "${make_flags[@]}" kernelversion)$(sed -n 's/^CONFIG_LOCALVERSION="\(.*\)"$/\1/p' "$config")"
echo "configured $release ($arch) from $tag and $base_config with $(cat "${fragments[@]}" | grep -cE '^CONFIG_') fragment options" >&2

mkdir -p "$out"
cp "$config" "$out/config-$release"
$config_only && exit 0

make -s "${make_flags[@]}" "$image_target"
built_release="$(cat "$build/include/config/kernel.release")"
if [[ "$built_release" != "$release" ]]; then
    echo "the build produced $built_release, expected $release" >&2
    exit 1
fi
cp "$build/$image_path" "$out/$image_target-$release"
echo "built $out/$image_target-$release" >&2
