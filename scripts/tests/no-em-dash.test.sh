#!/usr/bin/env bash
# Fails if an em dash appears anywhere a human will read it.
#
# AGENTS.md forbids em dashes in docs, code comments, commit messages and UI copy. The
# rule is checked rather than remembered, because merges keep bringing them back.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

# U+2014 EM DASH. An en dash (U+2013) is allowed in numeric ranges. Every tracked text
# file is searched, not a list of extensions, and `-I` leaves out binaries. The escaped
# forms catch a dash written as a string-literal escape or an HTML entity.
dash=$'\xe2\x80\x94'
hits="$(git grep -n -I -e "$dash" -e '\\u2014' -e '\\u{2014}' -e '&mdash;' -e '&#8212;' -e '&#x2014;' -- \
    ':!scripts/tests/no-em-dash.test.sh' || true)"

if [[ -n "$hits" ]]; then
    echo "error: em dashes found (AGENTS.md forbids them; use a comma, a colon or parentheses)" >&2
    echo "$hits" >&2
    exit 1
fi
echo "ok    no em dashes"
