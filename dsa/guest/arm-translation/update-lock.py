"""Moves digitalis-lock.json to the newest commits of each Digitalis branch.

    uv run dsa/guest/arm-translation/update-lock.py               every version, every project
    uv run dsa/guest/arm-translation/update-lock.py --missing     only projects without a commit
    uv run dsa/guest/arm-translation/update-lock.py --android android17

It reads Digitalis' manifest at the head of the version's branch (default.xml, the AOSP
manifest it starts from, and digitalis.xml, which replaces a few projects with Digitalis'
own), takes each AOSP project's commit from the AOSP superproject of the manifest's branch,
and each Digitalis project's commit from the head of its branch on GitHub. To add a project,
add {"path": "..."} to a version's projects (with "cone" or "patterns" to fetch it in part)
and run this with --missing, which pins it without moving the others.

It needs git and network access. It downloads only the two manifest files and the
superproject's trees, never a project's files.
"""

import argparse
import json
import subprocess
import sys
import tempfile
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import digitalis_lock  # noqa: E402

MANIFEST_REPO = "https://github.com/DigitalisX64/manifest"
RAW = "https://raw.githubusercontent.com/DigitalisX64/manifest/{commit}/{file}"


def git(*args, cwd=None):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout


def branch_head(url, branch):
    out = git("ls-remote", url, f"refs/heads/{branch}")
    if not out.strip():
        raise SystemExit(f"{url} has no branch {branch}")
    return out.split()[0]


def https(fetch):
    """A manifest remote's fetch URL as https (Digitalis names GitHub by ssh)."""
    if fetch.startswith("git@github.com:"):
        fetch = "https://github.com/" + fetch[len("git@github.com:"):]
    if fetch.startswith("ssh://git@github.com/"):
        fetch = "https://github.com/" + fetch[len("ssh://git@github.com/"):]
    return fetch.rstrip("/") + "/"


def read_manifest(commit):
    """{path: project} from default.xml with digitalis.xml applied, and the superproject."""
    texts = {}
    for name in ("default.xml", "digitalis.xml"):
        with urllib.request.urlopen(RAW.format(commit=commit, file=name)) as response:
            texts[name] = response.read()
    remotes, projects, superproject, default = {}, {}, None, {}
    for name in ("default.xml", "digitalis.xml"):
        root = ET.fromstring(texts[name])
        for remote in root.findall("remote"):
            remotes[remote.get("name")] = {"fetch": https(remote.get("fetch")),
                                           "revision": remote.get("revision")}
        if root.find("default") is not None:
            default = root.find("default").attrib
        if root.find("superproject") is not None:
            superproject = root.find("superproject").attrib
        for removed in root.findall("remove-project"):
            projects.pop(removed.get("path") or removed.get("name"), None)
        for element in root.findall("project"):
            remote = element.get("remote") or default.get("remote")
            path = element.get("path") or element.get("name")
            projects[path] = {
                "name": element.get("name"),
                "remote": remote,
                "revision": element.get("revision") or remotes[remote].get("revision")
                or default.get("revision"),
                "linkfiles": [dict(l.attrib) for l in element.findall("linkfile")],
                "copyfiles": [dict(c.attrib) for c in element.findall("copyfile")],
            }
    return remotes, projects, superproject, default


def superproject_commits(url, ref):
    """The superproject commit at `ref` (a branch, or a commit already pinned) and
    {path: commit} of its gitlinks."""
    with tempfile.TemporaryDirectory() as tmp:
        git("init", "-q", tmp)
        wanted = ref if len(ref) == 40 and all(c in "0123456789abcdef" for c in ref) else f"refs/heads/{ref}"
        git("fetch", "-q", "--depth=1", "--filter=blob:none", url, wanted, cwd=tmp)
        head = git("rev-parse", "FETCH_HEAD", cwd=tmp).strip()
        links = {}
        for line in git("ls-tree", "-r", "FETCH_HEAD", cwd=tmp).splitlines():
            meta, path = line.split("\t", 1)
            mode, kind, sha = meta.split()
            if kind == "commit":
                links[path] = sha
    return head, links


def update_version(version, missing_only):
    android = version["android"]
    manifest = version.setdefault("manifest", {"url": MANIFEST_REPO, "branch": android})
    if not missing_only or not manifest.get("commit"):
        manifest["commit"] = branch_head(manifest["url"], manifest["branch"])
    remotes, projects, superproject, default = read_manifest(manifest["commit"])
    aosp = remotes[superproject.get("remote") or default["remote"]]["fetch"]
    sp_branch = superproject.get("revision") or default["revision"]
    pin = version.setdefault("superproject", {})
    if not missing_only or not pin.get("commit"):
        head, links = superproject_commits(aosp + superproject["name"], sp_branch)
        pin.update({"url": aosp + superproject["name"], "branch": sp_branch, "commit": head})
    else:
        # Projects added to a pinned lock take their commits from the same superproject
        # commit as the others, so the tree stays one AOSP state.
        _, links = superproject_commits(aosp + superproject["name"], pin["commit"])
    version["aosp_branch"] = sp_branch
    for entry in version["projects"]:
        if missing_only and entry.get("commit"):
            continue
        found = projects.get(entry["path"])
        if found is None:
            raise SystemExit(f"{android}: the manifest has no project at {entry['path']}")
        url = remotes[found["remote"]]["fetch"] + found["name"]
        entry["name"] = found["name"]
        entry["url"] = url
        if url.startswith(aosp) and entry["path"] in links:
            entry["commit"] = links[entry["path"]]
        else:
            entry["commit"] = branch_head(url, found["revision"])
        for key in ("linkfiles", "copyfiles"):
            if found[key]:
                entry[key] = [{"src": f["src"], "dest": f["dest"]} for f in found[key]]
            else:
                entry.pop(key, None)
        print(f"{android} {entry['path']} {entry['commit']}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--lock", type=Path, default=digitalis_lock.LOCK)
    parser.add_argument("--android", action="append", help="only this version (android16, android17)")
    parser.add_argument("--missing", action="store_true", help="pin only projects that have no commit yet")
    args = parser.parse_args()
    lock = json.loads(args.lock.read_text())
    for version in lock["versions"]:
        if args.android and version["android"] not in args.android:
            continue
        update_version(version, args.missing)
    digitalis_lock.check(lock)
    args.lock.write_text(json.dumps(lock, indent=2) + "\n")


if __name__ == "__main__":
    sys.exit(main())
