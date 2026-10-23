#!/usr/bin/env bash
# Asserts that all dependencies satisfy the licence policy and that the licence bundle
# contains all required third-party component licences and Cargo.lock crates.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

if command -v uv >/dev/null 2>&1; then
    run_py() { uv run "$@"; }
elif command -v python3 >/dev/null 2>&1; then
    run_py() { python3 "$@"; }
else
    run_py() { python "$@"; }
fi

if [[ ! -f "dsa/deny.toml" ]]; then
    echo "missing dsa/deny.toml licence policy" >&2
    exit 1
fi

# 1. Check licence policy against allow-list
run_py dsa/host/platform/windows/installer/generate-licenses.py \
    --workspace dsa --check-only

# 2. Test licence bundle generation
out_dir=".scratch/nfr7-lic-test"
rm -rf "$out_dir"
mkdir -p "$out_dir"

run_py dsa/host/platform/windows/installer/generate-licenses.py \
    --workspace dsa \
    --out-dir "$out_dir/licenses" \
    --master-notice "$out_dir/NOTICE.txt"

# 3. Verify required licence files
for file in NOTICE.txt rust-crates.txt kernel-GPL-2.0.txt wix-MS-RL.txt setup-ui.txt gfxstream.txt aosp-notice.txt apache-2.0.txt mit.txt; do
    if [[ ! -f "$out_dir/licenses/$file" ]]; then
        echo "licence bundle missing expected file: $file" >&2
        rm -rf "$out_dir"
        exit 1
    fi
done

if [[ ! -f "$out_dir/NOTICE.txt" ]]; then
    echo "master NOTICE.txt was not generated" >&2
    rm -rf "$out_dir"
    exit 1
fi

# 4. Verify that every crate in Cargo.lock is listed in rust-crates.txt
crates=($(grep -E 'name = "[^"]+"' dsa/Cargo.lock | sed -E 's/name = "([^"]+)"/\1/' | sort -u))
missing=0
for crate in "${crates[@]}"; do
    if ! grep -q -E "^${crate} " "$out_dir/licenses/rust-crates.txt"; then
        echo "crate $crate from Cargo.lock not found in licence bundle" >&2
        missing=$((missing + 1))
    fi
done

rm -rf "$out_dir"

if (( missing > 0 )); then
    echo "total missing crates: $missing" >&2
    exit 1
fi

echo "ok    all dependencies comply with licence policy and are bundled"
