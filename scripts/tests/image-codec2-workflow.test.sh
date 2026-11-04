#!/usr/bin/env bash
# The image CI builds DSA's codec HAL, checks it starts on the image, and installs it.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-image.yml"
grep -Fq 'dsa/guest/codec2/build.sh' "$workflow"
grep -Fq 'guest-codec2' "$workflow"
grep -Fq -- '--codec2 .scratch/codec2/dsa-codec2' "$workflow"

# A released image is made the same way.
release="$(dirname "$workflow")/release-image.yml"
grep -Fq 'dsa/guest/codec2/build.sh' "$release"
grep -Fq 'guest-codec2' "$release"
grep -Fq -- '--codec2 .scratch/codec2/dsa-codec2' "$release"
