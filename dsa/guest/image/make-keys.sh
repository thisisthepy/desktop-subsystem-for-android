#!/usr/bin/env bash
# Makes a full set of image signing keys in <dir>: the APK keys Android signs its apps
# with (releasekey, platform, shared, media, networkstack, sdk_sandbox, bluetooth, nfc,
# apexcontainer), the APEX payload key and the verified boot (AVB) key.
#
# CI makes a throwaway set for each test run. A release uses the project's own set, kept
# as a repository secret and never in the repository.
#
#   dsa/guest/image/make-keys.sh .scratch/image-keys
set -euo pipefail
dir="${1:?usage: make-keys.sh <dir>}"
subject="${DSA_KEY_SUBJECT:-/C=KR/O=thisisthepy/CN=Desktop Subsystem for Android}"
mkdir -p "$dir"
for name in releasekey platform shared media networkstack sdk_sandbox bluetooth nfc apexcontainer; do
  [[ -f "$dir/$name.pk8" ]] && continue
  openssl genrsa -out "$dir/$name.pem" 4096 2>/dev/null
  openssl req -new -x509 -sha256 -key "$dir/$name.pem" -out "$dir/$name.x509.pem" -days 10000 -subj "$subject"
  openssl pkcs8 -in "$dir/$name.pem" -topk8 -outform DER -out "$dir/$name.pk8" -nocrypt
done
for name in avb apex; do
  [[ -f "$dir/$name.pem" ]] || openssl genrsa -out "$dir/$name.pem" 4096 2>/dev/null
done
echo "keys in $dir"
