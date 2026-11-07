#!/usr/bin/env bash
# Checks that DSA's patches for one Android version apply to the pinned source, without the
# rest of the tree: it fetches only the folders the patches change, each project at the
# commit dsa/guest/framework/framework-lock.json pins, and runs apply-patches.py --check.
#
#   check-patches.sh <android16|android17> <empty folder>
set -euo pipefail

android="$1"
top="$2"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
lock="$repo/dsa/guest/framework/framework-lock.json"
patches="$repo/dsa/guest/framework/patches"

mkdir -p "$top"
top="$(cd "$top" && pwd)"
for dir in $(cd "$patches" && find . -type d -name "$android" | sed 's|^\./||; s|/'"$android"'$||' | sort); do
  url="$(jq -r --arg a "$android" --arg p "$dir" '.versions[] | select(.android == $a) | .projects[] | select(.path == $p) | .url' "$lock")"
  commit="$(jq -r --arg a "$android" --arg p "$dir" '.versions[] | select(.android == $a) | .projects[] | select(.path == $p) | .commit' "$lock")"
  [[ -n "$url" && -n "$commit" ]] || { echo "$android does not fetch $dir, which it has patches for" >&2; exit 1; }
  # The folders the patches change, from their diff headers.
  folders="$(cat "$patches/$dir/$android"/*.patch | sed -n 's|^+++ b/||p; s|^--- a/||p' | grep -v '^/dev/null' \
    | awk -F/ '{ print $1 "/" $2 }' | sort -u)"
  mkdir -p "$top/$dir"
  git -C "$top/$dir" init -q
  git -C "$top/$dir" remote add origin "$url"
  git -C "$top/$dir" sparse-checkout set --cone $folders
  git -C "$top/$dir" fetch -q --depth=1 --filter=blob:none --no-tags origin "$commit"
  git -C "$top/$dir" checkout -q --detach FETCH_HEAD
  rm -rf "$top/$dir/.git"
  echo "$dir ${commit:0:12}: $(echo $folders)"
done
uv run "$repo/dsa/guest/framework/apply-patches.py" --android "$android" --top "$top" --check
