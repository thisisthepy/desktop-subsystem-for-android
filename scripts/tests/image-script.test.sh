#!/usr/bin/env bash
# The image scripts' own tests: what make-image.py puts in the image and what check-image.py
# refuses. The image itself is made only by the image workflow; these run on every change.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run dsa/guest/image/test_make_image.py
