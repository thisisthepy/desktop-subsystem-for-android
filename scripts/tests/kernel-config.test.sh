#!/usr/bin/env bash
# Checks the guest kernel configuration fragment against its constraints.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

python3 dsa/kernel/test_android_config.py
