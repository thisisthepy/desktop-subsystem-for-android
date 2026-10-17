#!/usr/bin/env bash
# Downloads the pinned AOSP build inputs of one Android major version (see inputs.json)
# into .scratch/aosp-inputs/<build>/ and checks their sizes and SHA-256.
#
#   dsa/guest/image/fetch.sh 17
set -euo pipefail
major="${1:?usage: fetch.sh <android major>}"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
read -r build branch target <<<"$(jq -r --argjson m "$major" \
  '.images[] | select(.android_major == $m) | "\(.build_id) \(.branch) \(.target)"' "$here/inputs.json")"
[[ -n "$build" ]] || { echo "inputs.json has no image for Android $major" >&2; exit 1; }
dest="${DSA_AOSP_INPUTS:-$repo/.scratch/aosp-inputs}/$build"
mkdir -p "$dest"
for kind in target_files otatools; do
  name="$(jq -r --argjson m "$major" --arg k "$kind" '.images[] | select(.android_major == $m) | .files[$k].name' "$here/inputs.json")"
  sha="$(jq -r --argjson m "$major" --arg k "$kind" '.images[] | select(.android_major == $m) | .files[$k].sha256' "$here/inputs.json")"
  file="$dest/$name"
  if [[ -f "$file" ]] && echo "$sha  $file" | sha256sum --check --status; then
    echo "have $name"
    continue
  fi
  echo "downloading $name of build $build"
  curl -fsSL --retry 3 -o "$file.part" "https://ci.android.com/builds/submitted/$build/$target/latest/raw/$name"
  if ! echo "$sha  $file.part" | sha256sum --check --status; then
    echo "$name does not match the SHA-256 pinned in inputs.json" >&2
    rm -f "$file.part"
    exit 1
  fi
  mv "$file.part" "$file"
done
echo "$dest"
