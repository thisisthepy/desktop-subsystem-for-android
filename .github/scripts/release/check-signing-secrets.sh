#!/usr/bin/env bash
# Usage: ./.github/scripts/release/check-signing-secrets.sh <image true|false> <installer true|false>
#
# Fails before anything is built when a secret the run signs with is not set, and names each
# missing one. A fork has none of the official repository's secrets until its owner adds them,
# and without this check the run fails an hour later inside a build step.
#
#   image       the Android key set (DSA_ANDROID_KEYS_1, and _2 when the packed set is larger
#               than one secret holds; _3 is optional) and DSA_RELEASE_SIGNING_KEY, which signs
#               the image list.
#   installer   DSA_RELEASE_SIGNING_KEY, which signs the program list and whose public half
#               the installer trusts.
#
# The secrets come in as environment variables of the same names. A Development build reads
# the official repository's published image list and checks it with DSA_RELEASE_SIGNING_KEY,
# so a fork that makes the installer needs the same key as the official repository.

set -euo pipefail

[[ $# -eq 2 ]] || { echo "usage: check-signing-secrets.sh <image true|false> <installer true|false>" >&2; exit 2; }
image="$1"
installer="$2"

needed=()
if [[ "$image" == true ]]; then
    needed+=(DSA_ANDROID_KEYS_1 DSA_ANDROID_KEYS_2 DSA_RELEASE_SIGNING_KEY)
elif [[ "$installer" == true ]]; then
    needed+=(DSA_RELEASE_SIGNING_KEY)
fi

missing=()
for name in "${needed[@]}"; do
    if [[ -z "${!name:-}" ]]; then
        missing+=("$name")
    fi
done

repo="${GITHUB_REPOSITORY:-this repository}"
if (( ${#missing[@]} )); then
    echo "::error::$repo has no ${missing[*]} secret$( (( ${#missing[@]} > 1 )) && echo s). Add them under Settings > Secrets and variables > Actions; the header of .github/workflows/dev-build.yml says what each one holds." >&2
    exit 1
fi
echo "the signing secrets this run needs are set${needed[*]:+: ${needed[*]}}"
