"""Applies DSA's patches for one Android version to a tree fetch-source.py made.

    uv run dsa/guest/framework/apply-patches.py --android android17 --top <tree>
    uv run dsa/guest/framework/apply-patches.py --android android17 --top <tree> --check

Each patches/<project>/<android>/NNNN-*.patch applies with `git apply` in <tree>/<project>, in
order. A patch that does not apply exactly stops it: a patch applied in part, or with fuzz to
other source than it was made for, is not the change that was reviewed. With --check nothing
is changed. <tree>/.dsa-patches.json then lists what was applied.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import framework_lock  # noqa: E402


class PatchError(RuntimeError):
    pass


def git_apply(project_dir, patch, check):
    # The project folder is no repository of its own (fetch-source.py drops each .git). Git must
    # not find one around it either: inside another repository, git apply would skip every
    # path outside the folder it runs in and report success.
    env = dict(os.environ, GIT_CEILING_DIRECTORIES=str(project_dir.parent))
    args = ["git", "apply", "--verbose"] + (["--check"] if check else []) + [str(patch)]
    result = subprocess.run(args, cwd=project_dir, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        raise PatchError(f"{patch.name} does not apply to {project_dir}:\n{result.stderr.strip()}")
    applied = [line for line in result.stderr.splitlines() if line.startswith(("Applied patch", "Checking patch"))]
    if not applied:
        raise PatchError(f"{patch.name} changed nothing in {project_dir}")
    return applied


def apply_patches(android, top, check=False, patches=framework_lock.PATCHES):
    """Applies (or with `check` only tries) every patch for `android` under `top`, and returns
    what it applied as (project, patch name, SHA-256)."""
    top = Path(top)
    found = framework_lock.patch_files(android, patches)
    if not found:
        raise PatchError(f"there are no patches for {android}")
    done = []
    for project, patch in found:
        project_dir = top / project
        if not project_dir.is_dir():
            raise PatchError(f"{project} is not in {top}, so {patch.name} has nothing to apply to")
        for line in git_apply(project_dir, patch, check):
            print(f"{project}: {line}", flush=True)
        done.append((project, patch.name, hashlib.sha256(patch.read_bytes()).hexdigest()))
    return done


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--android", required=True)
    parser.add_argument("--top", type=Path, required=True)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        done = apply_patches(args.android, args.top, args.check)
    except PatchError as e:
        raise SystemExit(str(e))
    if not args.check:
        (args.top / ".dsa-patches.json").write_text(json.dumps(
            [{"project": p, "patch": n, "sha256": s} for p, n, s in done], indent=2) + "\n")
    print(f"{len(done)} patches {'apply' if args.check else 'applied'} for {args.android}")


if __name__ == "__main__":
    sys.exit(main())
