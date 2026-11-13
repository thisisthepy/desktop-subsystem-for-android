#!/usr/bin/env bash
# Builds the patched SurfaceFlinger, libhwui and libEGL, and DSA's KeyMint (dsa-keymint, whose
# root key of key blobs comes from the DSA host, with the libssl it needs), in a tree
# fetch-source.py made from dsa/guest/framework/framework-lock.json.
#
#   build.sh <tree> <android16|android17> <x86_64|arm64>
#
# It applies DSA's patches (dsa/guest/framework/apply-patches.py), adds the build's product
# (device/dsa/dsa_framework, from ./device, with the board of the architecture), and builds
# exactly the install paths the image takes, with the published build's own release
# configuration. The tree holds only the projects those need, so a module elsewhere may name
# a dependency that is not there: ALLOW_MISSING_DEPENDENCIES makes that an error only if
# something built needs it. On failure it prints what the build could not find, which is
# what the lock's project list is missing.
set -euo pipefail

top="$(cd "$1" && pwd)"
android="$2"
arch="$3"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
lock="$repo/dsa/guest/framework/framework-lock.json"
product=dsa_framework
device="$top/device/dsa/$product"

case "$arch" in
  x86_64|arm64) ;;
  *) echo "the architecture must be x86_64 or arm64, not $arch" >&2; exit 1 ;;
esac
release="$(jq -r --arg a "$android" '.versions[] | select(.android == $a) | .release' "$lock")"
[[ -n "$release" && "$release" != null ]] || { echo "the lock has no $android" >&2; exit 1; }

uv run "$repo/dsa/guest/framework/apply-patches.py" --android "$android" --top "$top"

rm -rf "$device"
mkdir -p "$device"
cp "$here/device/AndroidProducts.mk" "$here/device/dsa_framework.mk" "$device/"
cp "$here/device/$arch/BoardConfig.mk" "$device/"
cp -r "$here/device/hosttools" "$device/"

# Build files the tree takes in DSA's cut (stand-ins/common for every version, then
# stand-ins/<android>): each upstream file declares modules over sources the sparse checkout
# leaves out, and Soong's analysis stops on them (prebuilt SDKs without API files, AIDL and
# gensrcs modules whose sources match nothing). The lock leaves the upstream file out (by
# not fetching it or by its exclude list), so finding one here means the lock and the
# stand-ins disagree.
for stand_ins in "$here/stand-ins/common" "$here/stand-ins/$android"; do
  [[ -d "$stand_ins" ]] || continue
  while IFS= read -r -d '' file; do
    rel="${file#"$stand_ins"/}"
    dest="$top/$rel"
    [[ -d "$(dirname "$dest")" ]] || { echo "the tree has no $(dirname "$rel") for the stand-in $rel" >&2; exit 1; }
    if [[ -e "$dest" ]] && ! cmp -s "$file" "$dest"; then
      echo "$rel came with the checkout; the lock must leave it out for its stand-in" >&2
      exit 1
    fi
    cp "$file" "$dest"
  done < <(find "$stand_ins" -type f -print0)
done

targets=()
for installed in $(uv run "$repo/dsa/guest/framework/framework_lock.py" outputs); do
  targets+=("out/target/product/$product/$installed")
done
printf '%s\n' "${targets[@]}" > "$top/.dsa-targets"
echo "building ${targets[*]} for $arch with release configuration $release"

cd "$top"
export ALLOW_MISSING_DEPENDENCIES=true
# Soong alone builds every module this needs; kati would read Android.mk files the tree
# does not have reasons to carry.
export SOONG_ONLY=true
# Android 17 runs siso by default, which the tree leaves out; ninja is in build-tools.
export SOONG_NINJA=ninja
export SKIP_ABI_CHECKS=true
export DISABLE_ARTIFACT_PATH_REQUIREMENTS=true
# Not TEMPORARY_DISABLE_PATH_RESTRICTIONS: soong_ui stops when it is set.
export BUILD_USERNAME=dsa-framework BUILD_HOSTNAME=github-actions
# An unbundled build, as the translator build learned: Soong then makes no device (its
# generated android_device and partition images stop the analysis of a partial tree), no boot
# image and no dist artifacts. Modules still install into out/target/product/<device>.
export TARGET_BUILD_UNBUNDLED=true
# The tree has no framework jars, so there are no hidden API lists to make for boot jars.
export UNSAFE_DISABLE_HIDDENAPI_FLAGS=true
if command -v ccache >/dev/null; then
  export CC_WRAPPER="$(command -v ccache)"
  # soong_ui hands ninja only the environment it knows; ccache needs its own.
  export ALLOW_NINJA_ENV=true
  # Ninja runs every action in a sandbox that can write only /tmp and out/, so the cache is
  # under /tmp ($HOME is read-only there, which failed every compile of the seventh run).
  export CCACHE_DIR="${CCACHE_DIR:-/tmp/dsa-ccache}"
  case "$CCACHE_DIR" in
    /tmp/*|"$top"/out/*) ;;
    *) echo "CCACHE_DIR must be under /tmp or out/, where the build sandbox can write" >&2; exit 1 ;;
  esac
  mkdir -p "$CCACHE_DIR"
  # A fresh checkout gives every file a new time, so the compiler is known by its version
  # rather than its time, and headers are trusted by content. The cache is restored from
  # GitHub's cache, which holds about 10 GB for the whole repository, so each of the four
  # builds keeps at most 2 GB.
  ccache -o max_size=2G -o compression=true -o hash_dir=false -o base_dir="$top" \
    -o compiler_check="string:$(jq -r '.clang | join(",")' "$top/.dsa-sources.json")" \
    -o sloppiness=include_file_mtime,include_file_ctime,time_macros,file_stat_matches
  ccache --zero-stats >/dev/null
fi

set +u
# shellcheck disable=SC1091
source build/envsetup.sh
lunch "$product-$release-userdebug"
set -u

status=0
# Soong's analysis on its own first, then every file the targets are built from that the tree
# lacks and no rule makes, listed together: ninja would stop at the first one.
m --skip-ninja "${targets[@]}" || status=$?
if (( status == 0 )); then
  uv run "$repo/.github/scripts/translator/check-ninja-inputs.py" --top "$top" \
    --ninja "$top/prebuilts/build-tools/linux-x86/bin/ninja" \
    --file "$top/out/combined-$product.ninja" --report "$top/out/missing-inputs.txt" \
    "${targets[@]}" || status=$?
fi
if (( status == 0 )); then
  m -k -j"$(nproc)" "${targets[@]}" || status=$?
fi

if command -v ccache >/dev/null; then
  ccache --show-stats || true
fi
if (( status != 0 )); then
  echo "::group::What the build could not find"
  for log in out/error.log out/soong.log; do
    [[ -f "$log" ]] || continue
    grep -hoE "(depends on undefined module|missing dependencies?:|module source path|unrecognized module type|no such file or directory|Cannot find|not found)[^\"]{0,240}" "$log" \
      | sort | uniq -c | sort -rn | head -80 || true
  done
  echo "::endgroup::"
  tail -n 120 out/error.log 2>/dev/null || true
fi
exit "$status"
