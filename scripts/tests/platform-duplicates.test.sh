#!/usr/bin/env bash
# Fails when two platform crates carry the same free function with the same body.
#
# The platform crates (windows, linux, macos, unix) hold only what talks to an OS. A function
# that two of them spell the same way is a rule or a helper that belongs in `dsa/host/core`
# (or in `unix`, for the Linux and macOS hosts), once. Only top-level functions outside
# `#[cfg(test)]` are compared, with comments and indentation ignored. A function that is the
# same text because each crate must have its own copy (it reads a constant or an OS call of
# that crate) goes in the list below, with the reason.
#
# `--names` also lists functions of the same name with different bodies, which are not an
# error: they are where to look next.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

# name  reason
allowed="
advertiser   builds the advertiser from that host's own OS access (mDNS), by the same one line
build        names the renderer by that crate's own VERSION constant
follow       hands the links watch that crate's own BUILD name, by the same one line, because the core watch is generic
look         hands the widget look that crate's own DEFAULT_ACCENT constant, by the same one line
"

files="$(git ls-files 'dsa/host/platform/windows/src/*.rs' 'dsa/host/platform/linux/src/*.rs' \
    'dsa/host/platform/macos/src/*.rs' 'dsa/host/platform/unix/src/*.rs')"

report="$(echo "$files" | ALLOWED="$allowed" xargs awk '
BEGIN {
    n = split(ENVIRON["ALLOWED"], lines, "\n")
    for (i = 1; i <= n; i++) {
        split(lines[i], word, " ")
        if (word[1] != "") ok[word[1]] = 1
    }
}
FNR == 1 { intest = 0; infn = 0 }
/^#\[cfg\(test\)\]/ { intest = 1 }
{
    if (intest) next
    if (!infn && match($0, /^(pub(\([a-z]+\))? )?(const )?(unsafe )?(async )?fn [a-z_0-9]+/)) {
        head = substr($0, RSTART, RLENGTH)
        sub(/^.*fn /, "", head)
        name = head
        split(FILENAME, part, "/")
        crate = part[4]
        where = FILENAME ":" FNR
        body = ""
        infn = 1
    }
    if (infn) {
        text = $0
        sub(/^[ \t]+/, "", text)
        if (text !~ /^\/\//) body = body " " text
        # A function ends at its closing brace in column 0, or on its own line when it is
        # written on one line.
        if ($0 ~ /^}/ || ($0 ~ /}[ \t]*$/ && $0 ~ /^[^ ]/ && $0 ~ /{.*}/)) {
            infn = 0
            key = name SUBSEP body
            if (!(name in crates) || index(crates[name], " " crate " ") == 0) crates[name] = crates[name] " " crate " "
            same[key] = same[key] " " crate "@" where
            samecrates[key] = samecrates[key] " " crate " "
            keyname[key] = name
            allkeys[key] = 1
        }
    }
}
END {
    for (key in allkeys) {
        name = keyname[key]
        count = 0
        seen = ""
        m = split(samecrates[key], c, " ")
        for (i = 1; i <= m; i++) if (index(seen, " " c[i] " ") == 0) { seen = seen " " c[i] " "; count++ }
        if (count > 1 && !(name in ok)) print "SAME " name ":" same[key]
    }
    for (name in crates) {
        m = split(crates[name], c, " ")
        if (m > 1 && !(name in ok)) print "NAME " name ":" crates[name]
    }
}
' | sort)"

status=0
same="$(echo "$report" | grep '^SAME ' || true)"
if [[ -n "$same" ]]; then
    echo "error: the same function is written in more than one platform crate;" >&2
    echo "       move it to dsa/host/core (or list it in this script with the reason)" >&2
    echo "$same" | sed 's/^SAME /  /' >&2
    status=1
fi
if [[ "${1:-}" == "--names" ]]; then
    echo "same name in more than one platform crate (bodies may differ):"
    echo "$report" | grep '^NAME ' | sed 's/^NAME /  /' || true
fi
if [[ $status -eq 0 ]]; then
    echo "ok    no function is written twice across the platform crates"
fi
exit $status
