#!/usr/bin/env bash
# The image CI builds DSA's hwcomposer, checks it starts on the image, and installs it.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-image.yml"
grep -Fq 'dsa/guest/hwcomposer/build.sh' "$workflow"
grep -Fq 'guest-hwcomposer' "$workflow"
grep -Fq 'check-guest-driver.py --executable' "$workflow"
grep -Fq -- '--hwcomposer' "$workflow"
