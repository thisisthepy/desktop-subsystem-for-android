#!/usr/bin/env bash
# Builds the Linux host's VM programs from the pinned upstream revisions in versions.env:
# crosvm, the VMM, and passt, the user-mode NAT. Neither is kept in this repository: each is
# fetched into .scratch/ and built there. crosvm is built with the patches in patches/
# applied: virtio-mem.patch adds the virtio-mem device the VM grows and shrinks with.
#
#   build.sh --out <folder> --gfxstream <folder with libgfxstream_backend.so>
#
# The folder gets:
#   crosvm                  linked with gfxstream's host renderer, which it finds beside itself
#   passt                   with vhost-user, which crosvm's network adapter connects to
#   passt-source.tar.gz     passt's complete source, shipped beside it (it is GPL)
#   crosvm-crates.txt       every crate and library crosvm links, with its licence
# check-licenses.py fails the build when anything crosvm links is GPL, LGPL or AGPL.
#
# Build dependencies (Debian and Ubuntu names): git, a Rust toolchain through rustup, clang,
# libclang-dev, pkg-config, libcap-dev, libwayland-dev, wayland-protocols, libdrm-dev,
# protobuf-compiler, make, gcc, and uv.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../../../.." && pwd)"
# shellcheck source=versions.env
source "$here/versions.env"

out=""
gfxstream=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --gfxstream) gfxstream="$2"; shift 2 ;;
        *) echo "build.sh: unknown argument $1" >&2; exit 1 ;;
    esac
done
[[ -n "$out" ]] || { echo "build.sh: name the output folder with --out" >&2; exit 1; }
[[ -f "$gfxstream/libgfxstream_backend.so" ]] || {
    echo "build.sh: --gfxstream names no folder with libgfxstream_backend.so" >&2
    exit 1
}
mkdir -p "$out"
out="$(cd "$out" && pwd)"
gfxstream="$(cd "$gfxstream" && pwd)"

# Checks out `revision` of `remote` in `dir`, fetching only that commit.
checkout() {
    local remote="$1" revision="$2" dir="$3"
    if [[ ! -d "$dir/.git" ]]; then
        mkdir -p "$dir"
        git -C "$dir" init -q
        git -C "$dir" remote add origin "$remote"
    fi
    git -C "$dir" fetch -q --depth 1 origin "$revision"
    git -C "$dir" checkout -q --detach FETCH_HEAD
    git -C "$dir" reset -q --hard FETCH_HEAD
}

# crosvm, with the one submodule it links: minijail, the sandbox.
crosvm_src="$root/.scratch/crosvm/src"
checkout "$CROSVM_REMOTE" "$CROSVM_REVISION" "$crosvm_src"
git -C "$crosvm_src" submodule update --init third_party/minijail
echo "crosvm $(git -C "$crosvm_src" rev-parse HEAD)"

# The patches DSA keeps on top of the pinned revision, in name order (see patches/). Files an
# earlier run's patches added are removed first; build output, which crosvm's .gitignore
# names, stays.
git -C "$crosvm_src" clean -fdq
for patch in "$here"/patches/*.patch; do
    echo "applying $(basename "$patch")"
    git -C "$crosvm_src" apply --whitespace=nowarn "$patch"
done
(
    cd "$crosvm_src"
    # gfxstream's renderer is linked from the given folder and found beside crosvm at run time.
    GFXSTREAM_PATH="$gfxstream" \
    RUSTFLAGS="-C link-arg=-Wl,-rpath,\$ORIGIN" \
        cargo build --release --locked --no-default-features --features "$CROSVM_FEATURES"
)
install -m 0755 "$crosvm_src/target/release/crosvm" "$out/crosvm"

# The licences of everything crosvm links, and the check that none of it is copyleft.
uv run "$here/check-licenses.py" \
    --source "$crosvm_src" --features "$CROSVM_FEATURES" \
    --binary "$out/crosvm" --out "$out/crosvm-crates.txt"

# passt, with its source beside it.
passt_src="$root/.scratch/passt/src"
checkout "$PASST_REMOTE" "refs/tags/$PASST_TAG" "$passt_src"
make -C "$passt_src" clean >/dev/null
make -C "$passt_src" passt VERSION="$PASST_TAG"
install -m 0755 "$passt_src/passt" "$out/passt"
git -C "$passt_src" archive --format=tar.gz --prefix="passt-$PASST_TAG/" -o "$out/passt-source.tar.gz" HEAD

"$out/crosvm" version || true
"$out/passt" --version | head -1
ls -l "$out"
