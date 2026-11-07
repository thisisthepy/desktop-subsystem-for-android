#!/usr/bin/env bash
# Checks that develop runs do not pile up and that no newer merge cancels a run in progress:
# every test workflow shares one concurrency group per workflow on develop, not one per
# commit, and cancels in progress only away from develop.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
for workflow in "$root"/.github/workflows/test*.yml; do
  name="$(basename "$workflow")"
  group="$(grep -F "  group: " "$workflow")"
  if grep -Fq "github.sha" <<<"$group" || ! grep -Fq "github.ref }}" <<<"$group"; then
    echo "$name: develop pushes do not share one concurrency group, so runs pile up" >&2
    exit 1
  fi
  if ! grep -Fxq "  cancel-in-progress: \${{ github.ref != 'refs/heads/develop' }}" "$workflow"; then
    echo "$name: a newer push to develop can cancel the run in progress" >&2
    exit 1
  fi
done
echo "develop runs finish: ok"
