#!/usr/bin/env bash
# Tests for .github/scripts/protocol/regenerate.sh.
#
# Every case runs against throwaway repositories built in this repository's ignored
# .scratch/, never against this repository or any remote, with a stand-in generator that
# writes each definition's hash next to it, as the real one writes the .sha256 files.

set -uo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
regenerate="$script_dir/../../.github/scripts/protocol/regenerate.sh"

failures=0
pass() { echo "ok   - $1"; }
fail() { echo "FAIL - $1"; failures=$((failures + 1)); }
check() { if [[ "$2" == "$3" ]]; then pass "$1"; else fail "$1: expected [$3], got [$2]"; fi; }

export DSA_PROTOCOL_CODEGEN='for p in protocol/*.proto; do sha256sum "$p" | cut -c1-64 > "${p%.proto}.sha256"; done'

mkdir -p "$script_dir/../../.scratch"
tmp="$(mktemp -d "$script_dir/../../.scratch/protocol-sync-test.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

hash_of() { sha256sum "$1" | cut -c1-64; }

# An origin with develop holding one definition and its hash, and a clone to work in.
make_origin() {
    local dir="$1"
    git init -q --bare -b develop "$dir/origin.git"
    git clone -q "$dir/origin.git" "$dir/seed" 2> /dev/null
    git -C "$dir/seed" config user.email test@example.invalid
    git -C "$dir/seed" config user.name "Sync Test"
    git -C "$dir/seed" checkout -q -b develop
    mkdir -p "$dir/seed/dsa/protocol"
    printf 'message A {}\n\n\n\n\n\n\n\nmessage Z {}\n' > "$dir/seed/dsa/protocol/control.proto"
    hash_of "$dir/seed/dsa/protocol/control.proto" > "$dir/seed/dsa/protocol/control.sha256"
    git -C "$dir/seed" add -A
    git -C "$dir/seed" commit -q -m "Initial commit"
    git -C "$dir/seed" push -q origin develop
}

# Commits a new definition on `branch` (made from develop when new), with or without the
# hash the generator would write.
change() {
    local dir="$1" branch="$2" proto="$3" regenerate_hash="$4"
    git -C "$dir/seed" fetch -q origin
    if git -C "$dir/seed" rev-parse -q --verify "origin/$branch" > /dev/null; then
        git -C "$dir/seed" checkout -q -B "$branch" "origin/$branch"
    else
        git -C "$dir/seed" checkout -q -B "$branch" origin/develop
    fi
    printf '%b' "$proto" > "$dir/seed/dsa/protocol/control.proto"
    if [[ "$regenerate_hash" == yes ]]; then
        hash_of "$dir/seed/dsa/protocol/control.proto" > "$dir/seed/dsa/protocol/control.sha256"
    fi
    git -C "$dir/seed" commit -qam "Change $branch"
    git -C "$dir/seed" push -q origin "$branch"
}

run() {
    local dir="$1" branch="$2"
    rm -rf "$dir/work"
    git clone -q "$dir/origin.git" "$dir/work" 2> /dev/null
    git -C "$dir/work" config user.email ci@example.invalid
    git -C "$dir/work" config user.name "CI"
    (cd "$dir/work" && bash "$regenerate" "$branch" develop 2>&1 | tail -1)
}

head_of() { git -C "$1/origin.git" rev-parse "$2"; }
file_on() { git -C "$1/origin.git" show "$2:dsa/protocol/$3"; }
subject_on() { git -C "$1/origin.git" log -1 --format=%s "$2"; }

# --- a branch whose generated code is up to date is left alone -----------------
d="$tmp/fresh"; make_origin "$d"
change "$d" feat/x 'message A { int32 a = 1; }\n\n\n\n\n\n\n\nmessage Z {}\n' yes
before="$(head_of "$d" feat/x)"
check "an up-to-date branch is unchanged (result)" "$(run "$d" feat/x)" "unchanged"
check "an up-to-date branch is unchanged (head)" "$(head_of "$d" feat/x)" "$before"

# --- a branch that changed a definition but not its code gets the code ---------
d="$tmp/stale"; make_origin "$d"
change "$d" feat/x 'message A { int32 a = 1; }\n\n\n\n\n\n\n\nmessage Z {}\n' no
check "a stale branch is regenerated (result)" "$(run "$d" feat/x)" "pushed"
check "a stale branch is regenerated (subject)" "$(subject_on "$d" feat/x)" \
    "Chore: Regenerate the protocol code"
check "a stale branch is regenerated (hash)" "$(file_on "$d" feat/x control.sha256)" \
    "$(file_on "$d" feat/x control.proto | sha256sum | cut -c1-64)"

# --- generated files that conflict with develop are merged and regenerated -----
d="$tmp/conflict"; make_origin "$d"
change "$d" feat/x 'message A { int32 a = 1; }\n\n\n\n\n\n\n\nmessage Z {}\n' yes
change "$d" develop 'message A {}\n\n\n\n\n\n\n\nmessage Z { int32 z = 1; }\n' yes
check "a generated-only conflict is resolved (result)" "$(run "$d" feat/x)" "pushed"
check "a generated-only conflict is resolved (merged definition)" \
    "$(file_on "$d" feat/x control.proto)" \
    "$(printf 'message A { int32 a = 1; }\n\n\n\n\n\n\n\nmessage Z { int32 z = 1; }')"
check "a generated-only conflict is resolved (hash of the merge)" \
    "$(file_on "$d" feat/x control.sha256)" \
    "$(file_on "$d" feat/x control.proto | sha256sum | cut -c1-64)"
check "a generated-only conflict is resolved (develop merged)" \
    "$(git -C "$d/origin.git" merge-base --is-ancestor develop feat/x; echo $?)" "0"

# --- a conflict in a definition is left for a person ----------------------------
d="$tmp/proto"; make_origin "$d"
change "$d" feat/x 'message A { int32 a = 1; }\n\n\n\n\n\n\n\nmessage Z {}\n' yes
change "$d" develop 'message A { int64 b = 2; }\n\n\n\n\n\n\n\nmessage Z {}\n' yes
before="$(head_of "$d" feat/x)"
check "a definition conflict is left (result)" "$(run "$d" feat/x)" "left"
check "a definition conflict is left (head)" "$(head_of "$d" feat/x)" "$before"

if ((failures)); then
    echo "$failures failed"
    exit 1
fi
echo "all passed"
