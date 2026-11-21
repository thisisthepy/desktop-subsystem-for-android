#!/usr/bin/env bash
# Usage: ./.github/scripts/release/installer-majors.sh <image list>
#
# Prints, as GITHUB_OUTPUT lines, the Android majors the installers of a release name, read
# from the image list the release's installers use (images.json):
#
#   newest_major=<n>      the major of the newest image on the list, which the default
#                         setup's first screen names and the default installation runs;
#   preset_majors=<a b>   every supported major that has an image on the list, newest first:
#                         one preset installer (a<n>) is built for each.
#
# Both come from the list, never from a number written here, so they stay right when a new
# major ships.

set -euo pipefail

die() {
    echo "installer-majors: $*" >&2
    exit 1
}

[[ $# -eq 1 ]] || die "usage: installer-majors.sh <image list>"
list="$1"
[[ -f "$list" ]] || die "$list does not exist"

newest="$(jq -r '[.images[].android_major] | max // empty' "$list")"
[[ -n "$newest" ]] || die "$list names no image, so there is no Android for an installer to name"
presets="$(jq -r '.supported_majors as $s
  | [.images[].android_major | select(. as $m | $s | index($m))] | unique | reverse | map(tostring) | join(" ")' "$list")"
[[ -n "$presets" ]] || die "$list has no image of a supported major"

echo "newest_major=$newest"
echo "preset_majors=$presets"
