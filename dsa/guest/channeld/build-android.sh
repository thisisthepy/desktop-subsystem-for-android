#!/usr/bin/env bash
# Builds dsa-channeld for the x86_64 or arm64 guest (DSA_GUEST_ARCH, x86_64 by default) with
# the Android NDK and checks the result.
#
#   ANDROID_NDK_HOME=/path/to/ndk dsa/guest/channeld/build-android.sh <out dir>
#
# The binary links only against the C library and the dynamic loader, which every vendor
# process may use, and is stripped. It opens the NDK binder (libbinder_ndk.so, which a
# vendor process may use too) at run time.
set -euo pipefail

out="${1:?usage: build-android.sh <out dir>}"
ndk="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}"
[[ -n "$ndk" ]] || { echo "set ANDROID_NDK_HOME to the NDK" >&2; exit 1; }
here="$(cd "$(dirname "$0")" && pwd)"
api=34
case "${DSA_GUEST_ARCH:-x86_64}" in
    x86_64) target=x86_64-linux-android; machine='Advanced Micro Devices X86-64' ;;
    arm64) target=aarch64-linux-android; machine='AArch64' ;;
    *) echo "DSA_GUEST_ARCH must be x86_64 or arm64" >&2; exit 1 ;;
esac

host_tag=linux-x86_64
[[ "$(uname -s)" == Darwin ]] && host_tag=darwin-x86_64
bin="$ndk/toolchains/llvm/prebuilt/$host_tag/bin"

rustup target add "$target"
linker_var="CARGO_TARGET_$(tr 'a-z-' 'A-Z_' <<<"$target")_LINKER"
export "$linker_var=$bin/$target$api-clang"
export CARGO_PROFILE_RELEASE_STRIP=symbols
(cd "$here/../.." && cargo build --release --locked -p dsa-guest-channeld --bin dsa-channeld --target "$target")

mkdir -p "$out"
cp "$here/../../target/$target/release/dsa-channeld" "$out/dsa-channeld"

readelf="$bin/llvm-readelf"
# Read whole first: grep -q stops at the first match, and llvm-readelf exits 74 when the pipe it
# is still writing to closes.
grep -Fq "$machine" <<<"$("$readelf" -h "$out/dsa-channeld")"
grep -Fq '/system/bin/linker64' <<<"$("$readelf" -l "$out/dsa-channeld")"
needed="$("$readelf" -d "$out/dsa-channeld" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | sort | tr '\n' ' ')"
for lib in $needed; do
    case "$lib" in
        libc.so|libdl.so|libm.so) ;;
        *) echo "dsa-channeld needs $lib, which a vendor process cannot load" >&2; exit 1 ;;
    esac
done
# Android is its own target_os; code gated on Linux alone leaves only the stub that refuses
# to run, which is what init started on the first boot.
if ! grep -aq 'connecting to the host' "$out/dsa-channeld"; then
    echo "dsa-channeld was built without the daemon; is it gated on target_os = linux only?" >&2
    exit 1
fi
# The binder service the agent and the hwcomposer reach the daemon through is Android only.
if ! grep -aq 'libbinder_ndk.so' "$out/dsa-channeld"; then
    echo "dsa-channeld was built without the binder service; is it gated on target_os = android?" >&2
    exit 1
fi
echo "built $out/dsa-channeld ($(wc -c < "$out/dsa-channeld") bytes, needs: $needed)"
