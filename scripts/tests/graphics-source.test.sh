#!/usr/bin/env bash
# The gfxstream transport patch is source-controlled but the upstream tree is fetched only
# for a graphics build. Keep its pin and its required Hyper-V socket adapter visible to the
# fast repository gate.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

python3 dsa/graphics/test_fetch.py
python3 dsa/graphics/test_hvsocket_patch.py
python3 dsa/graphics/test_build.py
