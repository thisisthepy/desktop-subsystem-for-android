#!/usr/bin/env bash
# Builds the APKs the APK reader and the install window are tested with, with the Android
# SDK's own tools (aapt2, zipalign, apksigner), as an app's build makes them:
#
#   notes.apk          "Notes", com.example.notes 4.2 (42), Android 8.0 to 14, asks for the
#                      camera and the network, a launcher icon in two densities, signed (v2, v3)
#   notes-old.apk      the same app at 4.1 (41), signed with the same key
#   notes-other.apk    the same app at 4.3 (43), signed with another key
#   adaptive.apk       "Adaptive", com.example.adaptive, an adaptive icon (a colour and an image)
#
# and beside each, <name>.sha256: the signer's SHA-256 digest as apksigner prints it.
#
#   .github/scripts/apk/make-test-apks.sh <out-dir>
#
# Needs ANDROID_HOME (or ANDROID_SDK_ROOT) with build-tools and a platform, and a JDK's
# keytool; GitHub's Linux and Windows runners have both.
set -euo pipefail

out="${1:?usage: make-test-apks.sh <out-dir>}"
here="$(cd "$(dirname "$0")" && pwd)"
sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:?set ANDROID_HOME}}"
sdk="$(cd "$sdk" && pwd)"
tools="$(ls -d "$sdk"/build-tools/* | sort -V | tail -1)"
# The newest released platform, not a preview.
platform="$(ls -d "$sdk"/platforms/android-* | grep -E '/android-[0-9]+$' | sort -V | tail -1)"
exe=""
bat=""
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) exe=".exe"; bat=".bat" ;;
esac
aapt2="$tools/aapt2$exe"
zipalign="$tools/zipalign$exe"
apksigner="$tools/apksigner$bat"
keytool="keytool"
if [ -n "${JAVA_HOME:-}" ]; then keytool="$JAVA_HOME/bin/keytool"; fi
echo "build tools: $tools"
echo "platform: $platform"

mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$out/work"
rm -rf "$work"
mkdir -p "$work"

key() {
  "$keytool" -genkeypair -keystore "$work/$1.jks" -storepass testpass -keypass testpass \
    -alias "$1" -keyalg RSA -keysize 2048 -validity 3650 -dname "CN=$1, O=DSA test" >/dev/null 2>&1
}

# app <name> <package> <label> <version code> <version name> <key> <res dir>
app() {
  local name="$1" package="$2" label="$3" code="$4" version="$5" signer="$6" res="$7"
  cat >"$work/$name-manifest.xml" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="$package">
    <uses-permission android:name="android.permission.CAMERA" />
    <uses-permission android:name="android.permission.INTERNET" />
    <application android:label="@string/app_name" android:icon="@mipmap/ic_launcher" />
</manifest>
EOF
  rm -rf "$work/$name-compiled"
  mkdir -p "$work/$name-compiled"
  "$aapt2" compile --dir "$res" -o "$work/$name-compiled"
  "$aapt2" link -I "$platform/android.jar" --manifest "$work/$name-manifest.xml" \
    --min-sdk-version 26 --target-sdk-version 34 --version-code "$code" --version-name "$version" \
    -o "$work/$name-unaligned.apk" "$work/$name-compiled"/*.flat
  "$zipalign" -f 4 "$work/$name-unaligned.apk" "$work/$name-aligned.apk"
  "$apksigner" sign --ks "$work/$signer.jks" --ks-pass pass:testpass --key-pass pass:testpass \
    --ks-key-alias "$signer" --out "$out/$name.apk" "$work/$name-aligned.apk"
  # The current signer's line reads "Signer #1 certificate SHA-256 digest: <hex>" or, for a
  # v3 signer limited to some Android versions, "Signer (minSdkVersion=...) certificate ...".
  "$apksigner" verify --print-certs "$out/$name.apk" | tr -d '\r' >"$work/$name-certs.txt"
  grep -m1 'certificate SHA-256 digest:' "$work/$name-certs.txt" | sed 's/.*digest: //' >"$out/$name.sha256" || true
  if [ ! -s "$out/$name.sha256" ]; then
    cat "$work/$name-certs.txt"
    echo "apksigner printed no SHA-256 digest for $name.apk" >&2
    exit 1
  fi
  echo "$name.apk: $(wc -c <"$out/$name.apk") bytes, signer $(cat "$out/$name.sha256")"
}

key notes
key other

notes_res="$work/notes-res"
mkdir -p "$notes_res/values" "$notes_res/values-de" "$notes_res/mipmap-mdpi" "$notes_res/mipmap-xxxhdpi"
cat >"$notes_res/values/strings.xml" <<'EOF'
<resources><string name="app_name">Notes</string></resources>
EOF
cat >"$notes_res/values-de/strings.xml" <<'EOF'
<resources><string name="app_name">Notizen</string></resources>
EOF
cp "$here/icon.png" "$notes_res/mipmap-mdpi/ic_launcher.png"
cp "$here/icon.png" "$notes_res/mipmap-xxxhdpi/ic_launcher.png"
app notes com.example.notes Notes 42 4.2 notes "$notes_res"
app notes-old com.example.notes Notes 41 4.1 notes "$notes_res"
app notes-other com.example.notes Notes 43 4.3 other "$notes_res"

adaptive_res="$work/adaptive-res"
mkdir -p "$adaptive_res/values" "$adaptive_res/mipmap-anydpi-v26" "$adaptive_res/mipmap-xxxhdpi"
cat >"$adaptive_res/values/strings.xml" <<'EOF'
<resources>
    <string name="app_name">Adaptive</string>
    <color name="ic_launcher_background">#FF3DDC84</color>
</resources>
EOF
cat >"$adaptive_res/mipmap-anydpi-v26/ic_launcher.xml" <<'EOF'
<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@color/ic_launcher_background" />
    <foreground android:drawable="@mipmap/ic_launcher_foreground" />
</adaptive-icon>
EOF
cp "$here/foreground.png" "$adaptive_res/mipmap-xxxhdpi/ic_launcher_foreground.png"
app adaptive com.example.adaptive Adaptive 1 1.0 notes "$adaptive_res"

rm -rf "$work"
