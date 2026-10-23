#!/usr/bin/env bash
# The image CI consumes the exact guest transport artifact that the graphics CI built.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-image.yml"
grep -Fq 'graphics_run:' "$workflow"
grep -Fq 'guest-gfxstream-hvsocket' "$workflow"
grep -Fq 'gh run download' "$workflow"
grep -Fq -- '--graphics-library' "$workflow"
