#!/usr/bin/env bash
# Reads the APKs make-test-apks.sh made with `dsa apk-info --json` and checks what it says
# against how they were made: the name, version, Android versions, permissions, icon and
# signer. Keeps each answer as <name>.json beside the APK.
#
#   .github/scripts/apk/check-apk-info.sh <dsa> <apk-dir>
set -euo pipefail

dsa="${1:?usage: check-apk-info.sh <dsa> <apk-dir>}"
dir="${2:?usage: check-apk-info.sh <dsa> <apk-dir>}"
failed=0

# No service listens here, so dsa reads the file alone and says Android is unreachable.
endpoint=(--socket "$dir/no-service.sock")

check() {
  local name="$1" filter="$2" want="$3" got
  got="$(jq -r "$filter" "$dir/$name.json")"
  if [ "$got" != "$want" ]; then
    echo "::error::$name.apk: $filter is '$got', not '$want'"
    failed=1
  else
    echo "$name.apk: $filter = $got"
  fi
}

# apksigner prints the digest in lowercase hex; dsa as colon-separated pairs.
fingerprint() {
  tr -d '\r\n' <"$dir/$1.sha256" | tr '[:lower:]' '[:upper:]' | sed 's/../&:/g; s/:$//'
}

for name in notes notes-old notes-other adaptive vector; do
  "$dsa" apk-info "$dir/$name.apk" --json "${endpoint[@]}" >"$dir/$name.json"
  "$dsa" apk-info "$dir/$name.apk" "${endpoint[@]}"
  echo
done

check notes .package com.example.notes
check notes .label Notes
check notes .version_code 42
check notes .version_name 4.2
check notes .min_sdk 26
check notes .min_android 8.0
check notes .target_sdk 34
check notes .target_android 14
check notes '[.permissions[].name] | join(",")' android.permission.CAMERA,android.permission.INTERNET
check notes '.permissions[0].sensitive' true
check notes .icon.kind image
check notes '.icon.png | .[0:8]' iVBORw0K
check notes .signer.sha256 "$(fingerprint notes)"
check notes .android unreachable
check notes .install null
check notes-old .version_code 41
check notes-old .signer.sha256 "$(fingerprint notes)"
check notes-other .version_code 43
check notes-other .signer.sha256 "$(fingerprint notes-other)"
check adaptive .label Adaptive
check adaptive .icon.kind adaptive
check adaptive .icon.background.kind color
check adaptive .icon.background.color '#FF3DDC84'
check adaptive .icon.foreground.kind image
# The foreground a vector drawable: drawn by the host, not left out.
check vector .icon.kind adaptive
check vector .icon.background.color '#FFFFFFFF'
check vector .icon.foreground.kind vector
check vector '.icon.foreground.paths | length' 2
check vector '.icon.foreground.paths[0].fill' '#FFDB4437'
# The translation is compared as numbers: jq keeps the 30.0 dsa writes for a float, so a
# joined string would test how the number is spelled rather than its value.
check vector '.icon.foreground.paths[0].transform[4:6] == [30, 30]' true

if [ "$(fingerprint notes)" = "$(fingerprint notes-other)" ]; then
  echo "::error::the two keys made the same signer"
  failed=1
fi

# A file that is not an APK is refused with a reason.
printf 'not an apk' >"$dir/broken.apk"
if "$dsa" apk-info "$dir/broken.apk" --json "${endpoint[@]}" 2>"$dir/broken.err"; then
  echo "::error::dsa apk-info took a file that is not an APK"
  failed=1
elif ! grep -q "not a ZIP archive" "$dir/broken.err"; then
  echo "::error::dsa apk-info refused a broken file without saying why: $(cat "$dir/broken.err")"
  failed=1
fi

exit "$failed"
