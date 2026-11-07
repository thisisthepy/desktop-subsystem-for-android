#!/usr/bin/env bash
# Builds Digitalis, the open source (Apache-2.0) ARM64 to x86_64 translator built on AOSP's
# Berberis, at a pinned revision, and leaves its binary bundle where make-package.py takes it.
#
#   dsa/guest/arm-translation/build-digitalis.sh [<work dir>]
#
# The work dir defaults to .scratch/digitalis in this repository. Nothing is vendored: the
# Digitalis manifest pulls an AOSP tree of its own (several hundred GB once built), and the
# build runs in Digitalis' own Docker container. It needs repo, git, jq, Docker, and a machine
# with the disk, memory and hours a full AOSP build takes. CI does not use it: the
# "Translator build" workflow builds the same pins in a reduced tree on a hosted runner
# (.github/scripts/translator/). Then:
#
#   uv run dsa/guest/arm-translation/make-package.py \
#       --bundle .scratch/digitalis/digitalis/dist/digitalis-prebuilts \
#       --name Digitalis --version <revision> --out .scratch/arm-translation-package
#   dsa arm-translation install .scratch/arm-translation-package
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
WORK="${1:-$REPO/.scratch/digitalis}"

for tool in repo git docker jq; do
    command -v "$tool" >/dev/null || { echo "build-digitalis.sh needs $tool" >&2; exit 1; }
done

# The translator and its ARM64 guest libraries come from one of Digitalis' version branches
# (android17 unless DSA_DIGITALIS_ANDROID says android16), at the commits
# digitalis-lock.json pins, the same pins the "Translator build" workflow builds from. Each
# is a commit, so a rebuild is the same build.
ANDROID_BRANCH="${DSA_DIGITALIS_ANDROID:-android17}"
LOCK="$HERE/digitalis-lock.json"
pin() { jq -r --arg a "$ANDROID_BRANCH" --arg p "$1" ".versions[] | select(.android == \$a) | $2" "$LOCK"; }
MANIFEST_URL="https://github.com/DigitalisX64/manifest.git"
MANIFEST_REVISION="$(pin "" .manifest.commit)"
DIGITALIS_REVISION="$(pin digitalis '.projects[] | select(.path == $p) | .commit')"
TRANSLATOR_REVISION="$(pin frameworks/libs/binary_translation '.projects[] | select(.path == $p) | .commit')"
[[ -n "$MANIFEST_REVISION" && "$MANIFEST_REVISION" != null ]] || { echo "digitalis-lock.json has no $ANDROID_BRANCH" >&2; exit 1; }

mkdir -p "$WORK"
cd "$WORK"
if [ ! -d .repo ]; then
    repo init -u "$MANIFEST_URL" -b "$MANIFEST_REVISION" --depth=1
fi
repo sync -c -d --no-tags --force-sync -j"$(nproc)"

# The manifest names Digitalis' own repositories by branch; they are moved to the pinned commits.
git -C digitalis fetch --depth=1 origin "$DIGITALIS_REVISION"
git -C digitalis checkout --detach "$DIGITALIS_REVISION"
git -C frameworks/libs/binary_translation fetch --depth=1 origin "$TRANSLATOR_REVISION"
git -C frameworks/libs/binary_translation checkout --detach "$TRANSLATOR_REVISION"

# A full build the first time: the bundle takes the guest libraries from the assembled image.
if [ -d out/target/product ]; then
    digitalis/docker/build-digitalis.sh
else
    digitalis/docker/build-digitalis.sh --full
fi
digitalis/scripts/verify-digitalis-prebuilts.sh

echo "the bundle is in $WORK/digitalis/dist/digitalis-prebuilts"
