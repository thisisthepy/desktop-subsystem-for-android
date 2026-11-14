#!/usr/bin/env bash
# Checks that the Test workflow never skips a change on develop: a newer push replaces a
# waiting one, so a push is compared with the last develop commit whose run passed, and the
# generated protocol code is checked on every push to develop.
set -euo pipefail

workflow="$(cd "$(dirname "$0")/../.." && pwd)/.github/workflows/test.yml"
grep -Fq 'cancel-in-progress: false' "$workflow"
grep -Fq -- '--status success --limit 1' "$workflow"
grep -Fq 'git merge-base --is-ancestor "$before" HEAD' "$workflow"
grep -Fq 'actions: read' "$workflow"
grep -Fq 'echo "protocol=true" >> "$GITHUB_OUTPUT"' "$workflow"
if grep -Fq 'github.event.before' "$workflow"; then
  echo "test.yml compares a push with the push before it, which a dropped run may never have tested" >&2
  exit 1
fi
# The protocol job runs on that output.
grep -Fq "if: needs.changes.outputs.protocol == 'true'" "$workflow"
echo "test workflow change detection: ok"
