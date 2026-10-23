#!/usr/bin/env bash
# Fetches the gfxstream source that the DSA graphics transport patches target.
#
# The upstream tree is deliberately kept out of Git.  This script places it under the
# repository's ignored .scratch directory, checks out the exact revision, and applies the
# checked-in DSA patches.  A graphics build must always begin here so a patch cannot silently
# land on a different upstream API.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source="${1:-$root/.scratch/gfxstream}"
revision="d047a57228332d995d36600792fa9ccc26cf8ae6"
remote="https://android.googlesource.com/platform/hardware/google/gfxstream"
patch_dir="$root/dsa/graphics/patches"

if [[ -e "$source" && ! -d "$source/.git" ]]; then
    echo "gfxstream source path is not a Git checkout: $source" >&2
    exit 1
fi

if [[ ! -d "$source/.git" ]]; then
    git clone "$remote" "$source"
fi

git -C "$source" fetch --no-tags origin "$revision"
git -C "$source" checkout --detach "$revision"
git -C "$source" clean -fdx

shopt -s nullglob
patches=("$patch_dir"/*.patch)
if ((${#patches[@]})); then
    git -C "$source" apply --recount --check "${patches[@]}"
    git -C "$source" apply --recount "${patches[@]}"
fi

git -C "$source" rev-parse HEAD
