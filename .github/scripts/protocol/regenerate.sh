#!/usr/bin/env bash
# Brings one pull request branch's generated protocol code in step with its base.
#
# Usage: regenerate.sh <branch> <base>
#
# Run from a full clone of this repository whose origin can be pushed to, with protoc and
# cargo on the path. The branch is merged with its base as GitHub would merge it, and the
# generator runs on the result:
#
# - When the generated code of that merge is already what the generator makes, the branch
#   is left as it is.
# - When it is stale, or the merge conflicts only in generated files, the merge is committed
#   with the base's side of those files, the generator rewrites them from the merged
#   definitions, and the result is pushed as one commit, "Chore: Regenerate the protocol
#   code".
# - When anything else conflicts, a .proto file included, the branch is left for a person.
#
# Prints "pushed" on its last line when it pushed, so the caller can test the new commit.
set -euo pipefail

branch="$1"
base="$2"
subject="Chore: Regenerate the protocol code"
generated='^dsa/protocol/([a-z_]+\.sha256|rust/src/generated/|java/src/main/java/|cpp/generated/)'

# Leaves the checkout as the branch was, whatever state the merge is in.
undo() {
  git merge --abort > /dev/null 2>&1 || true
  git reset -q --hard "origin/$branch"
  git clean -qfd -- dsa/protocol
}

git fetch -q --no-tags origin "$branch" "$base"
git checkout -q -B "$branch" "origin/$branch"
undo

merged=clean
if ! git merge -q --no-commit --no-ff "origin/$base" > /dev/null 2>&1; then
  conflicts="$(git diff --name-only --diff-filter=U)"
  echo "$branch conflicts with $base in:"
  echo "$conflicts"
  if [[ -z "$conflicts" ]] || grep -qvE "$generated" <<< "$conflicts"; then
    echo "::notice::$branch: files other than generated protocol code conflict with $base; leaving the merge to a person"
    undo
    echo "left"
    exit 0
  fi
  # The base's side; the generator rewrites these files from the merged definitions.
  xargs git checkout --theirs -- <<< "$conflicts"
  xargs git add -- <<< "$conflicts"
  merged=conflicted
fi
# The merged tree before the generator runs.
before="$(git write-tree)"

if [[ -n "${DSA_PROTOCOL_CODEGEN:-}" ]]; then
  # A stand-in generator, for the script's own test.
  (cd dsa && bash -c "$DSA_PROTOCOL_CODEGEN")
else
  (cd dsa && PROTOC="$(command -v protoc)" cargo run -q -p dsa-protocol-codegen)
fi
git add -A -- dsa/protocol

if [[ "$merged" == clean && "$(git write-tree)" == "$before" ]]; then
  undo
  echo "$branch: the generated protocol code is up to date"
  echo "unchanged"
  exit 0
fi

if [[ "$merged" == clean ]]; then
  body="Regenerates the protocol code, with $base merged in, from the definitions the checked-in code no longer matched."
else
  body="Merges $base, whose generated protocol code conflicted with this branch's, and regenerates it from the merged definitions."
fi
git commit -q -m "$subject" -m "$body"
git push -q origin "HEAD:$branch"
echo "pushed"
