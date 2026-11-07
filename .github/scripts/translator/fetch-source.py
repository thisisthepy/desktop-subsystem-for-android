"""Fetches the reduced AOSP tree one Digitalis version is built from, as
dsa/guest/arm-translation/digitalis-lock.json pins it, or the tree the patched framework is
built from, as dsa/guest/framework/framework-lock.json pins it (--lock).

    fetch-source.py --android android17 --top <tree> [--jobs 8]
    fetch-source.py --lock dsa/guest/framework/framework-lock.json --android android17 --top <tree>

Instead of repo, each project is fetched at its pinned commit alone: one commit deep, and for
a project the lock fetches in part, with blobs left on the server until git's sparse checkout
asks for the ones it keeps. That is what keeps the tree to a few gigabytes. The manifest's
linkfile and copyfile entries are then made as repo makes them, and each project's .git is
dropped, since the build reads none of them; <tree>/.dsa-sources.json says what was fetched.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / "dsa/guest/arm-translation"))
sys.path.insert(0, str(REPO / "dsa/guest/framework"))
import digitalis_lock  # noqa: E402
import framework_lock  # noqa: E402

# What the {clang} and {rust} entries need: the build's own word on its toolchains.
FIRST = ["build/soong", "build/release"]


def git(*args, cwd):
    subprocess.run(["git", "-c", "advice.detachedHead=false", *args], cwd=cwd, check=True)


def fetch(top, project, clang, rust):
    path = top / project["path"]
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)
    mode, entries = digitalis_lock.sparse_entries(project, clang, rust)
    for attempt in range(4):
        try:
            if (path / ".git").exists():
                shutil.rmtree(path / ".git")
            git("init", "-q", cwd=path)
            git("remote", "add", "origin", project["url"], cwd=path)
            fetch_args = ["fetch", "-q", "--depth=1", "--no-tags"]
            if mode:
                git("config", "core.sparseCheckout", "true", cwd=path)
                git("sparse-checkout", "set", "--cone" if mode == "cone" else "--no-cone",
                    *entries, cwd=path)
                fetch_args.append("--filter=blob:none")
            git(*fetch_args, "origin", project["commit"], cwd=path)
            git("checkout", "-q", "--detach", "FETCH_HEAD", cwd=path)
            break
        except subprocess.CalledProcessError:
            if attempt == 3:
                raise
            wait = 15 * (attempt + 1)
            print(f"{project['path']}: fetch failed, again in {wait} s", flush=True)
            time.sleep(wait)
    shutil.rmtree(path / ".git")
    for name in project.get("exclude", []):
        excluded = path / name
        if not excluded.is_file():
            raise SystemExit(f"{project['path']}: {name}, which the lock excludes, is not in the checkout")
        excluded.unlink()
    print(f"{project['path']} {project['commit'][:12]}"
          + (f" ({mode}: {' '.join(entries)})" if mode else ""), flush=True)


def link_files(top, project):
    """The manifest's linkfile (a relative symbolic link) and copyfile entries."""
    for entry in project.get("linkfiles", []):
        source = top / project["path"] / entry["src"]
        dest = top / entry["dest"]
        if not source.exists():
            print(f"{project['path']}: skipping the link to {entry['src']}, which is not fetched")
            continue
        dest.parent.mkdir(parents=True, exist_ok=True)
        if dest.is_symlink() or dest.exists():
            dest.unlink()
        dest.symlink_to(os.path.relpath(source, dest.parent))
    for entry in project.get("copyfiles", []):
        source = top / project["path"] / entry["src"]
        if source.is_file():
            (top / entry["dest"]).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(source, top / entry["dest"])


def load_version(path, android):
    """The lock's entry for `android`, checked by the reader of the lock's kind."""
    path = Path(path)
    if json.loads(path.read_text()).get("kind") == framework_lock.KIND:
        return framework_lock.version_entry(framework_lock.load(path), android)
    return digitalis_lock.version_entry(digitalis_lock.load(path), android)


def toolchains(version, top):
    """The Clang and Rust versions the build uses: the ones the lock names (a framework lock
    names the published build's own), or else what the build system says."""
    clang = [version["clang"]] if version.get("clang") else digitalis_lock.clang_versions(
        top, version.get("release", "trunk_staging"))
    rust = [version["rust"]] if version.get("rust") else digitalis_lock.rust_versions(top)
    return clang, rust


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--android", required=True)
    parser.add_argument("--top", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument("--lock", type=Path, default=digitalis_lock.LOCK)
    args = parser.parse_args()
    version = load_version(args.lock, args.android)
    top = args.top.resolve()
    top.mkdir(parents=True, exist_ok=True)
    projects = version["projects"]

    first = [p for p in projects if p["path"] in FIRST]
    for project in first:
        fetch(top, project, (), ())
    clang, rust = toolchains(version, top)
    print(f"the build uses {', '.join(clang)} and Rust {', '.join(rust) or '(none named)'}", flush=True)
    rest = [p for p in projects if p["path"] not in FIRST]
    # The large prebuilts first, so they do not finish last on their own.
    rest.sort(key=lambda p: not p["path"].startswith("prebuilts/"))
    with ThreadPoolExecutor(args.jobs) as pool:
        for future in [pool.submit(fetch, top, p, clang, rust) for p in rest]:
            future.result()
    for project in projects:
        link_files(top, project)
    sources = {
        "android": version["android"],
        **{key: version[key] for key in ("build_id", "release", "manifest", "superproject") if key in version},
        "clang": clang,
        "rust": rust,
        "projects": {p["path"]: {"url": p["url"], "commit": p["commit"]} for p in projects},
    }
    (top / ".dsa-sources.json").write_text(json.dumps(sources, indent=2) + "\n")


if __name__ == "__main__":
    sys.exit(main())
