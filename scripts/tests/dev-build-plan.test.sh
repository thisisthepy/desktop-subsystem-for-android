#!/usr/bin/env bash
# Checks how the Development build decides what to make (.github/scripts/release/dev-build-plan.sh):
# a part is made when a file it is made from changed since the last Development build that
# made it, documents and tests change nothing, and the workflow builds through the release's
# own jobs and publishes nothing.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
plan="$root/.github/scripts/release/dev-build-plan.sh"
workflow="$root/.github/workflows/dev-build.yml"
release="$root/.github/workflows/release-image.yml"

fail() {
    echo "dev build plan: $*" >&2
    exit 1
}

needs() { printf '%s\n' "${@:2}" | bash "$plan" needs "$1"; }

# What each part is made from.
[[ "$(needs image dsa/guest/hwcomposer/src/hwc.cpp)" == true ]] || fail "a guest change does not make the image"
[[ "$(needs installer dsa/guest/hwcomposer/src/hwc.cpp)" == false ]] || fail "a guest change makes the installer"
[[ "$(needs image dsa/kernel/android.config)" == true ]] || fail "a kernel change does not make the image"
[[ "$(needs installer dsa/host/core/src/session.rs)" == true ]] || fail "a host change does not make the installer"
[[ "$(needs image dsa/host/core/src/session.rs)" == false ]] || fail "a host change makes the image"
[[ "$(needs installer dsa/host/platform/windows/settings/DsaSettings/App.xaml.cs)" == true ]] \
    || fail "a settings app change does not make the installer"
for part in image installer; do
    [[ "$(needs "$part" dsa/protocol/control.proto)" == true ]] || fail "a protocol change does not make the $part"
    [[ "$(needs "$part" .github/workflows/release-image.yml)" == true ]] || fail "a release workflow change does not make the $part"
    [[ "$(needs "$part" dsa/Cargo.lock)" == true ]] || fail "a Cargo.lock change does not make the $part"
    # Documents and tests change nothing that is built.
    [[ "$(needs "$part" README.md dsa/host/core/tests/update.rs dsa/guest/image/test_make_image.py docs/SPEC.md scripts/tests/no-em-dash.test.sh)" == false ]] \
        || fail "documents or tests make the $part"
    [[ "$(needs "$part" < /dev/null)" == false ]] || fail "nothing changed makes the $part"
done
# Windows line endings in the list are read the same.
[[ "$(printf 'dsa/host/cli/src/lib.rs\r\n' | bash "$plan" needs installer)" == true ]] \
    || fail "a path with a carriage return is not read"

# By hand: what was asked for, without looking at any run.
[[ "$(bash "$plan" plan all HEAD | tr '\n' ' ')" == "image=true installer=true image_base= installer_base= " ]] \
    || fail "all does not make both"
[[ "$(bash "$plan" plan image HEAD | head -2 | tr '\n' ' ')" == "image=true installer=false " ]] \
    || fail "image does not make the image alone"
[[ "$(bash "$plan" plan installer HEAD | head -2 | tr '\n' ' ')" == "image=false installer=true " ]] \
    || fail "installer does not make the installer alone"
! bash "$plan" plan sometimes HEAD >/dev/null 2>&1 || fail "an unknown choice was accepted"

# auto, with a stand-in gh serving runs from files: $RUNS holds "<run> <sha>" lines, and
# $ARTIFACTS/<run> the names of that run's artifacts.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/artifacts"
cat > "$work/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
    "run list") cat "$RUNS" ;;
    "api repos/{owner}/{repo}/actions/runs/"*)
        run="${2#repos/\{owner\}/\{repo\}/actions/runs/}"
        run="${run%%/*}"
        name="$(sed -n 's/.*select(.name == "\([a-z-]*\)".*/\1/p' <<<"$4")"
        if [[ -f "$ARTIFACTS/$run" ]] && grep -qx "$name" "$ARTIFACTS/$run"; then echo 1; else echo 0; fi
        ;;
    *) echo "unexpected gh $*" >&2; exit 1 ;;
esac
EOF
chmod +x "$work/bin/gh"
repo="$work/repo"
git init -q "$repo"
commit() {
    mkdir -p "$repo/$(dirname "$1")"
    echo "$RANDOM" >> "$repo/$1"
    git -C "$repo" add -A
    git -C "$repo" -c user.name=test -c user.email=test@example.com commit -q -m "$1"
    git -C "$repo" rev-parse HEAD
}
first="$(commit dsa/guest/hwcomposer/hwc.cpp)"
auto() {
    (cd "$repo" && PATH="$work/bin:$PATH" RUNS="$work/runs" ARTIFACTS="$work/artifacts" \
        bash "$plan" plan auto "$(git rev-parse HEAD)" | head -2 | tr '\n' ' ')
}

# No Development build yet: both are made.
: > "$work/runs"
[[ "$(auto)" == "image=true installer=true " ]] || fail "the first Development build does not make both"

# The last one made both at the first commit; then only the host changed.
echo "101 $first" > "$work/runs"
printf 'development-image\ndevelopment-image-list\ndevelopment-installer\n' > "$work/artifacts/101"
commit dsa/host/core/src/session.rs >/dev/null
[[ "$(auto)" == "image=false installer=true " ]] || fail "a host change does not make the installer alone: $(auto)"

# Then the guest changed as well, and a later run made only the installer: the image is
# compared with the run that made it, not with the newest run.
second="$(commit dsa/guest/audio/audio.cpp)"
printf '102 %s\n101 %s\n' "$second" "$first" > "$work/runs"
printf 'development-installer\n' > "$work/artifacts/102"
[[ "$(auto)" == "image=true installer=false " ]] || fail "a guest change hidden by an installer-only run is missed: $(auto)"

# A run whose image artifact has gone counts as none.
: > "$work/artifacts/101"
[[ "$(auto)" == "image=true installer=false " ]] || fail "an expired image is reused"

# The workflow: by hand on develop, through the release's own jobs, publishing nothing.
grep -Fq 'name: Development build' "$workflow" || fail "the workflow is not named Development build"
if grep -Eq '^\s+(push|pull_request|schedule|workflow_run):' "$workflow"; then
    fail "the Development build has an automatic trigger"
fi
grep -Fq 'uses: ./.github/workflows/release-image.yml' "$workflow" || fail "the Development build does not reuse the release's jobs"
grep -Fq 'dev_build: true' "$workflow" || fail "the Development build does not ask for a development build"
grep -Fq 'workflow_call:' "$release" || fail "the release workflow cannot be called"
# Every publishing step of the release is off for a development build.
for step in 'name: Publish the image' 'name: Publish the lists'; do
    grep -A3 -F "$step" "$release" | grep -Fq 'if: ${{ !inputs.dev_build }}' || fail "$step runs for a development build"
done
grep -Fq "!cancelled() && !inputs.dev_build && inputs.installer_version != ''" "$release" \
    || fail "the release installer, which publishes, runs for a development build"
grep -Fq 'retention-days: 3' "$release" || fail "the development artifacts are kept longer than three days"
echo "dev build plan: ok"
