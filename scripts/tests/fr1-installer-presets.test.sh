#!/usr/bin/env bash
# Checks the installers a release builds per Android version: the default setup names the
# major of the newest image on the release's image list (never a number written by hand), one
# preset setup (a<major>) is built per supported major with an image, and the release
# publishes and lists them under their names.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
majors="$root/.github/scripts/release/installer-majors.sh"
workflow="$root/.github/workflows/release-image.yml"
action="$root/.github/actions/windows-installer/action.yml"
build="$root/dsa/host/platform/windows/installer/build.ps1"
macos="$root/dsa/host/platform/macos/packaging/build-app.sh"

fail() {
    echo "fr1 installer presets: $*" >&2
    exit 1
}

scratch="$(mktemp -d "${TMPDIR:-/tmp}/fr1-presets.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

list() {
    printf '{"format":1,"supported_majors":%s,"images":%s}\n' "$1" "$2" > "$scratch/images.json"
}

# fr1_the_default_installer_names_the_newest_image_major
list '[16,17]' '[{"version":"16.0.0-1-3","android_major":16,"build":3},{"version":"17.0.0-1-5","android_major":17,"build":5}]'
out="$(bash "$majors" "$scratch/images.json")"
grep -qx 'newest_major=17' <<<"$out" || fail "the default installer does not name the newest image's major: $out"
grep -qx 'preset_majors=17 16' <<<"$out" || fail "the presets are not one per supported major, newest first: $out"

# A list that supports a new major before its image ships still names the newest image.
list '[16,17,18]' '[{"version":"17.0.0-1-5","android_major":17,"build":5},{"version":"16.0.0-1-3","android_major":16,"build":3}]'
out="$(bash "$majors" "$scratch/images.json")"
grep -qx 'newest_major=17' <<<"$out" || fail "a major without an image was named: $out"
grep -qx 'preset_majors=17 16' <<<"$out" || fail "a preset was made for a major without an image: $out"

# When 18 ships, the installers follow without an edit here.
list '[17,18]' '[{"version":"18.0.0-1-1","android_major":18,"build":1},{"version":"17.0.0-1-5","android_major":17,"build":5}]'
out="$(bash "$majors" "$scratch/images.json")"
grep -qx 'newest_major=18' <<<"$out" || fail "18 is not named once it ships: $out"
grep -qx 'preset_majors=18 17' <<<"$out" || fail "the presets do not follow the list: $out"

list '[17]' '[]'
if bash "$majors" "$scratch/images.json" >/dev/null 2>&1; then
    fail "a list with no image was accepted"
fi

# fr1_the_release_takes_the_majors_from_the_list
grep -q 'installer-majors.sh' "$workflow" || fail "the release does not take the installer majors from the image list"
if grep -q "newest_major=\$(jq '.supported_majors | max'" "$workflow"; then
    fail "the release still names the newest supported major rather than the newest image's"
fi
grep -q 'preset-majors:' "$action" || fail "the installer action builds no presets"
grep -q 'PresetMajors' "$build" || fail "build.ps1 builds no presets"

# fr1_preset_installers_are_published_under_their_names
grep -q 'DSA-v$version-a$major-win-x64-setup.exe' "$workflow" || fail "the x64 preset installers are not published as DSA-v<version>-a<major>-win-x64-setup.exe"
grep -q 'DSA-v$version-a$major-win-arm64-setup.exe' "$workflow" || fail "the ARM64 preset installers are not published as DSA-v<version>-a<major>-win-arm64-setup.exe"
grep -q 'presetHash  $presetAsset' "$workflow" || fail "SHA256SUMS.txt does not list the preset installers"
grep -q 'Android $major fixed' "$workflow" || fail "the release notes do not explain the a<major> installers"
grep -q 'DSA_PRESET_MAJORS' "$macos" || fail "the macOS package builds no presets"

echo "fr1 installer presets: ok"
