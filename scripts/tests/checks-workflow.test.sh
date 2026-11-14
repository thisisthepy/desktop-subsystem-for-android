#!/usr/bin/env bash
# Checks that the development tests run only when started by hand, and that one run of
# Checks (checks.yml) can run all of them: each test workflow can be called, Checks calls
# every one, and no two of them keep an artifact under the same name, which one run refuses.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
workflows="$root/.github/workflows"
checks="$workflows/checks.yml"

# The top-level triggers of a workflow, without comments.
triggers() {
  awk '{ sub(/\r$/, "") } /^on:/ { on = 1; next } on && /^[^ #]/ { exit }
    on && /^  [a-z_]+:/ { sub(/:.*/, ""); sub(/^ +/, ""); print }' "$1"
}

if [[ "$(triggers "$checks")" != "workflow_dispatch" ]]; then
  echo "checks.yml: runs on something other than a start by hand" >&2
  exit 1
fi

for workflow in "$workflows"/test*.yml; do
  name="$(basename "$workflow")"
  if ! triggers "$workflow" | grep -Fxq workflow_call; then
    echo "$name: Checks cannot call it (no workflow_call)" >&2
    exit 1
  fi
  if ! grep -Eq "^    uses: \./\.github/workflows/$name\r?$" "$checks"; then
    echo "$name: Checks does not run it" >&2
    exit 1
  fi
  # Only the Graphics test, whose artifacts the builds take, still runs on a push.
  if [[ "$name" != test-graphics.yml ]] && triggers "$workflow" | grep -Fxq push; then
    echo "$name: runs on a push; development tests run only by hand" >&2
    exit 1
  fi
done

# The release pull request into main stays gated by the Test workflow.
triggers "$workflows/test.yml" | grep -Fxq pull_request

# Artifact names (the literal ones) are unique across the workflows Checks runs together.
duplicates="$(for workflow in "$workflows"/test*.yml; do
  awk '{ sub(/\r$/, "") } /uses: actions\/upload-artifact@/ { look = 4; next }
    look > 0 && /^ +name: / { sub(/^ +name: /, ""); if ($0 !~ /^\$\{\{/) print; look = 0; next }
    look > 0 { look-- }' "$workflow" | sort -u
done | sort | uniq -d)"
if [[ -n "$duplicates" ]]; then
  echo "artifact names kept by more than one test, which one Checks run refuses:" >&2
  echo "$duplicates" >&2
  exit 1
fi
echo "checks workflow: ok"
