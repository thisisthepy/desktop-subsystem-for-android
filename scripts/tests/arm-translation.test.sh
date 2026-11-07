#!/usr/bin/env bash
# The ARM translation package tools' own tests: what goes into a package's system image and
# what it refuses, the translator build's lock and product, and the bundle assembly from a
# fake build output and fake images. Building the translator itself is the "Translator
# build" workflow's job and is not run here.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run dsa/guest/arm-translation/test_make_package.py
uv run dsa/guest/arm-translation/test_digitalis_lock.py
uv run dsa/guest/arm-translation/test_assemble_digitalis.py
