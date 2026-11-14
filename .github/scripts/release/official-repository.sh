#!/usr/bin/env bash
# Usage: ./.github/scripts/release/official-repository.sh [--require]
#
# Prints the official repository, the only one that publishes DSA: its releases, its signed
# image and program lists, its Pages site and its release branch. An installed DSA reads its
# updates from there and nowhere else.
#
# With --require it fails unless this run is in that repository ($GITHUB_REPOSITORY). Every
# step that publishes runs it first, so a fork can build and test, and make Development
# builds, but never publishes a release, a list, a site or a branch under DSA's name.

set -euo pipefail

official="thisisthepy/desktop-subsystem-for-android"

case "${1:-}" in
    "")
        echo "$official"
        ;;
    --require)
        repo="${GITHUB_REPOSITORY:?the repository is not set (GITHUB_REPOSITORY)}"
        if [[ "$repo" != "$official" ]]; then
            echo "::error::This run is in $repo. Only $official publishes DSA (releases, image and program lists, the site, the release branch); a fork makes Development builds, which publish nothing (dev-build.yml)." >&2
            exit 1
        fi
        echo "publishing from the official repository $official"
        ;;
    *)
        echo "usage: official-repository.sh [--require]" >&2
        exit 2
        ;;
esac
