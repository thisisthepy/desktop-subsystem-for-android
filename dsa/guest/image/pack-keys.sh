#!/usr/bin/env bash
# Packs the Android key set for the Image release workflow's repository secrets.
#
#   dsa/guest/image/pack-keys.sh <keys> <out>
#
# A secret holds at most 48 KB and the key set is larger, so the packed set is split into
# <out>/DSA_ANDROID_KEYS_1, _2 and so on, one file per secret. Only the files signing
# needs go in: each APK key's .pk8 and .x509.pem, and the AVB and APEX payload keys.
# make-keys.sh also leaves a .pem copy of each APK key, which nothing reads.
set -euo pipefail
keys="${1:?usage: pack-keys.sh <keys> <out>}"
out="${2:?usage: pack-keys.sh <keys> <out>}"
names=(releasekey platform shared media networkstack sdk_sandbox bluetooth nfc apexcontainer)

files=()
for name in "${names[@]}"; do
    files+=("$name.pk8" "$name.x509.pem")
done
files+=(avb.pem apex.pem)
for file in "${files[@]}"; do
    [[ -f "$keys/$file" ]] || { echo "$keys has no $file; make the set with make-keys.sh" >&2; exit 1; }
done

rm -rf "$out"
mkdir -p "$out"
# 40000 characters per part leaves room under the 48 KB limit.
tar czf - -C "$keys" "${files[@]}" | base64 | tr -d '\n' | split -b 40000 -d -a 1 - "$out/part."
count=0
for part in "$out"/part.*; do
    count=$((count + 1))
    mv "$part" "$out/DSA_ANDROID_KEYS_$count"
done
if (( count > 3 )); then
    echo "the packed key set needs $count secrets; the workflow reads at most 3" >&2
    exit 1
fi

echo "Set each part as a repository secret of the same name:"
for part in "$out"/DSA_ANDROID_KEYS_*; do
    echo "  gh secret set $(basename "$part") --app actions < $part"
done
