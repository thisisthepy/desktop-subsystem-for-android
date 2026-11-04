#!/usr/bin/env bash
# Writes the guest kernel's corresponding source as one tarball, to ship beside every
# release that ships the kernel (the GPL-2.0 asks that the source go with the binary).
#
# Usage: source-bundle.sh <out.tar.gz> [config]
#
# Run it after build.sh, which leaves the WSL kernel checkout at the pinned tag in
# .scratch/kernel/<tag>/src (or DSA_KERNEL_SRC). The tarball holds:
#   linux/        the WSL kernel source at the tag, as fetched (no build output)
#   dsa-kernel/   this directory: wsl-kernel.tag, android.config, patches/, build.sh
#   config        the merged .config the kernel was built with, when given
#   README        how to rebuild the shipped bzImage from the above
set -euo pipefail

out="${1:?usage: source-bundle.sh <out.tar.gz> [config]}"
config="${2:-}"
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
cp -R "$kernel_dir/wsl-kernel.tag" "$kernel_dir/android.config" "$kernel_dir/build.sh" "$kernel_dir/patches" \
    "$stage/kernel-source/dsa-kernel/"
if [[ -n "$config" ]]; then
    cp "$config" "$stage/kernel-source/config"
fi
cat > "$stage/kernel-source/README" <<EOF
Desktop Subsystem for Android: guest kernel source

This is the complete corresponding source of the guest kernel (bzImage) shipped in the
same release, licensed under the GNU General Public License, version 2 (linux/COPYING).

linux/        the WSL2 Linux kernel at tag $tag
              (https://github.com/microsoft/WSL2-Linux-Kernel)
dsa-kernel/   DSA's patches, configuration fragment and build script
config        the merged configuration the shipped kernel was built with

To rebuild: apply dsa-kernel/patches/*.patch to linux/ in order, copy
linux/Microsoft/config-wsl to linux/.config, merge dsa-kernel/android.config into it
(scripts/kconfig/merge_config.sh), run make olddefconfig and make bzImage. The result
has the configuration in config. dsa-kernel/build.sh does the same steps.
EOF

mkdir -p "$(dirname "$out")"
tar -C "$stage" -czf "$out" kernel-source
echo "wrote $out" >&2
