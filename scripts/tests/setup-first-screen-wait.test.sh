#!/usr/bin/env bash
# Checks the installer test waits long enough for setup.exe's first screen: the first start
# on a fresh runner can take minutes, and a 60 second wait failed on windows-2025.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-installer.yml"
step="$(awk '/- name: Take pictures of setup.exe.s first screens/{on=1} on&&/Save-SetupScreen/{print; exit} on' "$workflow")"
grep -Fq 'Wait-SetupControl InstallButton -Enabled -Seconds 300' <<<"$step"
echo "setup first screen wait: ok"
