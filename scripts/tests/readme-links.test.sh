#!/usr/bin/env bash
# Fails if a README links a planning document.
#
# PROJECT.md, AGENTS.md and everything directly under docs/ are stripped from main, so a
# README that links them points at nothing for most readers.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

hits="$(git grep -n -I -E '\]\([^)]*(PROJECT\.md|AGENTS\.md|CLAUDE\.md|INTENT\.md|SPEC\.md)' -- \
    'README.md' '*/README.md' 'docs/locales/*' || true)"

if [[ -n "$hits" ]]; then
    echo "error: a README links a planning document that main does not have:" >&2
    echo "$hits" >&2
    exit 1
fi
echo "ok    no README links a planning document"
