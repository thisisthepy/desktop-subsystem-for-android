"""Moves framework-lock.json to the build an image is made from, or adds projects to it.

    uv run dsa/guest/framework/update-lock.py --android android17
    uv run dsa/guest/framework/update-lock.py --android android17 --add external/foo

It reads the build number and target dsa/guest/image/inputs.json pins for that Android version,
downloads that build's manifest (manifest_<build_id>.xml) from ci.android.com, and sets the
lock's build, manifest SHA-256 and every project's repository and commit to the manifest's.
With --add, the named manifest projects join the lock whole (narrow them to a cone by hand).
release, clang and rust are left as they are: read them from the new build's
SYSTEM/etc/build_flags.json (RELEASE_BUILD_CLANG_VERSION and the release the build ID names)
and its build/soong (RustDefaultVersion).
"""

import argparse
import hashlib
import json
import sys
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import framework_lock  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--android", required=True)
    parser.add_argument("--manifest", type=Path, help="a manifest already downloaded")
    parser.add_argument("--add", action="append", default=[], help="a manifest project path to add")
    args = parser.parse_args()

    lock = json.loads(framework_lock.LOCK.read_text())
    version = framework_lock.version_entry(lock, args.android)
    images = json.loads(framework_lock.INPUTS.read_text())["images"]
    image = next(i for i in images if i["android_major"] == version["android_major"])
    version["build_id"] = image["build_id"]
    version["target"] = image["target"]
    url = framework_lock.MANIFEST_URL.format(build_id=image["build_id"], target=image["target"])
    if args.manifest:
        text = args.manifest.read_bytes()
    else:
        with urllib.request.urlopen(url) as response:
            text = response.read()
    version["manifest"] = {"url": url, "sha256": hashlib.sha256(text).hexdigest()}

    commits = framework_lock.manifest_commits(text)
    paths = {p["path"] for p in version["projects"]}
    for path in args.add:
        if path in paths:
            raise SystemExit(f"{path} is already in the lock")
        version["projects"].append({"path": path})
    missing = []
    for project in version["projects"]:
        found = commits.get(project["path"])
        if found is None:
            missing.append(project["path"])
            continue
        name, revision = found
        project["url"] = framework_lock.GOOGLESOURCE + name
        project["commit"] = revision
    if missing:
        raise SystemExit(f"build {image['build_id']}'s manifest has no {', '.join(missing)}")
    version["projects"].sort(key=lambda p: p["path"])
    # Keep the key order of the other entries.
    version["projects"] = [{k: p[k] for k in ("path", "url", "commit", "cone", "patterns", "linkfiles", "copyfiles")
                            if k in p} for p in version["projects"]]
    framework_lock.check(lock, images)
    framework_lock.LOCK.write_text(json.dumps(lock, indent=2) + "\n")
    print(f"{args.android}: build {image['build_id']}, {len(version['projects'])} projects, "
          f"version {framework_lock.build_version(version)}")


if __name__ == "__main__":
    sys.exit(main())
