#!/usr/bin/env bash
# The release workflow publishes kernel-source.tar.gz in every release that ships the
# GPL-2.0 guest kernel, so the kernel's corresponding source always goes with it.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run scripts/tests/kernel_source_release_test.py
uv run scripts/tests/kernel_source_release.py

# Each kernel's source names that kernel: the arm64 job bundles with --arch arm64, and the
# bundle's README rebuilds bzImage for x86_64 and Image for arm64 from every fragment.
grep -Fq 'source-bundle.sh --arch arm64' .github/workflows/release-image.yml
bundle=dsa/kernel/source-bundle.sh
grep -Fq 'kernel_file=bzImage' "$bundle"
grep -Fq 'kernel_file=Image' "$bundle"
grep -Fq 'make $kernel_file' "$bundle"
grep -Fq '"$kernel_dir"/*.config' "$bundle"
if grep -Fq 'make bzImage' "$bundle"; then
  echo "the kernel source README rebuilds bzImage whatever the kernel" >&2
  exit 1
fi
