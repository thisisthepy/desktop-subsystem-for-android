#!/usr/bin/env bash
# Tests for .github/scripts/release/sync-release.sh.
#
# Every case runs against a throwaway repository built in a temporary
# directory -- never against this repository, and never against any remote.

set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
split="$script_dir/../../.github/scripts/release/sync-release.sh"

failures=0
pass() { echo "ok   - $1"; }
fail() { echo "FAIL - $1"; failures=$((failures + 1)); }
check() { if [[ "$2" == "$3" ]]; then pass "$1"; else fail "$1: expected [$3], got [$2]"; fi; }
check_contains() {
    if [[ "$2" == *"$3"* ]]; then pass "$1"; else fail "$1: [$2] does not contain [$3]"; fi
}
check_absent() {
    if [[ "$2" != *"$3"* ]]; then pass "$1"; else fail "$1: [$2] unexpectedly contains [$3]"; fi
}

# A repository shaped like this one: public source and translations, private
# planning documents.
make_repo() {
    local dir="$1"
    mkdir -p "$dir"
    git -C "$dir" init -q -b develop
    git -C "$dir" config user.email test@example.invalid
    git -C "$dir" config user.name "Split Test"
    mkdir -p "$dir/docs/locales" "$dir/dsa/src"
    echo readme > "$dir/README.md"
    echo license > "$dir/LICENSE"
    echo project > "$dir/PROJECT.md"
    echo claude > "$dir/CLAUDE.md"
    echo intent > "$dir/docs/INTENT.md"
    echo spec > "$dir/docs/SPEC.md"
    echo translation > "$dir/docs/locales/README_ko.md"
    echo code > "$dir/dsa/src/lib.rs"
    git -C "$dir" add -A
    git -C "$dir" commit -q -m "Initial commit"
}

files_on() { git -C "$1" ls-tree -r --name-only "$2" | sort; }

# --- refuses to run with uncommitted changes -------------------------------
# Inside this repository's ignored .scratch/, like everything else the project makes.
mkdir -p "$script_dir/../../.scratch"
tmp="$(mktemp -d "$script_dir/../../.scratch/sync-release-test.XXXXXX")"
repo="$tmp/dirty"
make_repo "$repo"
echo "scratch" >> "$repo/README.md"
out="$(cd "$repo" && "$split" --target main 2>&1)"
status=$?
check "refuses with uncommitted changes (exit)" "$status" "1"
check_contains "refuses with uncommitted changes (message)" "$out" "uncommitted"
check "refuses with uncommitted changes (main not created)" \
    "$(git -C "$repo" rev-parse --verify -q main >/dev/null 2>&1; echo $?)" "1"

# --- dry run is the default and writes nothing -----------------------------
repo="$tmp/dry"
make_repo "$repo"
out="$(cd "$repo" && "$split" --target main 2>&1)"
check "dry run succeeds" "$?" "0"
check_contains "dry run names itself" "$out" "dry run"
check_contains "dry run lists PROJECT.md" "$out" "PROJECT.md"
check_contains "dry run lists docs/SPEC.md" "$out" "docs/SPEC.md"
check_absent "dry run keeps README.md off the removal list" "$out" "- README.md"
check "dry run does not create main" \
    "$(git -C "$repo" rev-parse --verify -q main >/dev/null 2>&1; echo $?)" "1"

# --- --write produces main with the private documents removed --------------
repo="$tmp/write"
make_repo "$repo"
out="$(cd "$repo" && "$split" --write --target main 2>&1)"
check "--write succeeds" "$?" "0"
main_files="$(files_on "$repo" main)"
check "main keeps the public tree" "$main_files" "$(printf '%s\n' \
    LICENSE README.md dsa/src/lib.rs docs/locales/README_ko.md | sort)"
check_absent "main drops PROJECT.md" "$main_files" "PROJECT.md"
check_absent "main drops CLAUDE.md" "$main_files" "CLAUDE.md"
check_absent "main drops docs/INTENT.md" "$main_files" "docs/INTENT.md"
check_absent "main drops docs/SPEC.md" "$main_files" "docs/SPEC.md"

# --- the working tree of the current branch is never touched ---------------
check "develop's working tree still has PROJECT.md" "$(cat "$repo/PROJECT.md")" "project"
check "develop's working tree still has docs/SPEC.md" "$(cat "$repo/docs/SPEC.md")" "spec"
check "the checkout is still on develop" "$(git -C "$repo" rev-parse --abbrev-ref HEAD)" "develop"
check "the checkout is still clean" "$(git -C "$repo" status --porcelain)" ""

# --- it prints the push command instead of pushing -------------------------
check_contains "prints the push command" "$out" "git push origin main"
check "no remote was contacted (none exists)" "$(git -C "$repo" remote)" ""

# --- idempotent: a second run is a no-op -----------------------------------
before="$(git -C "$repo" rev-parse main)"
out="$(cd "$repo" && "$split" --write --target main 2>&1)"
check "second run succeeds" "$?" "0"
check_contains "second run reports no work" "$out" "up to date"
check "second run leaves main unmoved" "$(git -C "$repo" rev-parse main)" "$before"

# --- re-runnable as develop advances ---------------------------------------
echo more > "$repo/dsa/src/extra.rs"
echo "more spec" >> "$repo/docs/SPEC.md"
git -C "$repo" add -A
git -C "$repo" commit -q -m "Feat: More"
(cd "$repo" && "$split" --write --target main >/dev/null 2>&1)
main_files="$(files_on "$repo" main)"
check_contains "an advanced develop brings new source to main" "$main_files" "extra.rs"
check_absent "an advanced develop still drops docs/SPEC.md" "$main_files" "docs/SPEC.md"
if [[ "$(git -C "$repo" rev-parse main)" != "$before" ]]; then
    pass "main advanced"
else
    fail "main advanced: still at $before"
fi
check "main descends from the previous main" \
    "$(git -C "$repo" merge-base --is-ancestor "$before" main; echo $?)" "0"
check "main records develop in its history" \
    "$(git -C "$repo" merge-base --is-ancestor develop main; echo $?)" "0"

# --- new files directly under docs/ are dropped without editing the script -
echo notes > "$repo/docs/ROADMAP.md"
git -C "$repo" add -A
git -C "$repo" commit -q -m "Docs: Roadmap"
(cd "$repo" && "$split" --write --target main >/dev/null 2>&1)
check_absent "a new docs/ document is dropped too" "$(files_on "$repo" main)" "ROADMAP.md"
check_contains "docs/locales/ survives" "$(files_on "$repo" main)" "docs/locales/README_ko.md"

# --- safe to run while main itself is checked out --------------------------
repo="$tmp/on-main"
make_repo "$repo"
(cd "$repo" && "$split" --write --target main >/dev/null 2>&1)
git -C "$repo" checkout -q main
out="$(cd "$repo" && "$split" --write --target main 2>&1)"
check "running while on main succeeds" "$?" "0"
check "running while on main leaves the tree clean" "$(git -C "$repo" status --porcelain)" ""
check "running while on main keeps main's checkout intact" \
    "$([[ -f "$repo/README.md" ]] && echo yes)" "yes"

# --- works from a branch whose HEAD differs from the source branch ---------
#
# Regression: `git rm --cached` compares index entries against HEAD, so
# without --force it refuses when the checked-out branch has diverged from
# --source, and the private files silently survive into the target.
repo="$tmp/other-head"
make_repo "$repo"
git -C "$repo" checkout -q -b feature
echo work > "$repo/dsa/src/feature.rs"
# The private files must differ between HEAD and --source too: that is what
# makes git compare them and refuse.
echo "edited on the feature branch" >> "$repo/PROJECT.md"
echo "edited on the feature branch" >> "$repo/docs/SPEC.md"
git -C "$repo" add -A
git -C "$repo" commit -q -m "Feat: Work in progress"
out="$(cd "$repo" && "$split" --write --target main 2>&1)"
check "runs from a diverged branch" "$?" "0"
main_files="$(files_on "$repo" main)"
check_absent "a diverged HEAD still drops PROJECT.md" "$main_files" "PROJECT.md"
check_absent "a diverged HEAD still drops docs/SPEC.md" "$main_files" "docs/SPEC.md"
check_absent "the target takes content from --source, not HEAD" "$main_files" "feature.rs"
check "the diverged branch is still clean" "$(git -C "$repo" status --porcelain)" ""

# --- a missing develop branch is an error, not a silent success ------------
repo="$tmp/no-develop"
make_repo "$repo"
git -C "$repo" branch -m develop trunk
out="$(cd "$repo" && "$split" --target main 2>&1)"
check "missing source branch fails" "$?" "1"
check_contains "missing source branch explains itself" "$out" "develop"
out="$(cd "$repo" && "$split" --source trunk --target main 2>&1)"
check "--source selects another branch" "$?" "0"

# --- the default target is release, so the workflow needs no arguments -----
#
# main is protected and only moves through a pull request, so an argument-less
# run has to write the branch that pull request comes from.
repo="$tmp/default-target"
make_repo "$repo"
out="$(cd "$repo" && "$split" 2>&1)"
check "dry run names release as the target" "$?" "0"
check_contains "dry run names release as the target (message)" "$out" "release"
out="$(cd "$repo" && "$split" --write 2>&1)"
check_contains "prints the release push command" "$out" "git push origin release"
check "--write with no arguments creates release" \
    "$(git -C "$repo" rev-parse --verify -q release >/dev/null 2>&1; echo $?)" "0"
check "--write with no arguments leaves main alone" \
    "$(git -C "$repo" rev-parse --verify -q main >/dev/null 2>&1; echo $?)" "1"
release_files="$(files_on "$repo" release)"
check_absent "release drops PROJECT.md" "$release_files" "PROJECT.md"
check_contains "release keeps docs/locales/" "$release_files" "docs/locales/README_ko.md"

# --- continues from origin/<target> when there is no local branch -----------
#
# Regression: CI checkouts have the remote-tracking ref only. Building from
# nothing there produces a commit unrelated to what is published, and the push
# is rejected as a non fast-forward.
repo="$tmp/from-remote"
make_repo "$repo"
(cd "$repo" && "$split" --write >/dev/null 2>&1)
published="$(git -C "$repo" rev-parse release)"

clone="$tmp/from-remote-clone"
git clone -q "$repo" "$clone"
# A clone inherits no identity, and a CI runner has no global one to fall back on, so the
# commits below would fail with "empty ident name" on a machine that is not a developer's.
git -C "$clone" config user.email test@example.invalid
git -C "$clone" config user.name "Split Test"
git -C "$clone" checkout -q develop
check "the clone has no local release branch" \
    "$(git -C "$clone" rev-parse --verify -q release >/dev/null 2>&1; echo $?)" "1"
echo "more" > "$clone/dsa/src/later.rs"
git -C "$clone" add -A
git -C "$clone" commit -q -m "Feat: More work"
out="$(cd "$clone" && "$split" --write 2>&1)"
check "runs in a clone without a local target branch" "$?" "0"
check_contains "says where it continued from" "$out" "origin/release"
check "the new commit descends from what was published" \
    "$(git -C "$clone" merge-base --is-ancestor "$published" release; echo $?)" "0"
check_contains "the clone carries the new source across" "$(files_on "$clone" release)" "later.rs"

# --- main's own commits are absorbed, so release -> main never conflicts ---
#
# main moves only through pull requests from release, and each merge adds a commit release
# does not have. The next release must contain those commits, or the next pull request
# conflicts and only a force push could fix it.
repo="$tmp/absorb-main"
make_repo "$repo"
(cd "$repo" && "$split" --write >/dev/null 2>&1)
git -C "$repo" branch main release
git -C "$repo" checkout -q main
echo "merged" > "$repo/MERGED.md"
git -C "$repo" add -A
git -C "$repo" commit -q -m "Merge pull request from release"
main_tip="$(git -C "$repo" rev-parse main)"
git -C "$repo" checkout -q develop
echo "next" > "$repo/dsa/src/next.rs"
git -C "$repo" add -A
git -C "$repo" commit -q -m "Feat: Next"
out="$(cd "$repo" && "$split" --write 2>&1)"
check "absorbs main (exit)" "$?" "0"
check "main is an ancestor of the new release" \
    "$(git -C "$repo" merge-base --is-ancestor "$main_tip" release; echo $?)" "0"
check_absent "release keeps develop's tree, not main's extra file" "$(files_on "$repo" release)" "MERGED.md"
check_contains "release carries develop's new file" "$(files_on "$repo" release)" "next.rs"
check_contains "says it absorbed main" "$out" "absorbed main"
# A release whose tree is current but which lacks main's commits is not up to date.
git -C "$repo" checkout -q main
echo "again" > "$repo/MERGED.md"
git -C "$repo" commit -q -am "Merge pull request again"
main_tip="$(git -C "$repo" rev-parse main)"
git -C "$repo" checkout -q develop
(cd "$repo" && "$split" --write >/dev/null 2>&1)
check "absorbs main even when the tree is already current" \
    "$(git -C "$repo" merge-base --is-ancestor "$main_tip" release; echo $?)" "0"

rm -rf "$tmp"

if (( failures )); then
    echo "$failures test(s) failed" >&2
    exit 1
fi
echo "all sync-release.sh tests passed"
