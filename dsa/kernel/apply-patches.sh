#!/usr/bin/env bash
# Leaves the kernel checkout at <src> as its HEAD (the pinned tag) with patches/*.patch
# applied in name order, however often it runs:
#
#   dsa/kernel/apply-patches.sh [--reset] <src>
#
# DSA_KERNEL_PATCHES names another folder of patches (the tests use it).
#
# The patches stack: several change the same file (the page_reporting ones all change
# mm/page_reporting.c), so a later patch rewrites lines an earlier one left, and whether one
# patch is in the tree cannot be told from that patch alone. So the series is applied once,
# to HEAD, in a separate index inside the checkout's .git, and each file the series touches is
# compared with what that gives:
#   - a file that already is what the series makes is left alone, so a second run (CI
#     configures, then builds) changes nothing and rebuilds nothing;
#   - a file still as HEAD has it is written as the series makes it;
#   - a file that is neither (patched by an older series, or edited) is rewritten only with
#     --reset, which build.sh passes for the checkout it fetched itself. Without it the run
#     stops and names the file, so nobody's edits are overwritten.
# With --reset, changes outside the series' files (left by a patch that has since been
# removed) are undone as well.
set -euo pipefail

reset=false
if [[ "${1:-}" == --reset ]]; then
    reset=true
    shift
fi
src="${1:?usage: apply-patches.sh [--reset] <kernel source>}"
patch_dir="${DSA_KERNEL_PATCHES:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/patches}"

git_dir="$(git -C "$src" rev-parse --absolute-git-dir)"
series_index="$git_dir/dsa-series-index"
trap 'rm -f "$series_index"' EXIT
rm -f "$series_index"

# The series as it applies to HEAD. Each git apply --cached writes the index before the next
# one reads it, so stacked patches see the lines the patches before them left.
GIT_INDEX_FILE="$series_index" git -C "$src" read-tree HEAD
shopt -s nullglob
for patch in "$patch_dir"/*.patch; do
    if ! GIT_INDEX_FILE="$series_index" git -C "$src" apply --cached "$patch"; then
        echo "$(basename "$patch") does not apply to $(git -C "$src" describe --tags --always HEAD) after the patches before it" >&2
        exit 1
    fi
done
mapfile -t touched < <(GIT_INDEX_FILE="$series_index" git -C "$src" diff --cached --name-only --no-renames HEAD)

# The blob a path has in HEAD or in the series, or nothing when it has none there.
blob_in_head() { git -C "$src" rev-parse -q --verify "HEAD:$1" 2>/dev/null || true; }
blob_in_series() { GIT_INDEX_FILE="$series_index" git -C "$src" ls-files -s -- "$1" | cut -d' ' -f2; }
blob_in_tree() { [[ -f "$src/$1" ]] && git -C "$src" hash-object -- "$1" || true; }

write=()
remove=()
for path in "${touched[@]}"; do
    want="$(blob_in_series "$path")"
    have="$(blob_in_tree "$path")"
    if [[ "$have" == "$want" ]]; then
        continue
    fi
    if [[ "$have" != "$(blob_in_head "$path")" ]] && ! $reset; then
        echo "$src/$path has changes the patches do not make; restore it to $(git -C "$src" describe --tags --always HEAD) or rerun with --reset" >&2
        exit 1
    fi
    if [[ -n "$want" ]]; then write+=("$path"); else remove+=("$path"); fi
done

if $reset; then
    # Whatever differs from HEAD outside the series' files is left from an older series.
    declare -A in_series=()
    for path in "${touched[@]}"; do in_series["$path"]=1; done
    while IFS= read -r path; do
        if [[ -n "$path" && -z "${in_series[$path]:-}" ]]; then
            git -C "$src" checkout -q HEAD -- "$path"
        fi
    done < <(git -C "$src" diff --name-only --no-renames HEAD)
    while IFS= read -r path; do
        if [[ -n "$path" && -z "${in_series[$path]:-}" ]]; then
            rm -f "$src/$path"
        fi
    done < <(git -C "$src" ls-files --others --exclude-standard)
fi

if (( ${#write[@]} > 0 )); then
    GIT_INDEX_FILE="$series_index" git -C "$src" checkout-index -f -- "${write[@]}"
fi
for path in "${remove[@]}"; do
    rm -f "$src/$path"
done
echo "patches: ${#touched[@]} files in the series, $(( ${#write[@]} + ${#remove[@]} )) written" >&2
