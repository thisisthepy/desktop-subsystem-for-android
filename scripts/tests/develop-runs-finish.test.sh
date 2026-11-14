#!/usr/bin/env bash
# Checks that runs do not pile up and that no newer run cancels one in progress: every
# workflow has one concurrency group per workflow and branch (or a fixed one), never one per
# run or per commit, so at most one run goes and the newest waits, and none cancels a run
# that has started.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
for workflow in "$root"/.github/workflows/*.yml; do
  name="$(basename "$workflow")"
  # The top-level concurrency block, without comments.
  block="$(awk '{ sub(/\r$/, "") } /^concurrency:/ { on = 1; next } on && /^[^ #]/ { exit }
    on && !/^ *#/ { print }' "$workflow")"
  group="$(grep -E '^  group: ' <<<"$block" || true)"
  if [[ -z "$group" ]]; then
    echo "$name: no concurrency group, so its runs pile up" >&2
    exit 1
  fi
  if grep -Eq 'github\.(sha|run_id|run_number|run_attempt)' <<<"$group"; then
    echo "$name: the concurrency group is per run or per commit, so its runs pile up" >&2
    exit 1
  fi
  if grep -Fq 'github.' <<<"$group" && ! grep -Fq 'github.ref }}' <<<"$group"; then
    echo "$name: the concurrency group does not name the branch" >&2
    exit 1
  fi
  if ! grep -Fxq '  cancel-in-progress: false' <<<"$block"; then
    echo "$name: a newer run can cancel the run in progress" >&2
    exit 1
  fi
done
echo "runs finish: ok"
