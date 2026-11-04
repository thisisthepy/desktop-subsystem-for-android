#!/usr/bin/env bash
# Builds the two graphics test apps from Android's NDK samples (Apache-2.0): hello-gl2, an
# OpenGL ES app, and hello-vulkan, a Vulkan app. The samples are fetched at a pinned commit
# into .scratch, not kept in this repository. The "Graphics test" workflow runs this and keeps
# the APKs as the graphics-test-apps artifact, which verify.py downloads.
#
#   dsa/graphics/verify/build-test-apps.sh [<out dir>]
#
# Needs a JDK 21 and an Android SDK with CMake 4.1.0 (ANDROID_HOME).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source="$root/.scratch/ndk-samples"
out="${1:-$root/.scratch/graphics-verify/apps}"
revision="abb0d8ef36a5ee1be5cebc67025678f42970c393"
remote="https://github.com/android/ndk-samples.git"

if [[ ! -d "$source/.git" ]]; then
    git init -q "$source"
    git -C "$source" remote add origin "$remote"
fi
git -C "$source" fetch -q --depth 1 origin "$revision"
git -C "$source" checkout -q --detach FETCH_HEAD
git -C "$source" clean -fdxq

(cd "$source" && ./gradlew --no-daemon :hello-gl2:app:assembleDebug :hello-vulkan:app:assembleDebug)

mkdir -p "$out"
cp "$(ls "$source"/hello-gl2/app/build/outputs/apk/debug/*.apk | head -n1)" "$out/hello-gl2.apk"
cp "$(ls "$source"/hello-vulkan/app/build/outputs/apk/debug/*.apk | head -n1)" "$out/hello-vulkan.apk"
ls -l "$out"
