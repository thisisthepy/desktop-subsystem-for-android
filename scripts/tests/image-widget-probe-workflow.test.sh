#!/usr/bin/env bash
# The image CI builds the widget probe, an ordinary app whose widgets need a setup, and the
# widget boot check places them on the booted image: each setup must open as a window of its
# own, and its answer must decide whether the widget is placed.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-image.yml"
grep -Fq 'bash dsa/guest/widget-probe/build.sh .scratch/widget-probe' "$workflow"
grep -Fq 'name: guest-widget-probe' "$workflow"
grep -Fq 'DSA_WIDGET_PROBE="$GITHUB_WORKSPACE/.scratch/widget-probe/dsa-widget-probe.apk"' "$workflow"
grep -Fq '"dsa/guest/widget-probe/**"' "$workflow"
grep -Fq 'check: Place a widget on the booted image' "$workflow"
echo "image widget probe workflow: ok"
