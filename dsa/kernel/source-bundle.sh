#!/usr/bin/env bash
# Writes the guest kernel's corresponding source as one tarball, to ship beside every
# release that ships the kernel (the GPL-2.0 asks that the source go with the binary).
#
# Usage: source-bundle.sh [--arch x86_64|arm64] <out.tar.gz> [config]
#
# The architecture (x86_64 unless --arch or DSA_KERNEL_ARCH says otherwise) is the kernel's:
# the README rebuilds bzImage for x86_64 and Image for arm64, as build.sh does.
#
# Run it after build.sh, which leaves the WSL kernel checkout at the pinned tag in
# .scratch/kernel/<tag>/src (or DSA_KERNEL_SRC). The tarball holds:
#   linux/        the WSL kernel source at the tag, as fetched (no build output)
#   dsa-kernel/   this directory: wsl-kernel.tag, every configuration fragment, patches/,
#                 build.sh
#   config        the merged .config the kernel was built with, when given
#   README        how to rebuild the shipped kernel (bzImage or Image) from the above
set -euo pipefail

arch="${DSA_KERNEL_ARCH:-x86_64}"
if [[ "${1:-}" == --arch ]]; then
    arch="${2:?--arch needs x86_64 or arm64}"
    shift 2
fi
out="${1:?usage: source-bundle.sh [--arch x86_64|arm64] <out.tar.gz> [config]}"
config="${2:-}"
# What build.sh does for each architecture, as the README tells it.
case "$arch" in
    x86_64)
        kernel_file=bzImage
        base_config=Microsoft/config-wsl
        fragments="android.config, virtio.config and dxg.config"
        make_args=""
        ;;
    arm64)
        kernel_file=Image
        base_config=Microsoft/config-wsl-arm64
        fragments="android.config, android-arm64.config, virtio.config and dxg.config"
        make_args="ARCH=arm64 "
        ;;
    *) echo "unknown architecture $arch (x86_64 or arm64)" >&2; exit 2 ;;
esac
kernel_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$kernel_dir/../.." && pwd)"
tag="$(tr -d '[:space:]' < "$kernel_dir/wsl-kernel.tag")"
src="${DSA_KERNEL_SRC:-$repo_root/.scratch/kernel/$tag/src}"

if [[ ! -d "$src/.git" ]]; then
    echo "no kernel checkout in $src; run build.sh first" >&2
    exit 1
fi

mkdir -p "$repo_root/.scratch"
stage="$(mktemp -d "$repo_root/.scratch/kernel-source.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/kernel-source"

# The checkout's HEAD is the tag as fetched; the patches are applied to the working tree
# only, so the archive is the upstream source and dsa-kernel/patches holds the changes.
git -C "$src" archive --format=tar --prefix=linux/ HEAD | tar -x -C "$stage/kernel-source"
mkdir -p "$stage/kernel-source/dsa-kernel"
cp -R "$kernel_dir/wsl-kernel.tag" "$kernel_dir"/*.config "$kernel_dir/build.sh" "$kernel_dir/patches" \
    "$stage/kernel-source/dsa-kernel/"
if [[ -n "$config" ]]; then
    cp "$config" "$stage/kernel-source/config"
fi
cat > "$stage/kernel-source/README" <<EOF
Desktop Subsystem for Android: guest kernel source

This is the complete corresponding source of the $arch guest kernel ($kernel_file) shipped
in the same release, licensed under the GNU General Public License, version 2
(linux/COPYING).

linux/        the WSL2 Linux kernel at tag $tag
              (https://github.com/microsoft/WSL2-Linux-Kernel)
dsa-kernel/   DSA's patches, configuration fragments and build script
config        the merged configuration the shipped kernel was built with

To rebuild: apply dsa-kernel/patches/*.patch to linux/ in order, copy
linux/$base_config to linux/.config, merge dsa-kernel/$fragments into it
(scripts/kconfig/merge_config.sh), run ${make_args}make olddefconfig and
${make_args}make $kernel_file. The result has the configuration in config.
dsa-kernel/build.sh --arch $arch does the same steps.
EOF

mkdir -p "$(dirname "$out")"
tar -C "$stage" -czf "$out" kernel-source
echo "wrote $out" >&2
