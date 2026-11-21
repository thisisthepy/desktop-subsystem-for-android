#!/usr/bin/env bash
# Puts the Vulkan loader into DSA.app's Frameworks folder for the graphics renderer.
#
# Usage: bundle-vulkan-loader.sh <loader dylib> <Frameworks folder>
#
# The renderer opens the loader at run time by the bare name `libvulkan.dylib`, so linking
# never lists it and copying what the renderer links leaves it out. The copy is named
# `libvulkan.dylib` and takes that as its install name: the app opens it by its full path
# before the renderer starts, and the renderer's later open of the bare name finds the
# library that is already loaded. Changing the install name breaks the signature, so the
# copy is signed again (ad hoc here, and by build-app.sh with the real identity later).
set -euo pipefail

loader="${1:?the Vulkan loader library}"
frameworks="${2:?the Frameworks folder}"

[[ -f "$loader" ]] || { echo "the Vulkan loader $loader does not exist" >&2; exit 1; }
mkdir -p "$frameworks"
# A symbolic link is followed so that the bundle holds the library, not a link out of it.
cp -L "$loader" "$frameworks/libvulkan.dylib"
chmod u+w "$frameworks/libvulkan.dylib"
install_name_tool -id "libvulkan.dylib" "$frameworks/libvulkan.dylib" 2>/dev/null
codesign --force --sign - "$frameworks/libvulkan.dylib" 2>/dev/null
