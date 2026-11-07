#!/usr/bin/env bash
# Checks that the macOS package test reads the Icon Composer source as JSON: plutil -lint
# reads only property lists and rejects every JSON file.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test-macos-package.yml"
grep -Fq 'jq -e .groups DSA.icon/icon.json' "$workflow"
if grep -Eq 'plutil -lint [^ ]*\.json' "$workflow"; then
  echo "test-macos-package.yml lints a JSON file with plutil, which reads only property lists" >&2
  exit 1
fi
echo "macOS package workflow: ok"
