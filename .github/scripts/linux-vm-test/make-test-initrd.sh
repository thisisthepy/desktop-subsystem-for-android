#!/usr/bin/env bash
# Makes the initrd the launcher test boots the runner's own kernel with: a static init
# (test-init.c) that loads the virtio balloon and virtio-mem drivers and the modules they
# need, when the kernel has them as modules, and then idles. Without it the kernel finds no
# root, panics and resets the VM before its drivers are up, and the resize goes nowhere.
#
# Usage: make-test-initrd.sh <kernel release> <output initrd>
set -euo pipefail

release="$1"
out="$2"
here="$(cd "$(dirname "$0")" && pwd)"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

mkdir -p "$root/modules" "$root/dev"
gcc -static -O2 -o "$root/init" "$here/test-init.c"

n=0
seen=" "
# modprobe lists the dependencies first, then the module; a built-in one is "builtin". A
# module both drivers need goes in once.
while read -r kind path _; do
  [ "$kind" = insmod ] || continue
  case "$seen" in *" $path "*) continue ;; esac
  seen="$seen$path "
  dest="$root/modules/$(printf '%02d' "$n").ko"
  case "$path" in
    *.zst) zstd -q -d -c "$path" > "$dest" ;;
    *.xz) xz -d -c "$path" > "$dest" ;;
    *.gz) gzip -d -c "$path" > "$dest" ;;
    *) cp "$path" "$dest" ;;
  esac
  echo "initrd module $n: $path"
  n=$((n + 1))
done < <(for driver in virtio_balloon virtio_mem; do
  modprobe -S "$release" --show-depends "$driver" || true
done)

(cd "$root" && find . | cpio -o -H newc --quiet) > "$out"
ls -l "$out"
