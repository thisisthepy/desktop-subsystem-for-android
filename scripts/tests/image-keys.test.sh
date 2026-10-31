#!/usr/bin/env bash
# The Android key set travels to the Image release workflow as repository secrets. A
# secret holds at most 48 KB and the key set is larger, so pack-keys.sh splits it and
# unpack-keys.sh puts it back. This checks the round trip on a freshly made key set.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"
image=dsa/guest/image

work="$repo_root/.scratch/image-keys-test"
rm -rf "$work"
mkdir -p "$work"
trap 'rm -rf "$work"' EXIT

fail() { echo "error: $*" >&2; exit 1; }

"$image/make-keys.sh" "$work/keys" >/dev/null
"$image/pack-keys.sh" "$work/keys" "$work/parts" >/dev/null

parts=("$work"/parts/DSA_ANDROID_KEYS_*)
(( ${#parts[@]} >= 1 && ${#parts[@]} <= 3 )) || fail "expected 1 to 3 parts, got ${#parts[@]}"
for part in "${parts[@]}"; do
    size="$(wc -c < "$part")"
    (( size <= 48 * 1024 )) || fail "$(basename "$part") is $size bytes, over the 48 KB a secret holds"
done

# The workflow hands the parts over as environment variables of the same names.
for part in "${parts[@]}"; do
    export "$(basename "$part")=$(cat "$part")"
done
"$image/unpack-keys.sh" "$work/restored" >/dev/null

for name in releasekey platform shared media networkstack sdk_sandbox bluetooth nfc apexcontainer; do
    for ext in pk8 x509.pem; do
        cmp -s "$work/keys/$name.$ext" "$work/restored/$name.$ext" || fail "$name.$ext did not survive the round trip"
    done
done
for name in avb apex; do
    cmp -s "$work/keys/$name.pem" "$work/restored/$name.pem" || fail "$name.pem did not survive the round trip"
done

# A missing part is reported, not turned into a broken key set.
unset DSA_ANDROID_KEYS_1
if "$image/unpack-keys.sh" "$work/missing" >/dev/null 2>&1; then
    fail "unpack-keys.sh accepted a key set with its first part missing"
fi
echo "ok    the Android key set survives the trip through repository secrets"
