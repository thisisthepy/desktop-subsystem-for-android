#!/usr/bin/env bash
# The image CI builds DSA's power and health HALs, checks they start on the image, and
# installs them.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-image.yml"
grep -Fq 'dsa/guest/power/build.sh' "$workflow"
grep -Fq 'guest-power-hal' "$workflow"
grep -Fq -- '--power-hal .scratch/power-hal' "$workflow"

# A released image is made the same way.
release="$(dirname "$workflow")/release-image.yml"
grep -Fq 'dsa/guest/power/build.sh' "$release"
grep -Fq 'guest-power-hal' "$release"
grep -Fq -- '--power-hal .scratch/power-hal' "$release"
