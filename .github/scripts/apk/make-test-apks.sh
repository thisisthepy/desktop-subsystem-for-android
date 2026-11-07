#!/usr/bin/env bash
# Builds the APKs the APK reader and the install window are tested with, with the Android
# SDK's own tools (aapt2, zipalign, apksigner), as an app's build makes them:
#
#   notes.apk          "Notes", com.example.notes 4.2 (42), Android 8.0 to 14, asks for the
#                      camera and the network, a launcher icon in two densities, signed (v2, v3)
#   notes-old.apk      the same app at 4.1 (41), signed with the same key
#   notes-other.apk    the same app at 4.3 (43), signed with another key
#   adaptive.apk       "Adaptive", com.example.adaptive, an adaptive icon (a colour and an image)
#   vector.apk         "Vector", com.example.vector, an adaptive icon (a colour and a vector
#                      drawable)
#   many.apk           "Many", com.example.many 3.20.8107, 40 permissions, native code only for
#                      armeabi-v7a, an icon with transparent corners and a colour profile
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
jar="jar"
if [ -n "${JAVA_HOME:-}" ]; then keytool="$JAVA_HOME/bin/keytool"; jar="$JAVA_HOME/bin/jar"; fi
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
$(for permission in ${permissions:-CAMERA INTERNET}; do echo "    <uses-permission android:name=\"android.permission.$permission\" />"; done)
    <application android:label="@string/app_name" android:icon="@mipmap/ic_launcher" />
</manifest>
EOF
  rm -rf "$work/$name-compiled"
  mkdir -p "$work/$name-compiled"
  # --no-crunch keeps an icon's bytes as they are, colour profile and all.
  "$aapt2" compile ${no_crunch:+--no-crunch} --dir "$res" -o "$work/$name-compiled"
  "$aapt2" link -I "$platform/android.jar" --manifest "$work/$name-manifest.xml" \
    --min-sdk-version 26 --target-sdk-version 34 --version-code "$code" --version-name "$version" \
    -o "$work/$name-unaligned.apk" "$work/$name-compiled"/*.flat
  if [ -n "${native_abi:-}" ]; then
    mkdir -p "$work/$name-native/lib/$native_abi"
    printf 'not really code' >"$work/$name-native/lib/$native_abi/libnative.so"
    (cd "$work/$name-native" && "$jar" uf "$work/$name-unaligned.apk" lib)
  fi
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

# An adaptive icon as most apps made since Android 8 have it, Chrome among them: a colour
# behind a vector drawable, with a group that moves its paths and a fill from a colour
# resource.
vector_res="$work/vector-res"
mkdir -p "$vector_res/values" "$vector_res/mipmap-anydpi-v26" "$vector_res/drawable"
cat >"$vector_res/values/strings.xml" <<'EOF'
<resources>
    <string name="app_name">Vector</string>
    <color name="ic_launcher_background">#FFFFFFFF</color>
    <color name="ring">#FFDB4437</color>
</resources>
EOF
cat >"$vector_res/mipmap-anydpi-v26/ic_launcher.xml" <<'EOF'
<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@color/ic_launcher_background" />
    <foreground android:drawable="@drawable/ic_launcher_foreground" />
</adaptive-icon>
EOF
cat >"$vector_res/drawable/ic_launcher_foreground.xml" <<'EOF'
<?xml version="1.0" encoding="utf-8"?>
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    android:width="108dp" android:height="108dp"
    android:viewportWidth="108" android:viewportHeight="108">
    <group android:translateX="30" android:translateY="30">
        <path android:fillColor="@color/ring"
            android:pathData="M24,0A24,24 0,1 1,24 48A24,24 0,1 1,24 0z" />
        <path android:fillColor="#FF4285F4"
            android:pathData="M24,14a10,10 0,1 1,-.01 0z" />
    </group>
</vector>
EOF
app vector com.example.vector Vector 1 1.0 notes "$vector_res"

many_res="$work/many-res"
mkdir -p "$many_res/values" "$many_res/mipmap-xxxhdpi"
cat >"$many_res/values/strings.xml" <<'EOF'
<resources><string name="app_name">Many</string></resources>
EOF
cp "$here/icon-corners.png" "$many_res/mipmap-xxxhdpi/ic_launcher.png"
permissions="CAMERA RECORD_AUDIO ACCESS_FINE_LOCATION ACCESS_COARSE_LOCATION READ_CONTACTS WRITE_CONTACTS
  READ_CALENDAR WRITE_CALENDAR READ_SMS SEND_SMS RECEIVE_SMS READ_CALL_LOG WRITE_CALL_LOG READ_PHONE_STATE
  CALL_PHONE READ_MEDIA_AUDIO READ_MEDIA_IMAGES READ_MEDIA_VIDEO POST_NOTIFICATIONS BLUETOOTH_CONNECT
  BLUETOOTH_SCAN BODY_SENSORS GET_ACCOUNTS INTERNET ACCESS_NETWORK_STATE ACCESS_WIFI_STATE CHANGE_WIFI_STATE
  CHANGE_NETWORK_STATE FOREGROUND_SERVICE MODIFY_AUDIO_SETTINGS QUERY_ALL_PACKAGES RECEIVE_BOOT_COMPLETED
  REQUEST_INSTALL_PACKAGES SCHEDULE_EXACT_ALARM SYSTEM_ALERT_WINDOW USE_BIOMETRIC USE_FULL_SCREEN_INTENT
  VIBRATE WAKE_LOCK WRITE_SETTINGS"
no_crunch=1 native_abi=armeabi-v7a app many com.example.many Many 8107 3.20.8107 notes "$many_res"

rm -rf "$work"
