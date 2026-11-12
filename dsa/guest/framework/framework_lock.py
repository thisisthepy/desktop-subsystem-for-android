"""Reads and checks framework-lock.json, the pins the patched framework build is made from.

The build makes SurfaceFlinger, libhwui and libEGL with DSA's GPU device loss recovery, and DSA's
KeyMint, whose root key of key blobs comes from the DSA host (the patches in
patches/<project>/<android>/). The image takes the first three in place of the published build's
own, and installs the KeyMint in place of the build's KeyMint APEX (make-image.py --framework).
Their neighbours in the image stay the published ones, so each must be built from the exact
source of the build the image is made from.

The lock holds one entry per Android version DSA ships an image of (android16, android17). Each
entry names that published build (build_id and target, the ones dsa/guest/image/inputs.json
pins), the build's own manifest (manifest_<build_id>.xml, by URL and SHA-256) and release
configuration, the Clang and Rust versions it used (its SYSTEM/etc/build_flags.json and
build/soong say which), and the projects the build fetches, each at the commit the manifest
gives it.

Projects are written as in dsa/guest/arm-translation/digitalis-lock.json (cone or patterns for
a project fetched in part) and fetched by .github/scripts/translator/fetch-source.py.

    framework_lock.py versions                     the Android versions, as a JSON list
    framework_lock.py version <android>            the build's version: major and a hash of
                                                   the pins and the patches
    framework_lock.py check-manifest <android> <manifest.xml>
                                                   fails unless every pinned commit is the one
                                                   the build's manifest names
    framework_lock.py outputs                      where the build installs the files the image
                                                   takes (the framework's, then the KeyMint's),
                                                   one per line
"""

import hashlib
import json
import re
import sys
import xml.etree.ElementTree as ElementTree
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
sys.path.insert(0, str(REPO / "dsa/guest/arm-translation"))
import digitalis_lock  # noqa: E402

LOCK = HERE / "framework-lock.json"
PATCHES = HERE / "patches"
INPUTS = REPO / "dsa/guest/image/inputs.json"
KIND = "framework"
ARCHES = ("x86_64", "arm64")

# What the build makes and the image takes in place of the published build's file: where each
# is in target_files, and where the build installs it in its product folder.
OUTPUTS = {
    "SYSTEM/bin/surfaceflinger": "system/bin/surfaceflinger",
    "SYSTEM/lib64/libhwui.so": "system/lib64/libhwui.so",
    "SYSTEM/lib64/libEGL.so": "system/lib64/libEGL.so",
}

# DSA's KeyMint: the nonsecure KeyMint service whose TA takes the root key of key blobs from the
# DSA host (the patches in patches/hardware/interfaces, and in patches/system/keymint for
# Android 16, which lacks the TA's hook for moving blobs to a new root key). The image installs
# these as vendor files in place of the com.android.hardware.keymint APEX, which it then does
# not select. The binary links its Rust crates in; libssl, which the Rust openssl crate links, is
# the one shared library it needs that the published vendor partition does not carry.
KEYMINT_BINARY = "VENDOR/bin/hw/dsa-keymint"
KEYMINT_OUTPUTS = {
    KEYMINT_BINARY: "vendor/bin/hw/dsa-keymint",
    "VENDOR/lib64/libssl.so": "vendor/lib64/libssl.so",
}
# What the APEX carries beside its binary, taken as the tree has it: the VINTF fragments that
# declare the KeyMint, remotely provisioned component, secure clock and shared secret instances,
# and the hardware_keystore feature.
KEYMINT_DIR = "hardware/interfaces/security/keymint/aidl/default"
KEYMINT_SOURCES = {
    "VENDOR/etc/vintf/manifest/android.hardware.security.keymint-service.xml":
        f"{KEYMINT_DIR}/android.hardware.security.keymint-service.xml",
    "VENDOR/etc/vintf/manifest/android.hardware.security.secureclock-service.xml":
        f"{KEYMINT_DIR}/android.hardware.security.secureclock-service.xml",
    "VENDOR/etc/vintf/manifest/android.hardware.security.sharedsecret-service.xml":
        f"{KEYMINT_DIR}/android.hardware.security.sharedsecret-service.xml",
    "VENDOR/etc/permissions/android.hardware.hardware_keystore.xml":
        f"{KEYMINT_DIR}/android.hardware.hardware_keystore.xml",
}
KEYMINT_FILES = list(KEYMINT_OUTPUTS) + list(KEYMINT_SOURCES)

# Projects every version fetches: the build system, and the projects the patches change.
REQUIRED = ["build/make", "build/soong", "build/release", "build/blueprint",
            "frameworks/native", "frameworks/base", "hardware/interfaces", "system/keymint",
            "external/rust/crates/openssl", "external/boringssl"]
# The folders of a project fetched in part that the patches and the targets need.
REQUIRED_FOLDERS = {
    "frameworks/native": ["libs/renderengine", "services/surfaceflinger", "opengl", "libs/binder/rust"],
    "frameworks/base": ["libs/hwui"],
    "hardware/interfaces": ["security/keymint/aidl", "security/rkp/aidl", "security/secureclock/aidl",
                            "security/sharedsecret/aidl"],
    "external/boringssl": ["src/rust/bssl-sys"],
}

BUILD_ID = re.compile(r"^[0-9]+$")
RELEASE = re.compile(r"^[a-z][a-z0-9_]*$")
CLANG = re.compile(r"^clang-r[0-9]+[a-z]*$")
RUST = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
MANIFEST_URL = "https://ci.android.com/builds/submitted/{build_id}/{target}/latest/raw/manifest_{build_id}.xml"
GOOGLESOURCE = "https://android.googlesource.com/"


class LockError(ValueError):
    pass


def load(path=LOCK, inputs=INPUTS):
    lock = json.loads(Path(path).read_text())
    images = json.loads(Path(inputs).read_text())["images"] if inputs else None
    check(lock, images)
    return lock


def check(lock, images=None, patches=PATCHES):
    """Raises LockError naming the first thing wrong with `lock`. With `images` (inputs.json's
    list), each version must pin the build the images of its Android version are made from."""
    if lock.get("kind") != KIND:
        raise LockError(f"the lock's kind is {lock.get('kind')!r}, not {KIND!r}")
    versions = lock.get("versions")
    if not isinstance(versions, list) or not versions:
        raise LockError("the lock has no versions")
    seen = set()
    for version in versions:
        android = version.get("android")
        if not isinstance(android, str) or not re.fullmatch(r"android[0-9]+", android):
            raise LockError(f"{android!r} is not an Android version name such as android17")
        if android in seen:
            raise LockError(f"{android} is in the lock twice")
        seen.add(android)
        if version.get("android_major") != int(android[len("android"):]):
            raise LockError(f"{android}'s android_major does not match its name")
        build_id = version.get("build_id", "")
        if not isinstance(build_id, str) or not BUILD_ID.match(build_id):
            raise LockError(f"{android} does not pin a build by its number")
        target = version.get("target", "")
        if not isinstance(target, str) or not target.endswith("-userdebug"):
            raise LockError(f"{android}'s target {target!r} is not a userdebug build target")
        if not isinstance(version.get("release"), str) or not RELEASE.match(version["release"]):
            raise LockError(f"{android} does not name the build's release configuration")
        if not isinstance(version.get("clang"), str) or not CLANG.match(version["clang"]):
            raise LockError(f"{android} does not name the Clang the build used")
        if "rust" in version and (not isinstance(version["rust"], str) or not RUST.match(version["rust"])):
            raise LockError(f"{android}'s Rust version {version['rust']!r} is not a version")
        manifest = version.get("manifest") or {}
        if manifest.get("url") != MANIFEST_URL.format(build_id=build_id, target=target):
            raise LockError(f"{android}'s manifest is not build {build_id}'s own manifest")
        if not SHA256.match(manifest.get("sha256", "")):
            raise LockError(f"{android}'s manifest is not pinned by its SHA-256")
        projects = version.get("projects")
        if not isinstance(projects, list) or not projects:
            raise LockError(f"{android} has no projects")
        paths = {}
        for project in projects:
            digitalis_lock.check_project(android, project)
            if not project["url"].startswith(GOOGLESOURCE):
                raise LockError(f"{android} {project['path']}: not fetched from AOSP, so not the "
                                "published build's source")
            if project["path"] in paths:
                raise LockError(f"{android} has {project['path']} twice")
            paths[project["path"]] = project
        for path in REQUIRED:
            if path not in paths:
                raise LockError(f"{android} does not fetch {path}")
        for path, folders in REQUIRED_FOLDERS.items():
            cone = paths[path].get("cone")
            if "patterns" in paths[path]:
                raise LockError(f"{android} takes {path} by patterns; it must be whole or a cone")
            for folder in folders if cone is not None else []:
                if not any(folder == c or folder.startswith(c + "/") for c in cone):
                    raise LockError(f"{android} does not fetch {path}/{folder}, which the build needs")
        for project, _ in patch_files(android, patches):
            if project not in paths:
                raise LockError(f"{android} has patches for {project}, which it does not fetch")
        if images is not None:
            for image in images:
                if image["android_major"] == version["android_major"] and image["build_id"] != build_id:
                    raise LockError(f"{android} pins build {build_id}, but the {image.get('arch', 'x86_64')} "
                                    f"image is made from build {image['build_id']}")
            if not any(image["android_major"] == version["android_major"] for image in images):
                raise LockError(f"no image is made from {android}")


def version_entry(lock, android):
    for version in lock["versions"]:
        if version["android"] == android:
            return version
    raise LockError(f"the lock has no {android}; it has " + ", ".join(v["android"] for v in lock["versions"]))


def patch_files(android, patches=PATCHES):
    """The patches for `android`, as (project path, patch file), in the order they apply: by
    project, then by file name."""
    patches = Path(patches)
    found = []
    if not patches.is_dir():
        return found
    for folder in sorted(p for p in patches.rglob(android) if p.is_dir()):
        project = folder.parent.relative_to(patches).as_posix()
        for patch in sorted(folder.glob("*.patch")):
            found.append((project, patch))
    return found


def build_version(version, patches=PATCHES):
    """The version of what the build makes for one Android version: the Android major and the
    first 12 hex digits of a hash over the pins and every patch, so that any change to either
    names a different build."""
    h = hashlib.sha256(json.dumps(version, sort_keys=True).encode())
    for project, patch in patch_files(version["android"], patches):
        h.update(project.encode() + b"\0" + patch.name.encode() + b"\0")
        h.update(hashlib.sha256(patch.read_bytes()).digest())
    return f"{version['android_major']}-{h.hexdigest()[:12]}"


def manifest_commits(text):
    """Each project's path, name and revision in a build manifest."""
    root = ElementTree.fromstring(text)
    out = {}
    for project in root.iter("project"):
        path = project.get("path") or project.get("name")
        out[path] = (project.get("name"), project.get("revision"))
    return out


def manifest_problems(version, text):
    """What in `version` is not the published build's: a project the manifest does not have, at
    another commit, or from another repository."""
    if hashlib.sha256(text.encode() if isinstance(text, str) else text).hexdigest() != version["manifest"]["sha256"]:
        return [f"the manifest is not the one {version['android']} pins (SHA-256 differs)"]
    commits = manifest_commits(text)
    problems = []
    for project in version["projects"]:
        found = commits.get(project["path"])
        if found is None:
            problems.append(f"{project['path']} is not in build {version['build_id']}'s manifest")
            continue
        name, revision = found
        if project["url"] != GOOGLESOURCE + name:
            problems.append(f"{project['path']} comes from {project['url']}, not {GOOGLESOURCE + name}")
        if project["commit"] != revision:
            problems.append(f"{project['path']} is pinned to {project['commit'][:12]}, but build "
                            f"{version['build_id']} was made from {revision[:12]}")
    return problems


def main(argv):
    lock = load()
    if argv[1:] == ["versions"]:
        print(json.dumps([v["android"] for v in lock["versions"]]))
    elif len(argv) == 3 and argv[1] == "version":
        print(build_version(version_entry(lock, argv[2])))
    elif argv[1:] == ["outputs"]:
        print("\n".join(list(OUTPUTS.values()) + list(KEYMINT_OUTPUTS.values())))
    elif len(argv) == 4 and argv[1] == "check-manifest":
        problems = manifest_problems(version_entry(lock, argv[2]), Path(argv[3]).read_bytes())
        for problem in problems:
            print(problem, file=sys.stderr)
        if problems:
            raise SystemExit(1)
        print(f"{argv[2]}: every pinned commit is the published build's")
    else:
        raise SystemExit("usage: framework_lock.py versions | version <android> | outputs | "
                         "check-manifest <android> <manifest.xml>")


if __name__ == "__main__":
    main(sys.argv)
