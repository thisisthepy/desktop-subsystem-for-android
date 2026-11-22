"""Reads and checks framework-lock.json, the pins the patched framework build is made from.

The build makes SurfaceFlinger, libhwui and libEGL with DSA's GPU device loss recovery,
libandroid_runtime, whose Process.getTotalMemory gives Google Play the computer's memory, and
DSA's KeyMint, whose root key of key blobs comes from the DSA host (the patches in
patches/<project>/<android>/). The image takes the first four in place of the published build's
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
    framework_lock.py java-outputs                 services.jar's and framework.jar's
                                                   target_files names and where the services
                                                   build installs them, one pair per line
    framework_lock.py services-lock <android> <manifest.xml> <out.json>
                                                   writes the lock of the services build's tree:
                                                   every project the manifest syncs by default
                                                   but SERVICES_TREE_SKIPPED
    framework_lock.py install-source               reads `ninja -t query <install path>` and
                                                   prints the jar the install path copies
    framework_lock.py soong-memory-limit <soong.go>
                                                   changes soong_ui's source in the services
                                                   tree so that GOMEMLIMIT reaches soong_build

services.jar, with DSA's window manager, package manager, WebView service, display manager
(which shows Google Play a phone's density, so that Play delivers xxhdpi splits) and activity
manager (which gives Google Play the computer's installed memory as advertisedMem), is built in a
tree of its own (SERVICES_JAR): the Java framework needs most of the platform. framework.jar, whose
views a task set to forced system scaling rebuilds at a new density, is built beside it
(FRAMEWORK_JAR).
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
    # The framework's JNI library, whose Process.getTotalMemory gives the apps the guest names
    # (Google Play) the computer's memory as ActivityManager's totalMem.
    "SYSTEM/lib64/libandroid_runtime.so": "system/lib64/libandroid_runtime.so",
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

# The window manager, package manager, WebView service, display manager and activity manager
# with DSA's changes (the patches to frameworks/base/services in
# patches/frameworks/base/<android>/): services.jar, which the image takes in place of the
# published build's. The Java framework reaches across most of the
# platform's projects, so it is not built in the reduced tree the native files are built in, but
# in a tree of every project the build's manifest syncs by default (services_lock), with the
# build's own product (the version's target) and release configuration. A dex file is the same
# for every architecture, so one services.jar, built with the x86_64 target, serves the images of
# both. The native build applies no patch that changes Java sources only (java_only), as it
# builds none.
#
# framework.jar, the app side of the framework, with DSA's change that rebuilds the views of a task
# set to forced system scaling at its new density (the patches to frameworks/base/core/java and
# graphics/java), is built in the same tree and taken with services.jar: the window manager in one
# calls the other's new code, so the image takes both from one build or neither. framework.jar is
# on the boot class path, so the image compiles the boot image again for it, and everything
# compiled against the boot image (make-image.py, BOOT_IMAGE_JARS).
SERVICES_JAR = "SYSTEM/framework/services.jar"
FRAMEWORK_JAR = "SYSTEM/framework/framework.jar"
JAVA_OUTPUTS = {
    SERVICES_JAR: "system/framework/services.jar",
    FRAMEWORK_JAR: "system/framework/framework.jar",
}
# The folders of frameworks/base that each file the image takes from the build is built from. A
# patch changes frameworks/base only there, as the image keeps the published build's copy of
# everything else. libandroid_runtime and framework.jar come from one Framework build, but from
# two of its jobs, so a native method libandroid_runtime registers for a Java method only the
# patched framework.jar declares would make zygote abort on an image whose services build failed:
# core/jni changes only what the published framework.jar declares.
FRAMEWORKS_BASE_SOURCES = {
    "SYSTEM/lib64/libhwui.so": ["libs/hwui"],
    "SYSTEM/lib64/libandroid_runtime.so": ["core/jni"],
    SERVICES_JAR: ["services"],
    FRAMEWORK_JAR: ["core/java", "graphics/java"],
}
# The projects the services tree takes in part, as the native tree takes them: each holds every
# toolchain version the platform ever used, and the build reads only the ones the lock names.
# Android 17 keeps its Rust toolchains in prebuilts/rust-toolchain/linux-x86 and leaves only
# Soong's build files in prebuilts/rust, so the native lock names a cone for the first only; a
# project here that the native lock does not name at all is taken whole.
SERVICES_TREE_CONES = ["prebuilts/clang/host/linux-x86", "prebuilts/rust",
                       "prebuilts/rust-toolchain/linux-x86"]
# A project the services tree takes in the part the native lock takes another: Android 17's
# Rust toolchains for musl hosts are laid out as the ones for glibc hosts are (a build file and
# one folder per version, 6 GB for three versions), and the native lock does not name them.
# Taking the build's version alone leaves the analysis every build file and the disk 4 GB more,
# of the 17 GB the fetch leaves it on a standard runner.
SERVICES_TREE_CONES_LIKE = {
    "prebuilts/rust-toolchain/linux-musl-x86": "prebuilts/rust-toolchain/linux-x86",
}
# Projects the services tree leaves out although the manifest syncs them: prebuilt native
# libraries and their ABI dumps (the build skips ABI checks) and emulator kernels and binaries,
# which only native builds and other devices read. Nothing on the way from the Java sources to
# services.jar's dex reads them, and the build allows missing dependencies, so the modules they
# define may go missing. The product's configuration checks only that each VNDK version it names
# has a build file, so the workflow puts an empty one in each. Together they are
# several gigabytes of the tree, which only just fits a standard runner's disk. A path ending in
# "/" leaves out every project below it.
SERVICES_TREE_SKIPPED = ["prebuilts/abi-dumps/", "prebuilts/vndk/", "prebuilts/qemu-kernel",
                         "prebuilts/android-emulator"]
# Where build/soong/ui/build/soong.go starts the environment soong_build runs with, and what
# soong_memory_limit adds below it. The line is the same in Android 16 and 17.
SOONG_UI_SOURCE = "build/soong/ui/build/soong.go"
SOONG_INVOCATION_ENV = "\tinvocationEnv := make(map[string]string)\n"
SOONG_MEMORY_LIMIT = (
    "\t// DSA: soong_build starts with an empty environment, so its Go memory limit is handed on.\n"
    '\tif limit := os.Getenv("GOMEMLIMIT"); limit != "" {\n'
    '\t\tinvocationEnv["GOMEMLIMIT"] = limit\n'
    "\t}\n"
)
# Projects in a manifest group repo leaves out of a default sync on Linux.
UNSYNCED_GROUPS = {"notdefault", "platform-darwin", "darwin"}

# Projects every version fetches: the build system, and the projects the patches change.
REQUIRED = ["build/make", "build/soong", "build/release", "build/blueprint",
            "frameworks/native", "frameworks/base", "hardware/interfaces", "system/keymint",
            "external/rust/crates/openssl", "external/boringssl"]
# The folders of a project fetched in part that the patches and the targets need.
REQUIRED_FOLDERS = {
    "frameworks/native": ["libs/renderengine", "services/surfaceflinger", "opengl", "libs/binder/rust"],
    "frameworks/base": ["libs/hwui", "core/jni"],
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


def patch_paths(patch):
    """The files a patch changes, from its diff headers, relative to its project."""
    paths = []
    for line in Path(patch).read_text().splitlines():
        for prefix in ("--- a/", "+++ b/"):
            if line.startswith(prefix) and line[len(prefix):] not in paths:
                paths.append(line[len(prefix):])
    return paths


def java_only(patch):
    """Whether a patch changes Java sources only: it then changes nothing the native build makes,
    which builds no Java, and only the services build applies it."""
    paths = patch_paths(patch)
    return bool(paths) and all(p.endswith(".java") for p in paths)


def manifest_projects(text):
    """Every project of a build manifest a default repo sync on Linux takes, as lock entries:
    path, repository, commit, and the manifest's linkfile and copyfile entries."""
    root = ElementTree.fromstring(text)
    projects = []
    for project in root.iter("project"):
        groups = {g.strip() for g in (project.get("groups") or "").split(",") if g.strip()}
        if groups & UNSYNCED_GROUPS and not groups & {"linux", "platform-linux"}:
            continue
        name = project.get("name")
        entry = {"path": project.get("path") or name, "url": GOOGLESOURCE + name,
                 "commit": project.get("revision")}
        for kind in ("linkfile", "copyfile"):
            found = [{"src": e.get("src"), "dest": e.get("dest")} for e in project.findall(kind)]
            if found:
                entry[kind + "s"] = found
        projects.append(entry)
    return projects


def services_skipped(path):
    """Whether the services tree leaves out the project at `path` (SERVICES_TREE_SKIPPED)."""
    return any(path.startswith(skip) if skip.endswith("/") else path == skip
               for skip in SERVICES_TREE_SKIPPED)


def services_lock(version, text):
    """The lock the services build fetches its tree by: `version` with every project its
    manifest (`text`, the one it pins by SHA-256) syncs by default but SERVICES_TREE_SKIPPED, each
    at the manifest's commit, and the toolchain prebuilts (SERVICES_TREE_CONES, and
    SERVICES_TREE_CONES_LIKE as the project each names) taken in part as `version` takes them."""
    data = text.encode() if isinstance(text, str) else text
    if hashlib.sha256(data).hexdigest() != version["manifest"]["sha256"]:
        raise LockError(f"the manifest is not the one {version['android']} pins (SHA-256 differs)")
    native = {p["path"]: p for p in version["projects"]}
    projects = []
    for project in manifest_projects(data):
        if services_skipped(project["path"]):
            continue
        like = SERVICES_TREE_CONES_LIKE.get(project["path"], project["path"])
        if (project["path"] in SERVICES_TREE_CONES or like != project["path"]) and like in native:
            if "cone" not in native[like]:
                raise LockError(f"{version['android']} does not say which part of {like} to take")
            project["cone"] = list(native[like]["cone"])
        projects.append(project)
    tree = {key: value for key, value in version.items() if key != "projects"}
    tree["projects"] = sorted(projects, key=lambda p: p["path"])
    return {
        "comment": f"Every project build {version['build_id']}'s manifest syncs by default but the "
                   "ones only native builds read, for the services build (framework_lock.py services-lock).",
        "kind": KIND,
        "versions": [tree],
    }


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


def install_source(query, suffix=".jar"):
    """The file an install path is a copy of, from what `ninja -t query <install path>` prints:
    the one explicit input of its rule, which must copy a file ending in `suffix` (a jar, or an
    app's APK). The services build builds that file, the module's own dex jar or APK, so that
    installing it does not also build what the module requires."""
    rule, inputs, section = None, [], None
    for line in query.splitlines():
        if line.startswith("  ") and not line.startswith("    "):
            key, _, value = line.strip().partition(":")
            section = key
            if key == "input":
                rule = value.strip()
        elif section == "input" and line.startswith("    "):
            path = line.strip()
            if not path.startswith("|"):
                inputs.append(path)
    if rule is None:
        raise LockError("ninja names no rule that makes the install path")
    if not re.search(r"cp|copy|install", rule, re.IGNORECASE):
        raise LockError(f"the install path is made by {rule}, which is not a copy, so the module's "
                        "own output cannot stand in for it")
    if len(inputs) != 1 or not inputs[0].endswith(suffix):
        kind = "jar" if suffix == ".jar" else suffix.lstrip(".").upper()
        raise LockError(f"the install path is a copy of {', '.join(inputs) or 'nothing'}, not of one {kind}")
    return inputs[0]


def soong_memory_limit(text):
    """soong_ui's ui/build/soong.go (`text`), changed to hand GOMEMLIMIT on to soong_build.

    soong_ui starts soong_build, the analysis of every Android.bp, through a ninja rule that
    clears the environment (`env -i $env "$BUILDER"`), where $env holds only what soong.go puts
    in invocationEnv. So a GOMEMLIMIT exported to the build never reaches the one process it is
    meant for, and that process's heap grows to twice what is live before Go collects it. The
    change copies the variable into invocationEnv. Only this one: the rule writes the map in
    Go's map order, and a command that differed between two runs would make ninja analyse the
    tree again.

    Raises LockError when the file is not the one this was written against. A file already
    changed comes back as it is."""
    if SOONG_MEMORY_LIMIT in text:
        return text
    if text.count(SOONG_INVOCATION_ENV) != 1 or '\t"os"\n' not in text:
        raise LockError("soong.go does not make soong_build's environment where it did "
                        f"({SOONG_INVOCATION_ENV.strip()!r}, once, with os imported), so "
                        "GOMEMLIMIT cannot be handed on to the analysis")
    return text.replace(SOONG_INVOCATION_ENV, SOONG_INVOCATION_ENV + SOONG_MEMORY_LIMIT)


def main(argv):
    if argv[1:] in (["install-source"], ["install-source", "--apk"]):
        try:
            print(install_source(sys.stdin.read(), ".apk" if argv[2:] == ["--apk"] else ".jar"))
        except LockError as error:
            raise SystemExit(str(error))
        return
    if len(argv) == 3 and argv[1] == "soong-memory-limit":
        path = Path(argv[2])
        try:
            path.write_text(soong_memory_limit(path.read_text()))
        except LockError as error:
            raise SystemExit(str(error))
        print(f"{path}: soong_build takes GOMEMLIMIT from the build's environment")
        return
    lock = load()
    if argv[1:] == ["versions"]:
        print(json.dumps([v["android"] for v in lock["versions"]]))
    elif len(argv) == 3 and argv[1] == "version":
        print(build_version(version_entry(lock, argv[2])))
    elif argv[1:] == ["outputs"]:
        print("\n".join(list(OUTPUTS.values()) + list(KEYMINT_OUTPUTS.values())))
    elif argv[1:] == ["java-outputs"]:
        print("\n".join(f"{name} {installed}" for name, installed in JAVA_OUTPUTS.items()))
    elif len(argv) == 5 and argv[1] == "services-lock":
        tree = services_lock(version_entry(lock, argv[2]), Path(argv[3]).read_bytes())
        check(tree)
        Path(argv[4]).write_text(json.dumps(tree, indent=2) + "\n")
        print(f"{argv[2]}: {len(tree['versions'][0]['projects'])} projects for the services build")
    elif len(argv) == 4 and argv[1] == "check-manifest":
        problems = manifest_problems(version_entry(lock, argv[2]), Path(argv[3]).read_bytes())
        for problem in problems:
            print(problem, file=sys.stderr)
        if problems:
            raise SystemExit(1)
        print(f"{argv[2]}: every pinned commit is the published build's")
    else:
        raise SystemExit("usage: framework_lock.py versions | version <android> | outputs | java-outputs | "
                         "check-manifest <android> <manifest.xml> | "
                         "services-lock <android> <manifest.xml> <out.json> | install-source | "
                         "soong-memory-limit <soong.go>")


if __name__ == "__main__":
    main(sys.argv)
