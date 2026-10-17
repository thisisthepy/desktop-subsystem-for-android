#!/usr/bin/env bash
# Restores the Android key set from the parts pack-keys.sh made, handed over as the
# environment variables DSA_ANDROID_KEYS_1 to DSA_ANDROID_KEYS_3.
#
#   dsa/guest/image/unpack-keys.sh <keys>
set -euo pipefail
keys="${1:?usage: unpack-keys.sh <keys>}"

if [[ -z "${DSA_ANDROID_KEYS_1:-}" ]]; then
    echo "DSA_ANDROID_KEYS_1 is not set; pack the key set with pack-keys.sh and set each part as a secret" >&2
    exit 1
fi
packed="${DSA_ANDROID_KEYS_1}${DSA_ANDROID_KEYS_2:-}${DSA_ANDROID_KEYS_3:-}"

mkdir -p "$keys"
if ! base64 -d <<<"$packed" | tar xzf - -C "$keys"; then
    echo "the key set does not unpack; check that every DSA_ANDROID_KEYS_ part is set and complete" >&2
    exit 1
fi
for file in releasekey.pk8 releasekey.x509.pem avb.pem apex.pem; do
    [[ -f "$keys/$file" ]] || { echo "the unpacked key set has no $file" >&2; exit 1; }
done
echo "keys in $keys"
