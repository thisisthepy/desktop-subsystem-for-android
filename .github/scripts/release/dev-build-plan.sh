#!/usr/bin/env bash
# Usage:
#   .github/scripts/release/dev-build-plan.sh needs <image|installer>
#       reads changed paths (one per line) on standard input and prints true when any of them
#       changes what that part of a Development build is made from, false otherwise.
#   .github/scripts/release/dev-build-plan.sh plan <auto|all|image|installer> <commit>
#       decides what the Development build of <commit> makes, and prints it as lines for
#       $GITHUB_OUTPUT: image=true|false, installer=true|false, and for each part the run
#       whose artifacts it compared against (image_base=, installer_base=, empty for none).
#
# "auto" makes a part when a file it is made from changed since the newest successful
# Development build that made it and still has its artifact; a part no such run has is made.
# So a change to the host alone makes the installer only (about ten minutes), a change to the
# guest alone the image only, and the install script keeps the other part as it is installed.
# Documents and tests change neither. "all", "image" and "installer" choose by hand.
#
# plan reads the runs with gh (GH_TOKEN, and GH_REPO or the current repository) and the
# history with git, so the checkout needs the full history (fetch-depth: 0).

set -euo pipefail

die() {
    echo "dev-build-plan: $*" >&2
    exit 1
}

# What each part is made from. The image: the guest's programs and image scripts, the guest
# kernel, the guest's graphics transport and the protocol its programs speak. The installer:
# the host programs, the settings app and setup, the protocol, and the files the host build
# reads beside them. Both: the workflows and scripts that build them.
image_paths='^(dsa/guest/|dsa/kernel/|dsa/graphics/|dsa/protocol/|dsa/android-versions\.json$|dsa/Cargo\.(toml|lock)$|\.github/actions/guest-kernel/)'
installer_paths='^(dsa/host/|dsa/protocol/|dsa/brand/|dsa/android-versions\.json$|dsa/Cargo\.(toml|lock)$|dsa/about\.toml$|dsa/third-party\.toml$|dsa/guest/image/inputs\.json$|\.github/actions/windows-installer/)'
both_paths='^(\.github/workflows/(release-image|dev-build)\.yml$|\.github/scripts/release/dev-build-plan\.sh$)'
# Documents and tests: they change nothing that is built.
ignored='(\.md$|/tests?/|/test_[^/]*$|_test\.[a-z]+$)'

needs() {
    local part="$1" pattern
    case "$part" in
        image) pattern="$image_paths" ;;
        installer) pattern="$installer_paths" ;;
        *) die "needs takes image or installer, not $part" ;;
    esac
    local path
    while IFS= read -r path; do
        path="${path%$'\r'}"
        [[ -z "$path" ]] && continue
        if [[ "$path" =~ $both_paths ]]; then
            echo true
            return
        fi
        [[ "$path" =~ $ignored ]] && continue
        if [[ "$path" =~ $pattern ]]; then
            echo true
            return
        fi
    done
    echo false
}

# The commit of the newest successful Development build on develop that still has the
# artifact <name>, and its run, as "<run> <sha>"; nothing when there is none.
last_with() {
    local name="$1" run sha has
    while read -r run sha; do
        [[ -z "$run" ]] && continue
        has="$(gh api "repos/{owner}/{repo}/actions/runs/$run/artifacts?per_page=100" \
            --jq "[.artifacts[] | select(.name == \"$name\" and (.expired | not))] | length")"
        if [[ "$has" != 0 ]]; then
            echo "$run $sha"
            return
        fi
    done < <(gh run list --workflow dev-build.yml --branch develop --status success --limit 30 \
        --json databaseId,headSha --jq '.[] | "\(.databaseId) \(.headSha)"')
}

# Whether <part> changed between the commit <base> and <head>; true when <base> is not in
# this history (the history was rewritten), so the part is made rather than missed.
changed_since() {
    local part="$1" base="$2" head="$3"
    if ! git cat-file -e "$base^{commit}" 2>/dev/null; then
        echo true
        return
    fi
    needs "$part" < <(git diff --name-only "$base" "$head")
}

plan() {
    local requested="$1" head="$2"
    local image=false installer=false image_base="" installer_base="" found
    case "$requested" in
        all) image=true installer=true ;;
        image) image=true ;;
        installer) installer=true ;;
        auto)
            found="$(last_with development-image)"
            if [[ -z "$found" ]]; then
                image=true
            else
                image_base="${found%% *}"
                image="$(changed_since image "${found#* }" "$head")"
            fi
            found="$(last_with development-installer)"
            if [[ -z "$found" ]]; then
                installer=true
            else
                installer_base="${found%% *}"
                installer="$(changed_since installer "${found#* }" "$head")"
            fi
            ;;
        *) die "plan takes auto, all, image or installer, not $requested" ;;
    esac
    echo "image=$image"
    echo "installer=$installer"
    echo "image_base=$image_base"
    echo "installer_base=$installer_base"
}

case "${1:-}" in
    needs)
        [[ $# -eq 2 ]] || die "usage: dev-build-plan.sh needs <image|installer>"
        needs "$2"
        ;;
    plan)
        [[ $# -eq 3 ]] || die "usage: dev-build-plan.sh plan <auto|all|image|installer> <commit>"
        plan "$2" "$3"
        ;;
    *) die "usage: dev-build-plan.sh needs <image|installer> | plan <auto|all|image|installer> <commit>" ;;
esac
