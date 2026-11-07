"""Reads and checks digitalis-lock.json, the pins the translator build is made from.

The lock holds one entry per Android version Digitalis has a branch for (android16,
android17). Each entry pins the Digitalis manifest commit it was resolved from, the AOSP
superproject commit that gave the AOSP projects their commits, and the projects the build
fetches: the few AOSP projects the translator, its guest libraries and its host tests need,
and Digitalis' own translator repository. Every project is an exact commit, so a rebuild is
the same build.

A project may be fetched in part:

    "cone": ["soong", "{clang}"]      git's cone mode: these folders, and the files in the
                                      project's root and in each folder on the way to them
    "patterns": ["/include/"]         gitignore-style patterns; the lock uses them for
                                      projects the build takes only headers from, and leaves
                                      their build files out so the build never sees their
                                      modules

    "exclude": ["apex/Android.bp"]    files deleted after the checkout, for build files whose
                                      modules this build never uses and cannot analyse
                                      without projects it leaves out

"{clang}" and "{rust}" stand for the Clang and the Rust toolchain the build uses, which the
fetch reads from the build system itself (see clang_versions and rust_versions).
"""

import json
import re
from pathlib import Path

LOCK = Path(__file__).with_name("digitalis-lock.json")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
PROJECT_PATH = re.compile(r"^[A-Za-z0-9_.+-]+(/[A-Za-z0-9_.+-]+)*$")
CLANG_TOKEN = "{clang}"
RUST_TOKEN = "{rust}"
# Where the translator's own repository sits in the tree, and what the package is called.
TRANSLATOR_PATH = "frameworks/libs/binary_translation"
TRANSLATOR_NAME = "Digitalis"
TRANSLATOR_LICENSE = "Apache-2.0"


class LockError(ValueError):
    pass


def load(path=LOCK):
    lock = json.loads(Path(path).read_text())
    check(lock)
    return lock


def check(lock):
    """Raises LockError naming the first thing that is wrong with `lock`."""
    versions = lock.get("versions")
    if not isinstance(versions, list) or not versions:
        raise LockError("the lock has no versions")
    seen = set()
    for version in versions:
        android = version.get("android")
        if not isinstance(android, str) or not re.fullmatch(r"android[0-9]+", android):
            raise LockError(f"{android!r} is not a Digitalis branch name such as android17")
        if android in seen:
            raise LockError(f"{android} is in the lock twice")
        seen.add(android)
        if version.get("android_major") != int(android[len("android"):]):
            raise LockError(f"{android}'s android_major does not match its name")
        for what in ("manifest", "superproject"):
            pin = version.get(what) or {}
            if not COMMIT.match(pin.get("commit", "")):
                raise LockError(f"{android}'s {what} is not pinned to a commit")
        projects = version.get("projects")
        if not isinstance(projects, list) or not projects:
            raise LockError(f"{android} has no projects")
        paths = set()
        for project in projects:
            check_project(android, project)
            if project["path"] in paths:
                raise LockError(f"{android} has {project['path']} twice")
            paths.add(project["path"])
        if TRANSLATOR_PATH not in paths:
            raise LockError(f"{android} does not fetch the translator ({TRANSLATOR_PATH})")


def check_project(android, project):
    path = project.get("path", "")
    where = f"{android} {path or '(a project without a path)'}"
    if not PROJECT_PATH.match(path) or ".." in path.split("/"):
        raise LockError(f"{where}: the path must be relative and plain")
    if not COMMIT.match(project.get("commit", "")):
        raise LockError(f"{where}: not pinned to a commit")
    if not project.get("url", "").startswith("https://"):
        raise LockError(f"{where}: the URL must be https")
    if "cone" in project and "patterns" in project:
        raise LockError(f"{where}: a project is fetched either by cone or by patterns, not both")
    for key in ("cone", "patterns"):
        if key in project:
            entries = project[key]
            if not isinstance(entries, list) or not entries or not all(
                    isinstance(e, str) and e and ".." not in e.split("/") for e in entries):
                raise LockError(f"{where}: {key} must be a list of paths inside the project")
    exclude = project.get("exclude", [])
    if not isinstance(exclude, list) or not all(
            isinstance(e, str) and e and not e.startswith("/") and ".." not in e.split("/") for e in exclude):
        raise LockError(f"{where}: exclude must be a list of files inside the project")
    for key in ("linkfiles", "copyfiles"):
        for entry in project.get(key, []):
            for side in ("src", "dest"):
                value = entry.get(side, "")
                if not value or value.startswith("/") or ".." in value.split("/"):
                    raise LockError(f"{where}: a {key[:-1]} {side} must be a relative path")


def version_entry(lock, android):
    for version in lock["versions"]:
        if version["android"] == android:
            return version
    raise LockError(f"the lock has no {android}; it has "
                    + ", ".join(v["android"] for v in lock["versions"]))


def project(version, path):
    for entry in version["projects"]:
        if entry["path"] == path:
            return entry
    raise LockError(f"{version['android']} does not fetch {path}")


def translator_version(version):
    """The package version: the Android major and the translator commit it was built from."""
    return f"{version['android_major']}-{project(version, TRANSLATOR_PATH)['commit'][:12]}"


def sparse_entries(project, clang=(), rust=()):
    """The project's cone folders or patterns, with {clang} and {rust} replaced by each Clang
    and Rust toolchain version the build uses."""
    key = "cone" if "cone" in project else "patterns" if "patterns" in project else None
    if key is None:
        return None, []
    out = []
    for entry in project[key]:
        for token, versions, what in ((CLANG_TOKEN, clang, "Clang"), (RUST_TOKEN, rust, "Rust")):
            if token in entry:
                if not versions:
                    raise LockError(f"{project['path']} needs the {what} version, which is not known yet")
                out.extend(entry.replace(token, version) for version in versions)
                break
        else:
            out.append(entry)
    return key, list(dict.fromkeys(out))


def needs_clang(project):
    return any(CLANG_TOKEN in e for e in project.get("cone", []) + project.get("patterns", []))


RUST_DEFAULT = re.compile(r'RustDefaultVersion\s*=\s*"([0-9][A-Za-z0-9.]*)"')


def rust_versions(top):
    """The Rust toolchain folder the build uses (Soong's RustDefaultVersion), or none when the
    tree's Soong names none."""
    config = Path(top) / "build/soong/rust/config/global.go"
    found = RUST_DEFAULT.search(config.read_text()) if config.is_file() else None
    return [found.group(1)] if found else []


CLANG_DEFAULT = re.compile(r'ClangDefaultVersion\s*=\s*"(clang-[A-Za-z0-9.]+)"')
CLANG_FLAG = re.compile(r'string_value:\s*"(clang-[A-Za-z0-9.]+)"')


def clang_versions(top, release="trunk_staging"):
    """The Clang folders the build uses: the release configuration's choice
    (RELEASE_BUILD_CLANG_VERSION) when it makes one, else Soong's default."""
    top = Path(top)
    flag = top / "build/release/flag_values" / release / "RELEASE_BUILD_CLANG_VERSION.textproto"
    if flag.is_file():
        found = CLANG_FLAG.search(flag.read_text())
        if found:
            return [found.group(1)]
    found = CLANG_DEFAULT.search((top / "build/soong/cc/config/global.go").read_text())
    if not found:
        raise LockError("build/soong/cc/config/global.go names no ClangDefaultVersion")
    return [found.group(1)]


def main(argv):
    """`versions` prints the lock's branches as a JSON list; `version <branch>` prints the
    package version of one; both check the lock first."""
    lock = load()
    if argv[1:] == ["versions"]:
        print(json.dumps([v["android"] for v in lock["versions"]]))
    elif len(argv) == 3 and argv[1] == "version":
        print(translator_version(version_entry(lock, argv[2])))
    else:
        raise SystemExit("usage: digitalis_lock.py versions | version <android16|android17>")


if __name__ == "__main__":
    import sys
    main(sys.argv)
