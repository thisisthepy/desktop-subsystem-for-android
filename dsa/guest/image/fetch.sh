#!/usr/bin/env bash
# Downloads the pinned AOSP build inputs of one Android major version and guest architecture
# (see inputs.json: target_files, otatools, dexpreopt_tools with the build's own dex2oat, and
# services.jar's ART profile from the build's own frameworks/base commit) into .scratch/aosp-inputs/<build>-<arch>/ and checks their sizes and SHA-256.
#
#   dsa/guest/image/fetch.sh 17            the x86_64 image's inputs
#   dsa/guest/image/fetch.sh 17 arm64      the arm64 (64-bit only) image's inputs
set -euo pipefail
major="${1:?usage: fetch.sh <android major> [x86_64|arm64]}"
arch="${2:-x86_64}"
case "$arch" in
  x86_64|arm64) ;;
  *) echo "the architecture must be x86_64 or arm64, not $arch" >&2; exit 1 ;;
esac
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
select='.images[] | select(.android_major == $m and ((.arch // "x86_64") == $a))'
read -r build branch target <<<"$(jq -r --argjson m "$major" --arg a "$arch" \
  "$select | \"\(.build_id) \(.branch) \(.target)\"" "$here/inputs.json")"
[[ -n "$build" ]] || { echo "inputs.json has no image for Android $major on $arch" >&2; exit 1; }
dest="${DSA_AOSP_INPUTS:-$repo/.scratch/aosp-inputs}/$build-$arch"
mkdir -p "$dest"
kinds="$(jq -r --argjson m "$major" --arg a "$arch" "$select | .files | keys[]" "$here/inputs.json")"
for kind in $kinds; do
  name="$(jq -r --argjson m "$major" --arg a "$arch" --arg k "$kind" "$select | .files[\$k].name" "$here/inputs.json")"
  sha="$(jq -r --argjson m "$major" --arg a "$arch" --arg k "$kind" "$select | .files[\$k].sha256" "$here/inputs.json")"
  file="$dest/$name"
  if [[ -f "$file" ]] && echo "$sha  $file" | sha256sum --check --status; then
    echo "have $name"
    continue
  fi
  # A file with a url is a source file at the build's own commit, which the source server
  # sends base64 encoded; the others are the build's outputs.
  url="$(jq -r --argjson m "$major" --arg a "$arch" --arg k "$kind" "$select | .files[\$k].url // empty" "$here/inputs.json")"
  encoding="$(jq -r --argjson m "$major" --arg a "$arch" --arg k "$kind" "$select | .files[\$k].encoding // empty" "$here/inputs.json")"
  echo "downloading $name of build $build ($target)"
  if [[ -n "$url" ]]; then
    curl -fsSL --retry 8 --retry-all-errors -o "$file.download" "$url"
    if [[ "$encoding" == "base64" ]]; then
      base64 -d "$file.download" > "$file.part"
      rm -f "$file.download"
    else
      mv "$file.download" "$file.part"
    fi
  else
    curl -fsSL --retry 8 --retry-all-errors -o "$file.part" "https://ci.android.com/builds/submitted/$build/$target/latest/raw/$name"
  fi
  if ! echo "$sha  $file.part" | sha256sum --check --status; then
    echo "$name does not match the SHA-256 pinned in inputs.json" >&2
    rm -f "$file.part"
    exit 1
  fi
  mv "$file.part" "$file"
done
echo "$dest"
