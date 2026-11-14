#!/usr/bin/env bash
# Puts the macOS host's programs together as DSA.app, signs it with the virtualization
# entitlement, and wraps it in an installer package (.pkg) and a disk image (.dmg).
#
# Usage: build-app.sh <version> <programs folder> <output folder> [renderer dylib]
#
# The programs folder holds the release builds (dsa-app-window, dsa, and the service, window
# manager, graphics renderer and device lender programs when they are built). The settings app (dsa-settings)
# is taken from there too, or built from the Swift package in ../settings when it is not.
# The icons are the ones dsa/brand/generate.py renders into ./icon. DSA_SIGN_IDENTITY names
# the Developer ID to sign with; without it the bundle is signed ad hoc, which runs on the machine that
# built it and is ready to be signed for distribution. DSA_INSTALLER_IDENTITY signs the
# package the same way.
#
# The address data (the administrative boundaries the device lender turns Android's positions
# into addresses with) goes into Contents/Resources/geodata: from DSA_GEODATA_DIR, a folder
# dsa/host/core/geodata/build-geodata.py filled, or made here by that script, which downloads
# its pinned sources once into .scratch/geodata in the repository.
set -euo pipefail

version="${1:?the version, such as 0.1.0}"
programs="${2:?the folder of the built programs}"
out_arg="${3:?the output folder}"
renderer="${4:-}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../../../.." && pwd)"
mkdir -p "$out_arg"
out="$(cd "$out_arg" && pwd)"

app="$out/DSA.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources/licenses"

sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

# The bundle's executable is the settings app (SwiftUI): its window, its menu bar item and its
# Dock icon are what opening DSA.app shows. It is built here from the Swift package beside
# this folder unless the programs folder already holds it.
settings_package="$here/../settings"
if [[ ! -x "$programs/dsa-settings" ]]; then
    scratch="$root/dsa/target/dsa-settings"
    swift build -c release --package-path "$settings_package" --scratch-path "$scratch" \
        --product dsa-settings
    swift_bin="$(swift build -c release --package-path "$settings_package" --scratch-path "$scratch" --show-bin-path)"
else
    swift_bin="$programs"
fi
cp "$swift_bin/dsa-settings" "$app/Contents/MacOS/"

# The window host is required: it is the template each app's own bundle copies, and the
# window manager runs it for an app that has no bundle. It lives in a helper bundle of its own,
# with its own bundle identifier, and never in DSA.app's Contents/MacOS: a window host that ran
# as DSA.app would make macOS take DSA.app for running, and opening DSA.app would then bring
# that window forward instead of starting the settings app.
helper="$app/Contents/Helpers/DSA App Window.app"
mkdir -p "$helper/Contents/MacOS" "$helper/Contents/Resources"
sed "s/@VERSION@/$version/g" "$here/window-host-Info.plist" > "$helper/Contents/Info.plist"
plutil -lint "$helper/Contents/Info.plist"
printf 'APPL????' > "$helper/Contents/PkgInfo"
cp "$programs/dsa-app-window" "$helper/Contents/MacOS/"
# The others are taken when they were built.
for program in dsa dsa-hostd dsa-wm dsa-gfx dsa-devices; do
    if [[ -f "$programs/$program" ]]; then
        cp "$programs/$program" "$app/Contents/MacOS/"
    fi
done
if [[ -n "$renderer" ]]; then
    frameworks="$app/Contents/Frameworks"
    cp "$renderer" "$frameworks/libgfxstream_backend.dylib"
    # The renderer links the Vulkan loader from where it was built (Homebrew). The loader
    # comes along into Frameworks and the renderer is pointed at that copy, so DSA.app runs
    # on a Mac without Homebrew.
    while read -r dependency; do
        case "$dependency" in
            /usr/lib/* | /System/* | @*) continue ;;
        esac
        name="$(basename "$dependency")"
        cp "$dependency" "$frameworks/$name"
        chmod u+w "$frameworks/$name"
        install_name_tool -id "@rpath/$name" "$frameworks/$name"
        install_name_tool -change "$dependency" "@loader_path/$name" \
            "$frameworks/libgfxstream_backend.dylib"
    done < <(otool -L "$renderer" | tail -n +2 | awk '{print $1}')
    # MoltenVK is the Vulkan driver the loader opens; its description sits in Resources,
    # where the window manager points the loader (VK_ICD_FILENAMES).
    moltenvk="${DSA_MOLTENVK:-}"
    if [[ -z "$moltenvk" ]] && command -v brew >/dev/null; then
        moltenvk="$(brew --prefix molten-vk 2>/dev/null)/lib/libMoltenVK.dylib"
    fi
    if [[ -n "$moltenvk" && -f "$moltenvk" ]]; then
        cp "$moltenvk" "$frameworks/libMoltenVK.dylib"
        mkdir -p "$app/Contents/Resources/vulkan/icd.d"
        cat > "$app/Contents/Resources/vulkan/icd.d/MoltenVK_icd.json" <<'JSON'
{
    "file_format_version": "1.0.0",
    "ICD": {
        "library_path": "../../../Frameworks/libMoltenVK.dylib",
        "api_version": "1.2.0",
        "is_portability_driver": true
    }
}
JSON
    else
        echo "MoltenVK was not found (set DSA_MOLTENVK); the renderer needs it installed" >&2
    fi
fi
cp "$root/LICENSE" "$app/Contents/Resources/licenses/DSA.txt"

geodata="${DSA_GEODATA_DIR:-}"
if [[ -z "$geodata" ]]; then
    geodata="$out/geodata"
    if command -v uv >/dev/null; then
        uv run "$root/dsa/host/core/geodata/build-geodata.py" --out "$geodata/address-boundaries.bin"
    else
        python3 "$root/dsa/host/core/geodata/build-geodata.py" --out "$geodata/address-boundaries.bin"
    fi
fi
mkdir -p "$app/Contents/Resources/geodata"
for file in address-boundaries.bin NOTICE.txt; do
    if [[ ! -f "$geodata/$file" ]]; then
        echo "$geodata has no $file (build-geodata.py makes it)" >&2
        exit 1
    fi
    cp "$geodata/$file" "$app/Contents/Resources/geodata/"
done

# The app icon, one for every macOS version, at every size from 16 to 1024 px in one .icns,
# and the menu bar's template image at 1x and 2x.
cp "$here/icon/DSA.icns" "$app/Contents/Resources/DSA.icns"
cp "$here/icon/DSA.icns" "$helper/Contents/Resources/DSA.icns"
# Each companion app's own icon in the same format and sizes, none with a badge: the settings
# app (DSA.app itself) and Files are the plain symbol, the APK Installer the open box.
cp "$here/icon/Settings.icns" "$here/icon/ApkInstaller.icns" "$here/icon/Files.icns" \
    "$app/Contents/Resources/"
cp "$here/icon/MenuBarTemplate.png" "$here/icon/MenuBarTemplate@2x.png" "$app/Contents/Resources/"
# The same picture as a layered Icon Composer source, for the Liquid Glass look. It goes
# beside the package and the disk image.
rm -rf "$out/DSA.icon"
cp -R "$here/icon/DSA.icon" "$out/DSA.icon"

# The settings app's texts: its String Catalog compiled to the English and Korean string
# tables the bundle looks up in the user's language.
xcrun xcstringstool compile "$settings_package/Resources/Localizable.xcstrings" \
    --output-directory "$app/Contents/Resources"
for language in en ko; do
    table="$app/Contents/Resources/$language.lproj/Localizable.strings"
    if [[ ! -s "$table" ]]; then
        echo "the String Catalog gave no $language table ($table)" >&2
        exit 1
    fi
    plutil -lint "$table"
done

identity="${DSA_SIGN_IDENTITY:--}"
sign() {
    codesign --force --timestamp=none --options runtime \
        --entitlements "$here/dsa.entitlements" --sign "$identity" "$@"
}
# The Mac's passkeys for Android apps need dsa-devices signed with Apple's browser passkey
# entitlement, which only a provisioning profile that grants it allows. DSA_PASSKEY_PROFILE
# names that profile; it is embedded in the bundle, and its identifiers are merged into
# dsa-devices' entitlements. Without it dsa-devices is signed like the other programs, and
# Android is told the Mac's passkeys are unavailable.
devices_entitlements="$here/dsa.entitlements"
if [[ -n "${DSA_PASSKEY_PROFILE:-}" ]]; then
    cp "$DSA_PASSKEY_PROFILE" "$app/Contents/embedded.provisionprofile"
    devices_entitlements="$out/dsa-devices.entitlements"
    cp "$here/dsa-devices-passkeys.entitlements" "$devices_entitlements"
    granted="$out/profile-entitlements.plist"
    security cms -D -i "$DSA_PASSKEY_PROFILE" > "$out/profile.plist"
    /usr/libexec/PlistBuddy -x -c "Print :Entitlements" "$out/profile.plist" > "$granted"
    for key in com.apple.application-identifier com.apple.developer.team-identifier; do
        if value="$(/usr/libexec/PlistBuddy -c "Print :$key" "$granted" 2>/dev/null)"; then
            /usr/libexec/PlistBuddy -c "Add :$key string $value" "$devices_entitlements"
        fi
    done
    rm -f "$out/profile.plist" "$granted"
fi
# Inside out: libraries and helper programs first, the bundle last.
for library in "$app/Contents/Frameworks"/*.dylib; do
    [[ -e "$library" ]] || continue
    codesign --force --timestamp=none --sign "$identity" "$library"
done
for program in "$app/Contents/MacOS"/*; do
    case "$(basename "$program")" in
        dsa-settings) continue ;;
        dsa-devices)
            codesign --force --timestamp=none --options runtime \
                --entitlements "$devices_entitlements" --sign "$identity" "$program"
            ;;
        *) sign "$program" ;;
    esac
done
if [[ "$devices_entitlements" == "$out/"* ]]; then
    rm -f "$devices_entitlements"
fi
sign "$helper"
sign "$app"
codesign --verify --strict --verbose=2 "$app"

package="$out/DSA-$version.pkg"
installer_identity=()
if [[ -n "${DSA_INSTALLER_IDENTITY:-}" ]]; then
    installer_identity=(--sign "$DSA_INSTALLER_IDENTITY")
fi
pkgbuild --component "$app" --install-location /Applications \
    --scripts "$here/scripts" \
    --identifier org.thisisthepy.dsa --version "$version" \
    ${installer_identity[@]+"${installer_identity[@]}"} "$package"

image="$out/DSA-$version.dmg"
staging="$out/dmg"
rm -rf "$staging" "$image"
mkdir -p "$staging"
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -volname "DSA $version" -srcfolder "$staging" -fs HFS+ -format UDZO "$image"
rm -rf "$staging"

echo "Built $app"
echo "Built $package"
echo "Built $image"
