#!/usr/bin/env bash
# The image CI builds the framework overlay with the SDK's aapt2. The step that makes room
# deletes the SDK, so the build-tools are kept first and aapt2 is passed by path.
set -euo pipefail

workflows="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows"
for name in test-image.yml release-image.yml; do
    workflow="$workflows/$name"
    grep -Fq -- '--aapt2 .scratch/build-tools/aapt2' "$workflow"
    keep=$(grep -n 'cp -r "$tools" .scratch/build-tools' "$workflow" | head -1 | cut -d: -f1)
    remove=$(grep -n 'rm -rf .*/usr/local/lib/android' "$workflow" | head -1 | cut -d: -f1)
    test -n "$keep" && test -n "$remove" && test "$keep" -lt "$remove"
done
