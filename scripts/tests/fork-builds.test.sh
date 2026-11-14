#!/usr/bin/env bash
# Checks that a fork of the repository can make Development builds and run the tests, and
# that nothing it runs publishes under DSA's name: releases, image and program lists, the
# site and the release branch come from the official repository only.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
official_script="$root/.github/scripts/release/official-repository.sh"
secrets_script="$root/.github/scripts/release/check-signing-secrets.sh"
workflows="$root/.github/workflows"

fail() {
    echo "fork builds: $*" >&2
    exit 1
}

official="$(bash "$official_script")"
[[ "$official" == thisisthepy/desktop-subsystem-for-android ]] || fail "the official repository is $official"

# The official repository may publish; a fork may not, and is told why.
GITHUB_REPOSITORY="$official" bash "$official_script" --require >/dev/null \
    || fail "the official repository was refused"
if out="$(GITHUB_REPOSITORY=someone/desktop-subsystem-for-android bash "$official_script" --require 2>&1)"; then
    fail "a fork was allowed to publish"
fi
[[ "$out" == *"someone/desktop-subsystem-for-android"*"Development builds"* ]] \
    || fail "the refusal does not say where the run is and what a fork does instead: $out"
if (unset GITHUB_REPOSITORY; bash "$official_script" --require >/dev/null 2>&1); then
    fail "a run without GITHUB_REPOSITORY was allowed to publish"
fi

# Every literal official name in the workflows is the one the script prints, so a rename
# cannot leave one guard behind.
while IFS= read -r line; do
    [[ "$line" == *"$official"* ]] || fail "a workflow names another official repository: $line"
done < <(grep -rhoE "thisisthepy/[A-Za-z0-9_.-]+" "$workflows" | sort -u)

# The signing secrets: each missing one is named before anything is built.
secrets() { env -u DSA_ANDROID_KEYS_1 -u DSA_ANDROID_KEYS_2 -u DSA_RELEASE_SIGNING_KEY "$@"; }
if out="$(secrets GITHUB_REPOSITORY=someone/fork bash "$secrets_script" true true 2>&1)"; then
    fail "an image build without any signing secret was allowed"
fi
for name in DSA_ANDROID_KEYS_1 DSA_ANDROID_KEYS_2 DSA_RELEASE_SIGNING_KEY; do
    [[ "$out" == *"$name"* ]] || fail "the missing $name is not named: $out"
done
[[ "$out" == *"someone/fork"* ]] || fail "the refusal does not name the repository: $out"
if out="$(secrets DSA_ANDROID_KEYS_1=a DSA_ANDROID_KEYS_2=b bash "$secrets_script" true false 2>&1)"; then
    fail "an image build without DSA_RELEASE_SIGNING_KEY was allowed"
fi
[[ "$out" == *DSA_RELEASE_SIGNING_KEY* && "$out" != *DSA_ANDROID_KEYS_1* ]] \
    || fail "only the missing secret is named: $out"
secrets DSA_RELEASE_SIGNING_KEY=k bash "$secrets_script" false true >/dev/null \
    || fail "an installer build needs more than DSA_RELEASE_SIGNING_KEY"
secrets bash "$secrets_script" false false >/dev/null || fail "a run that signs nothing needs a secret"
secrets DSA_ANDROID_KEYS_1=a DSA_ANDROID_KEYS_2=b DSA_RELEASE_SIGNING_KEY=k bash "$secrets_script" true true >/dev/null \
    || fail "an image build with every secret was refused"
grep -Fq '.github/scripts/release/check-signing-secrets.sh' "$workflows/release-image.yml" \
    || fail "the Image release does not check the signing secrets"

# The workflows themselves, read without a YAML library: which jobs and steps publish, and
# what guards them.
uv run --no-project python - "$workflows" "$official" <<'PY'
import re
import sys
from pathlib import Path

workflows, official = Path(sys.argv[1]), sys.argv[2]
guard = f"github.repository == '{official}'"
errors = []


def jobs(path):
    """Each job of a workflow: its name, its own if: text and its steps (each step's text)."""
    lines = path.read_text(encoding="utf-8").splitlines()
    out, job, in_jobs = [], None, False
    for line in lines:
        if line.startswith("jobs:"):
            in_jobs = True
            continue
        if not in_jobs:
            continue
        if re.match(r"^  [A-Za-z0-9_-]+:\s*$", line):
            job = {"name": line.strip()[:-1], "head": [], "steps": [], "in_steps": False}
            out.append(job)
            continue
        if job is None:
            continue
        if re.match(r"^    steps:\s*$", line):
            job["in_steps"] = True
            continue
        if job["in_steps"] and line.startswith("      - "):
            job["steps"].append([line])
        elif job["in_steps"] and job["steps"] and (line.startswith("        ") or not line.strip()):
            job["steps"][-1].append(line)
        elif re.match(r"^    [a-z-]+:", line):
            job["in_steps"] = False
            job["head"].append(line)
        elif not job["in_steps"]:
            job["head"].append(line)
    for job in out:
        head = "\n".join(job["head"])
        match = re.search(r"^    if:(.*?)(?=^    [a-z-]+:|\Z)", head, re.S | re.M)
        job["if"] = match.group(1) if match else ""
        job["steps"] = ["\n".join(step) for step in job["steps"]]
    return out


def step_if(step):
    match = re.search(r"^        if:(.*)$", step, re.M)
    return match.group(1) if match else ""


def step_name(step):
    match = re.search(r"name:\s*(.*)", step)
    return match.group(1).strip() if match else step.splitlines()[0].strip()


PUBLISHES = re.compile(r"gh release (create|upload|edit|delete)|publish-program-list\.sh|"
                       r"actions/deploy-pages|git push origin release|gh pr create")

checked = 0
for name in ("release-image.yml", "release-site.yml", "release-sync.yml"):
    for job in jobs(workflows / name):
        for step in job["steps"]:
            body = "\n".join(l for l in step.splitlines() if not l.strip().startswith("#"))
            if not PUBLISHES.search(body):
                continue
            checked += 1
            if guard not in job["if"] and guard not in step_if(step):
                errors.append(f"{name}: '{step_name(step)}' in job {job['name']} publishes, "
                              f"and neither it nor its job runs only when {guard}")
if checked < 8:
    errors.append(f"only {checked} publishing steps were found; the reader no longer understands the workflows")

# A release refuses any repository but the official one before anything is built; a
# Development build is decided before that check, so a fork's develop is accepted.
image = (workflows / "release-image.yml").read_text(encoding="utf-8")
channel = next(j for j in jobs(workflows / "release-image.yml") if j["name"] == "channel")
decide = next(s for s in channel["steps"] if "release-branch-channel.sh" in s)
dev = decide.find('if [[ "$DEV_BUILD" == true ]]; then')
dev_end = decide.find("exit 0", dev)
require = decide.find("official-repository.sh --require")
if not (0 <= dev < dev_end < require):
    errors.append("the channel job does not decide a Development build before it requires the official repository")
if "github.repository" in decide[dev:dev_end] or official in decide[dev:dev_end]:
    errors.append("the Development build's branch check looks at the repository, so a fork's develop is refused")
for job in jobs(workflows / "release-image.yml"):
    if job["name"] in ("dev-installer", "published", "image", "list", "kernel", "guest-programs", "agent", "prebuilt"):
        if guard in job["if"]:
            errors.append(f"release-image.yml: the job {job['name']}, which a Development build runs, needs the official repository")

dev_build = (workflows / "dev-build.yml").read_text(encoding="utf-8")
code = "\n".join(l for l in dev_build.splitlines() if not l.lstrip().startswith("#"))
if "github.repository ==" in code or "github.repository !=" in code or official in code:
    errors.append("dev-build.yml checks the repository, so a fork cannot make a Development build")
if 'refs/heads/develop' not in code:
    errors.append("dev-build.yml no longer requires develop")

# What an installed Development build reads stays the official repository's: the image and
# program lists its installer names, the published list its trust key is checked against,
# and the releases its image revision has to be newer than.
dev_installer = next(j for j in jobs(workflows / "release-image.yml") if j["name"] == "dev-installer")
build = next(s for s in dev_installer["steps"] if "windows-installer" in s)
for key in ("list-url", "program-list-url", "prerelease-program-list-url"):
    match = re.search(rf"^\s*{key}: (.*)$", build, re.M)
    if not match or "${{ env.OFFICIAL_REPOSITORY }}" not in match.group(1):
        errors.append(f"the Development build's installer takes {key} from somewhere other than the official repository")
if not re.search(rf"^  OFFICIAL_REPOSITORY: {re.escape(official)}$", image, re.M):
    errors.append("release-image.yml does not name the official repository in OFFICIAL_REPOSITORY")
if "gh release download images --repo \"$OFFICIAL_REPOSITORY\"" not in image:
    errors.append("the published image list is not read from the official repository")
if 'repos/$OFFICIAL_REPOSITORY/releases?per_page=100' not in image:
    errors.append("the image revision is not chosen against the official repository's releases")

# What a fork finds in itself: the runs it builds on.
for text, what in ((image, "the Translator and Framework build runs"),
                   ((Path(sys.argv[1]).parent / "actions/windows-installer/action.yml").read_text(encoding="utf-8"),
                    "the graphics build runs")):
    if re.search(r"gh run (list|download)[^\n]*--repo\s+\"?" + re.escape(official), text):
        errors.append(f"{what} are looked up in the official repository, not the one the run is in")

if errors:
    print("\n".join(errors), file=sys.stderr)
    sys.exit(1)
PY

# install-dev-build.ps1 takes the repository from -Repo or the checkout's origin.
if ! command -v pwsh >/dev/null; then
    fail "pwsh is not installed, so install-dev-build.ps1 cannot be checked"
fi
pwsh -NoProfile -NonInteractive -File "$root/scripts/tests/install-dev-build_test.ps1"
