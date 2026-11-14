#!/usr/bin/env bash
# Keeps a framework build's compiler cache between runs as a workflow artifact
# (framework-ccache-<android>-<arch>, one tar file) instead of in GitHub's Actions cache.
#
# The Actions cache holds 10 GB for the whole repository, and the rest of CI keeps about 7 GB
# there, read many times a day. Each framework cache is about 1.8 GB and is read only when the
# framework builds, so GitHub evicted them as the least recently read: the run on develop of
# 2026-11-14 found none of its four and built every one cold (about 4 % compiler cache hits,
# 92 to 136 minutes), while a run that found its cache hit 95 % and built in 19 minutes. An
# artifact is not evicted; it lasts its retention, and this script deletes the ones a newer
# run replaces.
#
#   ccache-artifact.sh restore <name> <ccache dir> <download dir>
#       Unpacks into <ccache dir> the newest unexpired artifact <name> of a run on this branch,
#       or else of a run on develop. Writes found=true or found=false to $GITHUB_OUTPUT. A
#       missing or broken artifact only means a cold build, so it never fails.
#   ccache-artifact.sh pack <ccache dir> <tar file>
#       Packs <ccache dir> into <tar file>, the single file the artifact holds. The cache's
#       files are compressed already, so the artifact is uploaded without compression.
#   ccache-artifact.sh prune <name> <kept artifact id>
#       Deletes every artifact <name> of a run on develop made before <kept artifact id>. The
#       kept one holds what it was restored from and what its run added, so the older ones are
#       never restored again.
#
# GH_TOKEN needs actions: read (actions: write for prune). GITHUB_REPOSITORY names the
# repository, GITHUB_HEAD_REF or GITHUB_REF_NAME the branch.
set -euo pipefail

repo="${GITHUB_REPOSITORY:?name the repository in GITHUB_REPOSITORY}"
develop=develop

# Every unexpired artifact called $1 as "id, run id, branch, created at", newest first.
artifacts() {
    gh api --paginate "repos/$repo/actions/artifacts?name=$1&per_page=100" \
        --jq '.artifacts[] | select(.expired | not) | [.id, .workflow_run.id, .workflow_run.head_branch, .created_at] | @tsv' |
        LC_ALL=C sort -t $'\t' -k4,4r
}

output() {
    echo "$1" >> "${GITHUB_OUTPUT:-/dev/null}"
}

case "${1:-}" in
    restore)
        name="${2:?name the artifact}"
        dir="${3:?name the compiler cache folder}"
        download="${4:?name the folder to download into}"
        branch="${GITHUB_HEAD_REF:-${GITHUB_REF_NAME:-$develop}}"
        list="$(artifacts "$name")" || { echo "::warning::could not list the artifacts $name; the build starts with an empty compiler cache"; output found=false; exit 0; }
        pick=""
        for want in "$branch" "$develop"; do
            pick="$(awk -F '\t' -v b="$want" '$3 == b { print $1 "\t" $2 "\t" $4; exit }' <<<"$list")"
            [[ -n "$pick" ]] && break
        done
        if [[ -z "$pick" ]]; then
            echo "no run on $branch or $develop kept $name; the build starts with an empty compiler cache"
            output found=false
            exit 0
        fi
        IFS=$'\t' read -r id run created <<<"$pick"
        echo "restoring $name (artifact $id of run $run, made $created)"
        mkdir -p "$dir" "$download"
        zip="$download/$name.zip"
        start=$SECONDS
        # The artifact API serves a run's artifact while that run is still going, and follows
        # to the storage it is kept in; curl drops the token on that other host.
        if curl -fsSL --retry 5 --retry-all-errors -H "Authorization: Bearer $GH_TOKEN" \
                -o "$zip" "https://api.github.com/repos/$repo/actions/artifacts/$id/zip" &&
            unzip -p "$zip" ccache.tar | tar -x -C "$dir"; then
            rm -f "$zip"
            echo "restored $(du -sh "$dir" | cut -f1) in $(( SECONDS - start )) seconds"
            output found=true
        else
            # A half unpacked cache is still usable by ccache, which checks every entry, but an
            # empty one says plainly what happened.
            rm -rf "$zip" "$dir"
            mkdir -p "$dir"
            echo "::warning::could not restore $name from artifact $id; the build starts with an empty compiler cache"
            output found=false
        fi
        ;;
    pack)
        dir="${2:?name the compiler cache folder}"
        tar="${3:?name the tar file}"
        mkdir -p "$(dirname "$tar")"
        tar -C "$dir" -cf "$tar" .
        ls -l "$tar"
        ;;
    prune)
        name="${2:?name the artifact}"
        kept="${3:?name the artifact id that was just uploaded}"
        list="$(artifacts "$name")"
        saved="$(awk -F '\t' -v id="$kept" '$1 == id { print $4; exit }' <<<"$list")"
        if [[ -z "$saved" ]]; then
            echo "artifact $kept is not listed, so the ones before it stay"
            exit 0
        fi
        # Only the ones made before the kept one go: a run that started later may have kept a
        # newer one in the meantime, and restore picks that.
        awk -F '\t' -v id="$kept" -v saved="$saved" -v b="$develop" \
            '$3 == b && $1 != id && $4 < saved { print $1 "\t" $2 }' <<<"$list" |
            while IFS=$'\t' read -r id run; do
                if gh api -X DELETE "repos/$repo/actions/artifacts/$id" >/dev/null 2>&1; then
                    echo "deleted $name of run $run (artifact $id): superseded by artifact $kept"
                else
                    echo "artifact $id of run $run was already gone"
                fi
            done
        ;;
    *)
        echo "usage: ccache-artifact.sh restore <name> <ccache dir> <download dir> | pack <ccache dir> <tar file> | prune <name> <kept artifact id>" >&2
        exit 2
        ;;
esac
