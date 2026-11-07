#!/usr/bin/env bash
# Builds the translator's files and its host tests in a tree fetch-source.py made.
#
#   build.sh <tree> <framework libs dir>
#
# It adds the build's product (device/dsa/dsa_translator, from ./device) and the x86_64
# framework libraries the proxies link against, then builds exactly the install paths the
# package takes (assemble-digitalis.py targets) and berberis_arm64_host_tests. The tree
# holds only the projects those need, so a module elsewhere may name a dependency that is
# not there: ALLOW_MISSING_DEPENDENCIES makes that an error only if something built needs
# it. On failure it prints what the build could not find, which is what the lock's project
# list is missing.
set -euo pipefail

top="$(cd "$1" && pwd)"
framework_libs="$(cd "$2" && pwd)"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
product=dsa_translator
device="$top/device/dsa/$product"

rm -rf "$device"
mkdir -p "$device"
cp -r "$here/device/." "$device/"
mkdir -p "$device/prebuilts/x86_64"
cp "$framework_libs"/*.so "$device/prebuilts/x86_64/"

cd "$top"
uv run "$repo/dsa/guest/arm-translation/assemble-digitalis.py" targets \
  --config-mk frameworks/libs/binary_translation/berberis_config.mk \
  --product-out "out/target/product/$product" > "$top/.dsa-targets"
echo "building $(wc -l < "$top/.dsa-targets") files and berberis_arm64_host_tests"

export ALLOW_MISSING_DEPENDENCIES=true
# An unbundled build, as AOSP builds its modules on their own from a partial tree: Soong then
# makes no device (its generated android_device and partition images, which stopped the
# fifth run's analysis), no boot image and no dist artifacts. Modules still install into
# out/target/product/<device>.
export TARGET_BUILD_UNBUNDLED=true
# Soong alone builds every module this needs; kati would read Android.mk files the tree
# does not have reasons to carry.
export SOONG_ONLY=true
# Android 17 runs siso by default, which the tree leaves out; ninja is in build-tools.
export SOONG_NINJA=ninja
export SKIP_ABI_CHECKS=true
# The tree has no framework, so there are no hidden API lists to make for boot jars.
export UNSAFE_DISABLE_HIDDENAPI_FLAGS=true
export DISABLE_ARTIFACT_PATH_REQUIREMENTS=true
# Not TEMPORARY_DISABLE_PATH_RESTRICTIONS: soong_ui stops when it is set (the third run did).
export BUILD_USERNAME=dsa-translator BUILD_HOSTNAME=github-actions
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
  # GitHub's cache, which holds about 10 GB for the whole repository.
  ccache -o max_size=3G -o compression=true -o hash_dir=false -o base_dir="$top" \
    -o compiler_check="string:$(jq -r '.clang | join(",")' "$top/.dsa-sources.json")" \
    -o sloppiness=include_file_mtime,include_file_ctime,time_macros,file_stat_matches
  ccache --zero-stats >/dev/null
fi

set +u
# shellcheck disable=SC1091
source build/envsetup.sh
lunch "$product-trunk_staging-userdebug"
set -u

status=0
m -k -j"$(nproc)" $(cat "$top/.dsa-targets") berberis_arm64_host_tests || status=$?

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
  # soong_ui's own fatal errors reach soong.log and the console, not error.log.
  echo "::group::The end of soong.log"
  tail -n 40 out/soong.log 2>/dev/null || true
  echo "::endgroup::"
fi
exit "$status"
