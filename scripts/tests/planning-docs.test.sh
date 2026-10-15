#!/usr/bin/env bash
# Fails if the planning documents are missing from the working branch.
#
# They exist only on develop: .github/scripts/release/sync-release.sh strips them when it
# builds the public branch. That makes them easy to lose by accident, and the loss is
# quiet. A worktree cut from main or release starts without them, and merging such a
# branch back into develop replays the deletion as if someone meant it.
#
# Publishing is one way: develop to release to main. Nothing that has been through the
# publish script should ever be merged back.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

# Which branch this tree belongs to. A CI checkout is detached, so `git rev-parse
# --abbrev-ref HEAD` says "HEAD". On a pull request GITHUB_REF_NAME is the merge ref
# ("8/merge") and the proposed branch is in GITHUB_HEAD_REF, so that is read first.
branch="${GITHUB_HEAD_REF:-}"
if [[ -z "$branch" ]]; then
    branch="${GITHUB_REF_NAME:-}"
fi
if [[ -z "$branch" || "$branch" == "HEAD" ]]; then
    branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo detached)"
fi
case "$branch" in
    main|release)
        echo "ok    $branch is a published branch; the planning documents belong on develop"
        exit 0
        ;;
esac

required=(AGENTS.md PROJECT.md docs/INTENT.md docs/SPEC.md)
missing=()
for doc in "${required[@]}"; do
    [[ -s "$doc" ]] || missing+=("$doc")
done

if (( ${#missing[@]} )); then
    echo "error: the planning documents are missing from '$branch':" >&2
    printf '  %s\n' "${missing[@]}" >&2
    cat >&2 <<'HINT'

This usually means a branch cut from main or release was merged into develop, which
replays the deletion sync-release.sh made. Restore them from the last commit that had
them, and rebase the offending work onto develop instead of merging it:

  git log --diff-filter=D --oneline -- AGENTS.md PROJECT.md
  git checkout <commit-before-that> -- AGENTS.md PROJECT.md docs/INTENT.md docs/SPEC.md
HINT
    exit 1
fi

echo "ok    planning documents present on $branch"
