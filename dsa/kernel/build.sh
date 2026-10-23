#!/usr/bin/env bash
# Builds the guest kernel: the WSL kernel source at the tag in wsl-kernel.tag, configured
# with Microsoft/config-wsl plus android.config.
#
#   dsa/kernel/build.sh                 configure, check, and build bzImage
#   dsa/kernel/build.sh --config-only   configure and check only (about a minute)
#
# The source is fetched into .scratch/kernel/<tag>/src unless DSA_KERNEL_SRC names an
# existing checkout of that tag. The kernel and its .config are copied to
# .scratch/kernel/<tag>/out/, or to DSA_KERNEL_OUT.
#
# A full build takes tens of minutes and most of the machine; CI runs it ("Kernel test").
# Build dependencies are those of the WSL kernel README: gcc, make, flex, bison, bc,
# libelf, openssl headers. TOOLS_ROOT points at an extracted root (usr/bin, usr/include,
# usr/lib/<triplet>) if they are not installed system wide.
set -euo pipefail

kernel_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$kernel_dir/../.." && pwd)"
tag="$(tr -d '[:space:]' < "$kernel_dir/wsl-kernel.tag")"
fragment="$kernel_dir/android.config"
work="$repo_root/.scratch/kernel/$tag"
src="${DSA_KERNEL_SRC:-$work/src}"
out="${DSA_KERNEL_OUT:-$work/out}"
config_only=false
[[ "${1:-}" == "--config-only" ]] && config_only=true

make_flags=(-C "$src" -j"$(nproc)" LOCALVERSION=)
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

# Merge: config-wsl, then the fragment, then fill in every option the fragment's choices
# make newly visible with its default.
cp "$src/Microsoft/config-wsl" "$src/.config"
(cd "$src" && KCONFIG_CONFIG=.config ./scripts/kconfig/merge_config.sh -m -Q .config "$fragment" >/dev/null)
make -s "${make_flags[@]}" olddefconfig

# Check that the merge kept every line of the fragment.
failed=0
while IFS= read -r line; do
    [[ "$line" =~ ^CONFIG_[A-Z0-9_]+= ]] || continue
    if ! grep -qxF -- "$line" "$src/.config"; then
        name="${line%%=*}"
        actual="$(grep -E "^(# )?$name[= ]" "$src/.config" || echo "$name missing")"
        echo "not applied: $line (the kernel has: $actual); a dependency is missing" >&2
        failed=1
    fi
done < "$fragment"

# Android must not need ashmem: it left the kernel in 6.6 and a guest that relies on it
# breaks on newer kernels. memfd replaces it.
if grep -qE '^CONFIG_ASHMEM=' "$src/.config"; then
    echo "CONFIG_ASHMEM is set; the guest must run without ashmem" >&2
    failed=1
fi
grep -qx 'CONFIG_MEMFD_CREATE=y' "$src/.config" || { echo "CONFIG_MEMFD_CREATE is not set" >&2; failed=1; }
(( failed == 0 )) || exit 1

# `make kernelrelease` reads a file only a build regenerates, so derive the release from
# the version and the configured local version.
release="$(make -s "${make_flags[@]}" kernelversion)$(sed -n 's/^CONFIG_LOCALVERSION="\(.*\)"$/\1/p' "$src/.config")"
echo "configured $release from $tag with $(grep -cE '^CONFIG_' "$fragment") fragment options" >&2

mkdir -p "$out"
cp "$src/.config" "$out/config-$release"
$config_only && exit 0

make -s "${make_flags[@]}" bzImage
built_release="$(cat "$src/include/config/kernel.release")"
if [[ "$built_release" != "$release" ]]; then
    echo "the build produced $built_release, expected $release" >&2
    exit 1
fi
cp "$src/arch/x86/boot/bzImage" "$out/bzImage-$release"
echo "built $out/bzImage-$release" >&2
