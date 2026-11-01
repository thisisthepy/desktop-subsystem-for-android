#!/usr/bin/env bash
# The input trace merge tool: it puts each click's host and guest stages on the host clock.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run scripts/tests/input_trace_test.py
