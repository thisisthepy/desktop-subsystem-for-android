#!/usr/bin/env bash
# No two fields, oneof entries or enum values of one scope share a number, and no name is
# defined twice, in the protocol definitions.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run scripts/tests/proto_numbers_test.py
uv run scripts/tests/proto_numbers.py dsa/protocol/*.proto
