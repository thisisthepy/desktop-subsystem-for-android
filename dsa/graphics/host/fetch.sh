#!/usr/bin/env bash
# Fetches the pinned gfxstream source and the host build dependencies on Linux, as fetch.ps1
# does on Windows: gfxstream with the host patches, aemu with its patches, and ANGLE's
# headers. The trees go under the repository's ignored .scratch directory.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
scratch="${1:-$root/.scratch}"
mkdir -p "$scratch"
scratch="$(cd "$scratch" && pwd)"

gfxstream_dir="$scratch/gfxstream"
aemu_dir="$scratch/aemu"
angle_dir="$scratch/angle"
patch_dir="$root/dsa/graphics/patches"

# The same revisions fetch.ps1 pins; the two move together.
gfxstream_revision="d047a57228332d995d36600792fa9ccc26cf8ae6"
aemu_revision="3ff20aed307f55bc4ae865373d07bf76c9efa43c"

checkout() {
    local remote="$1" dir="$2" revision="$3"
    if [[ ! -d "$dir/.git" ]]; then
        git clone "$remote" "$dir"
    fi
    git -C "$dir" fetch --no-tags origin "$revision"
    git -C "$dir" checkout --detach "$revision"
    git -C "$dir" reset --hard "$revision"
    git -C "$dir" clean -fdx
}

checkout https://android.googlesource.com/platform/hardware/google/gfxstream \
    "$gfxstream_dir" "$gfxstream_revision"
# The host patches, as fetch.ps1 lists them: 0002, 0003 and 0008 patch Mesa for the guest.
for number in 0001 0004 0005 0006 0007 0009 0010 0011 0012 0013 0014 0015; do
    for patch in "$patch_dir"/"$number"-*.patch; do
        echo "Applying patch: $(basename "$patch")"
        if ! git -C "$gfxstream_dir" apply --recount "$patch"; then
            echo "patch $(basename "$patch") does not apply to gfxstream $gfxstream_revision" >&2
            exit 1
        fi
    done
done

checkout https://android.googlesource.com/platform/hardware/google/aemu \
    "$aemu_dir" "$aemu_revision"
for patch in "$patch_dir"/aemu/*.patch; do
    [[ -e "$patch" ]] || continue
    echo "Applying aemu patch: $(basename "$patch")"
    git -C "$aemu_dir" apply --recount "$patch"
done

if [[ ! -d "$angle_dir/.git" ]]; then
    git clone --depth=1 --filter=blob:none --sparse \
        https://android.googlesource.com/platform/external/angle "$angle_dir"
fi
git -C "$angle_dir" sparse-checkout set include

echo "All host dependencies fetched in $scratch."
