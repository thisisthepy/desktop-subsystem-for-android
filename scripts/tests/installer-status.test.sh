#!/usr/bin/env bash
# Checks that the installer test accepts the states Android can be in right after install:
# the window manager starts a stopped Android at sign-in, so it may already be starting.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-installer.yml"
grep -Fq "'^Android: (Stopped|Starting|Active|Dozing)'" "$workflow"
if grep -Fq "'^Android: Stopped'" "$workflow"; then
  echo "the installer test requires a stopped Android, which the window manager starts at sign-in" >&2
  exit 1
fi
echo "installer status check: ok"
