#!/usr/bin/env bash
# Fails if a merge left its conflict markers in a tracked file.
#
# A marker inside a comment, a docstring or a README still builds and passes every other test,
# so a botched merge resolution can reach develop unnoticed. This catches it.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

# The opening and closing markers, at the start of a line, followed by a space or the end of
# the line. A line of seven equals signs alone is left out: Markdown and reStructuredText use
# it as an underline.
hits="$(git grep -n -I -E -e '^<{7}( |$)' -e '^>{7}( |$)' -e '^\|{7}( |$)' -- \
    ':!scripts/tests/no-conflict-markers.test.sh' || true)"

if [[ -n "$hits" ]]; then
    echo "error: merge conflict markers found; resolve the merge before committing" >&2
    echo "$hits" >&2
    exit 1
fi
echo "no merge conflict markers"
