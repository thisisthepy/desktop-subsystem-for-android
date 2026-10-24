#!/usr/bin/env bash
# A kept image carries the disks make-image.py writes, so it can be booted on a real host.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
workflow="$root/.github/workflows/test-image.yml"
keep="$(sed -n '/name: Keep the guest disk/,/if-no-files-found/p' "$workflow")"
for file in system.vhdx data.vhdx image.json initrd.img; do
  grep -Fq "dsa/target/image/*/$file" <<<"$keep" || { echo "the kept disk lacks $file"; exit 1; }
  grep -Fq "$file" "$root/dsa/guest/image/make-image.py" || { echo "make-image.py writes no $file"; exit 1; }
done
if grep -Fq "disk.vhdx" <<<"$keep"; then
  echo "the kept disk names disk.vhdx, which make-image.py no longer writes"
  exit 1
fi
echo "ok"
