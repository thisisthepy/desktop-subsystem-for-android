"""Makes a DSA guest image from the published AOSP build inputs of one Android version.

    uv run dsa/guest/image/make-image.py --major 17 --keys .scratch/image-keys

It takes the pinned target_files and otatools (fetch.sh downloads them) and:

1. removes the launcher, SystemUI and the keyboard app, because the host is the shell;
2. switches the build to user (not debuggable, adb authorised only);
3. drops the published kernel modules (they belong to Google's kernel, and DSA ships its
   own kernel with the drivers built in) and installs the Hyper-V mount table;
4. re-signs every APK, APEX and verified boot image with the keys in --keys, because the
   published build is signed with AOSP's test keys, whose private halves are public;
5. builds the images, and lays them out on the template of a user's Android disk.

The results go to dsa/target/image/<version>/:

- disk.vhdx: a new user's Android disk (dynamic, so it holds only what is written): the
  super partition with Android itself, its verified boot data (the vbmeta images, and the
  boot images verified boot checks: boot, init_boot, vendor_boot), and empty misc, metadata
  and userdata partitions that Android formats on first boot;
- initrd.img: the vendor and generic ramdisks, which the VM boots with DSA's kernel;
- image.json: the version, the kernel command line the VM needs, and the files' SHA-256.
"""

import argparse
import gzip
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]

# The host is the shell: no launcher, no SystemUI, no keyboard app in the guest.
REMOVED_APPS = ["Launcher3QuickStep", "SystemUI", "LatinIME"]

# The user's disk. The super partition's UUID is how first-stage init finds the disk, so it
# is fixed and passed on the kernel command line.
SUPER_UUID = "6b1ad5ad-0d5a-4dd3-9a6b-5a0a1d5a5a17"
MISC_UUID = "811d3bbb-a7ff-437a-857f-f54af996b190"
DISK_SIZE = 64 << 30
SYSTEM_SMALL_PARTITIONS = [
    ("misc", 1 << 20),
    ("vbmeta_a", 1 << 20),
    ("vbmeta_system_a", 1 << 20),
    ("vbmeta_system_dlkm_a", 1 << 20),
    ("vbmeta_vendor_dlkm_a", 1 << 20),
    # Verified boot reads these: the top-level vbmeta chains to boot and init_boot and
    # holds the hash of vendor_boot, so without them first stage init cannot set up
    # verity and does not mount /system. The VM boots DSA's kernel and initrd, not these.
    # AVB finds the footer at the end of each partition, so these must match
    # the partition sizes used when sign_target_files_apks signs the images.
    ("boot_a", 64 << 20),
    ("init_boot_a", 8 << 20),
    ("vendor_boot_a", 64 << 20),
]
SMALL_PARTITIONS = SYSTEM_SMALL_PARTITIONS

DATA_PARTITIONS = [
    ("metadata", 64 << 20),
    ("userdata", 0),
]

# What is written to each partition, from the signed build's IMAGES/.
PARTITION_IMAGES = {
    "vbmeta_a": "vbmeta.img",
    "vbmeta_system_a": "vbmeta_system.img",
    "vbmeta_system_dlkm_a": "vbmeta_system_dlkm.img",
    "vbmeta_vendor_dlkm_a": "vbmeta_vendor_dlkm.img",
    "boot_a": "boot.img",
    "init_boot_a": "init_boot.img",
    "vendor_boot_a": "vendor_boot.img",
}

USER_PROPS = {
    "ro.debuggable": "0",
    "ro.adb.secure": "1",
    "ro.secure": "1",
    "ro.force.debuggable": "0",
}

# Cuttlefish ships selectable vendor APEX implementations. Its launcher passes
# the selections in bootconfig. DSA does not run that launcher, so choose the
# implementations that can start without Cuttlefish remote services before
# apexd scans the vendor APEX directory.
VENDOR_APEX_BOOTCONFIG = {
    "com.android.hardware.gatekeeper": "com.android.hardware.gatekeeper.nonsecure",
    "com.android.hardware.keymint": "com.android.hardware.keymint.rust_nonsecure",
    "com.android.hardware.graphics.composer": "com.android.hardware.graphics.composer.ranchu",
    "com.google.emulated.camera.provider.hal": "com.google.emulated.camera.provider.hal",
}


def run(*cmd, **kw):
    print("+", " ".join(str(c) for c in cmd), flush=True)
    subprocess.run([str(c) for c in cmd], check=True, **kw)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def is_wsl_windows_mount(parts):
    """Whether resolved path parts name a WSL mount of a Windows drive."""
    return len(parts) >= 3 and parts[1] == "mnt"


def use_windows_sparse_file(path, windows=None, wsl_distro=None):
    """Whether `path` is on Windows' NTFS, where WSL ftruncate allocates the whole file."""
    windows = os.name == "nt" if windows is None else windows
    wsl_distro = os.environ.get("WSL_DISTRO_NAME") if wsl_distro is None else wsl_distro
    return windows or (bool(wsl_distro) and is_wsl_windows_mount(path.resolve().parts))


def windows_path(path):
    """The Windows spelling of `path`, from either Windows or WSL."""
    if os.name == "nt":
        return str(path.resolve())
    return subprocess.run(
        ["wslpath", "-w", path], check=True, capture_output=True, text=True
    ).stdout.strip()


def make_raw_disk(path, size):
    """Makes a zero-filled virtual disk without reserving its full size on NTFS."""
    path.unlink(missing_ok=True)
    if use_windows_sparse_file(path):
        path_text = windows_path(path).replace("'", "''")
        command = (
            f"$path = '{path_text}'; New-Item -ItemType File -Force -Path $path | Out-Null; "
            f"fsutil sparse setflag $path; fsutil file seteof $path {size}"
        )
        run("powershell.exe", "-NoProfile", "-Command", command)
    else:
        with open(path, "wb") as f:
            f.truncate(size)


def materialize_backup_gpt(path):
    """Writes the backup GPT through Windows on a Windows sparse file.

    DrvFs can leave the final sparse range unallocated after `sgdisk` writes the backup
    GPT. qemu-img then converts it as zeroes, and the guest loses GPT partition names.
    A GPT with 128 128-byte entries has a 32-sector entry array and one header sector.
    """
    if not use_windows_sparse_file(path):
        return
    path_text = windows_path(path).replace("'", "''")
    command = (
        f"$path = '{path_text}'; $stream = [System.IO.File]::Open($path, 'Open', 'ReadWrite', 'None'); "
        "try { $bytes = New-Object byte[] 16896; $start = $stream.Length - $bytes.Length; "
        "$stream.Position = $start; $read = $stream.Read($bytes, 0, $bytes.Length); "
        "if ($read -ne $bytes.Length) { throw 'the raw disk has no complete backup GPT' }; "
        "$stream.Position = $start; $stream.Write($bytes, 0, $bytes.Length) } finally { $stream.Dispose() }"
    )
    run("powershell.exe", "-NoProfile", "-Command", command)


def command_line_partitions(partitions, uuids=None):
    """Returns the Hyper-V SCSI layout that restores GPT names in early Android init.

    Hyper-V's virtual SCSI controller exposes the GPT partition devices but omits their
    PARTNAME uevent property. The kernel's command-line partition parser supplies those
    names before first-stage init starts. Every range comes from the GPT we just wrote, so
    an AOSP super image changing size does not change the mapping by assumption.
    """
    entries = []
    uuids = {} if uuids is None else uuids
    for name, (offset, size) in partitions.items():
        identifier = f"{name}:{uuids[name]}" if name in uuids else name
        entries.append(f"{size}@{offset}({identifier})")
    return "blkdevparts=sda:" + ",".join(entries)


def removed(name):
    """Whether a target_files member belongs to an app the guest must not have."""
    return any(f"/{app}/" in name for app in REMOVED_APPS)


def edit_props(text):
    out = []
    for line in text.splitlines():
        key = line.split("=", 1)[0]
        if key in USER_PROPS:
            continue
        if key.endswith("build.type"):
            line = f"{key}=user"
        out.append(line)
    out += [f"{k}={v}" for k, v in USER_PROPS.items()]
    return "\n".join(out) + "\n"


PROP_FILES = {
    "SYSTEM/build.prop",
    "VENDOR/build.prop",
    "PRODUCT/etc/build.prop",
    "SYSTEM_EXT/etc/build.prop",
    "ODM/etc/build.prop",
    "VENDOR_BOOT/RAMDISK/prop.default",
}

FSTAB_DIRS = {
    "VENDOR/etc/": "META/vendor_filesystem_config.txt",
    "VENDOR_BOOT/RAMDISK/first_stage_ramdisk/system/etc/": "META/vendor_boot_filesystem_config.txt",
}

# The transport library stays in vendor with the rest of the Cuttlefish graphics stack.
# Supplying it at image-build time keeps the signed target_files archive as the single
# source from which AVB-protected partitions are generated.
GFXSTREAM_LIBRARY = "VENDOR/lib64/libOpenglSystemCommon.so"


def modify(source, dest, graphics_library=None):
    """Copies target_files, removing the shell apps and the published kernel modules,
    switching to a user build and adding the Hyper-V mount table."""
    fstab = (HERE / "fstab.dsa").read_bytes()
    graphics_payload = None
    if graphics_library:
        graphics_payload = Path(graphics_library).read_bytes()
    replaced_graphics_payload = False
    kept_modules = re.compile(r"^(VENDOR_BOOT/RAMDISK|VENDOR_DLKM|SYSTEM_DLKM)/lib/modules/.*\.ko$")
    with zipfile.ZipFile(source) as zin, zipfile.ZipFile(dest, "w", zipfile.ZIP_STORED, allowZip64=True) as zout:
        for info in zin.infolist():
            name = info.filename
            if removed(name) or kept_modules.match(name):
                continue
            if name.startswith("IMAGES/"):
                # Rebuilt after signing from the modified contents.
                continue
            data = graphics_payload if name == GFXSTREAM_LIBRARY and graphics_payload is not None else zin.read(name)
            if name == GFXSTREAM_LIBRARY and graphics_payload is not None:
                replaced_graphics_payload = True
            if name in PROP_FILES:
                data = edit_props(data.decode()).encode()
            elif name.endswith("lib/modules/modules.load") or name.endswith("lib/modules/modules.dep"):
                data = b""
            elif name == "META/apkcerts.txt":
                data = b"".join(
                    line + b"\n" for line in data.splitlines() if not any(f'name="{a}.apk"' in line.decode() for a in REMOVED_APPS)
                )
            elif name in FSTAB_DIRS.values():
                text = data.decode()
                for directory, config in FSTAB_DIRS.items():
                    if config == name:
                        # vendor/etc/... for the partition, first_stage_ramdisk/... for the ramdisk.
                        if name.startswith("META/vendor_boot"):
                            prefix = directory.split("RAMDISK/", 1)[1]
                        else:
                            prefix = directory.lower()
                        text += f"{prefix}fstab.dsa 0 0 644 capabilities=0x0\n"
                data = text.encode()
            info.compress_type = zipfile.ZIP_STORED
            zout.writestr(info, data)
        for directory in FSTAB_DIRS:
            zout.writestr(directory + "fstab.dsa", fstab)
    if graphics_payload is not None and not replaced_graphics_payload:
        dest.unlink(missing_ok=True)
        raise ValueError(f"target_files does not contain {GFXSTREAM_LIBRARY}")


def key_arguments(target_files, keys):
    """The sign_target_files_apks options that put every certificate and key on ours."""
    args = ["-d", keys]
    with zipfile.ZipFile(target_files) as z:
        certs = set()
        for line in z.read("META/apkcerts.txt").decode().splitlines():
            m = re.search(r'certificate="([^"]+)"', line)
            if m and m.group(1) not in ("PRESIGNED", ""):
                certs.add(m.group(1))
        apexes = z.read("META/apexkeys.txt").decode().splitlines()
    standard = {"testkey", "platform", "shared", "media", "networkstack", "sdk_sandbox"}
    for cert in sorted(certs):
        base = cert[: -len(".x509.pem")]
        stem = os.path.basename(base)
        if base.startswith("build/make/target/product/security/") and stem in standard:
            continue
        own = {"com.android.bluetooth": "bluetooth", "nfc": "nfc"}.get(stem, "releasekey")
        args += ["-k", f"{base}={keys}/{own}"]
    for line in apexes:
        fields = dict(re.findall(r'(\w+)="([^"]*)"', line))
        if not fields or fields.get("private_key") == "PRESIGNED":
            continue
        # APEX containers are signed by file name (-e); the payload has its own key.
        args += ["-e", f"{fields['name']}={keys}/apexcontainer"]
        args += ["--extra_apex_payload_key", f"{fields['name']}={keys}/apex.pem"]
    avb = f"{keys}/avb.pem"
    for part in ["vbmeta", "vbmeta_system", "vbmeta_vendor", "boot", "init_boot"]:
        args += [f"--avb_{part}_key", avb, f"--avb_{part}_algorithm", "SHA256_RSA4096"]
    for part in ["vendor_boot", "vbmeta_system_dlkm", "vbmeta_vendor_dlkm"]:
        args += ["--avb_extra_custom_image_key", f"{part}={avb}"]
        args += ["--avb_extra_custom_image_algorithm", f"{part}=SHA256_RSA4096"]
    return args


def extract_tree(z, prefix, dest):
    """Writes the entries under prefix to dest as the tree they describe.

    zipfile.extract would turn symbolic links into files holding their target, and the
    ramdisk would lose its empty directories (proc, sys, dev...), which first stage init
    mounts onto: without them it stops and the kernel panics.
    """
    shutil.rmtree(dest, ignore_errors=True)
    dest.mkdir(parents=True)
    for info in z.infolist():
        if not info.filename.startswith(prefix) or info.filename == prefix:
            continue
        path = dest / info.filename[len(prefix):]
        mode = info.external_attr >> 16
        if info.filename.endswith("/"):
            path.mkdir(parents=True, exist_ok=True)
        elif stat.S_ISLNK(mode):
            path.parent.mkdir(parents=True, exist_ok=True)
            os.symlink(z.read(info).decode(), path)
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(z.read(info))


def ramdisk(signed, otatools, work, out):
    """The vendor ramdisk followed by the generic one, as one gzip'd initrd."""
    parts = []
    with zipfile.ZipFile(signed) as z:
        for prefix, config in [("VENDOR_BOOT/RAMDISK/", "META/vendor_boot_filesystem_config.txt"), ("INIT_BOOT/RAMDISK/", "META/init_boot_filesystem_config.txt")]:
            root = work / prefix.split("/")[0].lower()
            shutil.rmtree(root, ignore_errors=True)
            src = work / "extract" / prefix
            extract_tree(z, prefix, src)
            src.mkdir(parents=True, exist_ok=True)
            cfg = work / f"{root.name}.fs_config"
            cfg.write_bytes(z.read(config))
            cpio = subprocess.run([otatools / "bin/mkbootfs", "-f", cfg, src], check=True, capture_output=True).stdout
            parts.append(gzip.compress(cpio, 9))
    out.write_bytes(b"".join(parts))


def verified_boot_partitions(vbmeta, otatools):
    """The partitions verified boot reads from the disk itself: those the top-level vbmeta
    chains to or holds a whole-partition hash of. Hashtree partitions live in super."""
    # avbtool is a native launcher in otatools, run as it is.
    result = subprocess.run([otatools / "bin/avbtool", "info_image", "--image", vbmeta],
                            capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"avbtool could not read {vbmeta.name}: {result.stderr.strip()}")
    info = result.stdout
    found, kind = [], None
    for line in info.splitlines():
        line = line.strip()
        if line.endswith("descriptor:"):
            kind = line
        elif line.startswith("Partition Name:") and kind in ("Chain Partition descriptor:", "Hash descriptor:"):
            found.append((kind.split()[0].lower(), line.split(":", 1)[1].strip()))
    return found


def unlisted_verified_boot_partitions(found, fstab):
    """The verified boot partitions outside vbmeta that lack a first-stage fstab entry.

    First-stage init creates by-name links only for partitions it is told it needs. AVB
    reads partitions a hash descriptor covers, so an omitted entry makes verification fail
    before /system can mount. Chained vbmeta images are resolved by the AVB loader.
    """
    listed = set()
    for line in fstab.splitlines():
        fields = line.split()
        if len(fields) >= 5 and not line.startswith("#") and "first_stage_mount" in fields[4].split(","):
            listed.add(fields[0].rsplit("/", 1)[-1])
    return [name for _, name in found if name not in listed]


def system_disk(images, otatools, work, out):
    """Lays the system images out on the shared read-only system disk and converts it to a dynamic VHDX."""
    super_raw = work / "super.raw"
    super_img = images / "super.img"
    with open(super_img, "rb") as f:
        sparse = f.read(4) == b"\x3a\xff\x26\xed"
    if sparse:
        run(otatools / "bin/simg2img", super_img, super_raw)
    else:
        shutil.copy(super_img, super_raw)
    raw = work / "system.raw"
    needed = sum(size for _, size in SYSTEM_SMALL_PARTITIONS) + super_raw.stat().st_size + (128 << 20)
    system_disk_size = ((needed + (1 << 20) - 1) // (1 << 20)) * (1 << 20)
    make_raw_disk(raw, system_disk_size)
    on_disk = {name for name, _ in SYSTEM_SMALL_PARTITIONS}
    found = verified_boot_partitions(images / "vbmeta.img", otatools)
    missing = [n for _, n in found if f"{n}_a" not in on_disk]
    if missing:
        sys.exit(f"verified boot reads {', '.join(missing)}, which the disk layout has no partition for")
    unlisted = unlisted_verified_boot_partitions(found, (HERE / "fstab.dsa").read_text())
    if unlisted:
        sys.exit(f"verified boot reads {', '.join(unlisted)}, which fstab.dsa does not list for first stage mount")
    sgdisk = shutil.which("sgdisk") or str(otatools / "bin/sgdisk")
    cmd = [sgdisk, "--clear"]
    number = 1
    for name, size in SYSTEM_SMALL_PARTITIONS:
        cmd += [f"--new={number}:0:+{size // 1024}K", f"--change-name={number}:{name}"]
        if name == "misc":
            cmd += [f"--partition-guid={number}:{MISC_UUID}"]
        number += 1
    cmd += [f"--new={number}:0:+{super_raw.stat().st_size // 1024}K", f"--change-name={number}:super", f"--partition-guid={number}:{SUPER_UUID}", str(raw)]
    run(*cmd)
    materialize_backup_gpt(raw)
    layout = subprocess.run([sgdisk, "--print", raw], check=True, capture_output=True, text=True).stdout
    partitions = {}
    for line in layout.splitlines():
        m = re.match(r"\s*(\d+)\s+(\d+)\s+(\d+)\s+\S+\s+\S+\s+\S+\s+(\S+)", line)
        if m:
            first, last = int(m.group(2)), int(m.group(3))
            partitions[m.group(4)] = (first * 512, (last - first + 1) * 512)
    print(layout)
    sizes = dict(SYSTEM_SMALL_PARTITIONS)
    writes = [("super", super_raw)] + [(name, images / file) for name, file in PARTITION_IMAGES.items()]
    for name, image in writes:
        if not image.exists():
            continue
        if name in sizes and image.stat().st_size > sizes[name]:
            sys.exit(f"{image.name} is {image.stat().st_size} bytes, more than the {name} partition holds")
        run("dd", f"if={image}", f"of={raw}", "bs=1M", f"seek={partitions[name][0]}", "oflag=seek_bytes", "conv=notrunc,sparse", "status=none")
    super_raw.unlink()
    out.unlink(missing_ok=True)
    run(shutil.which("qemu-img") or "qemu-img", "convert", "-f", "raw", "-O", "vhdx", "-o", "subformat=dynamic", "-S", "4k", raw, out)
    raw.unlink()
    return command_line_partitions(partitions, {"misc": MISC_UUID})


def data_disk(otatools, work, out):
    """Emits an empty dynamic data disk template containing metadata and userdata partitions."""
    raw = work / "data.raw"
    make_raw_disk(raw, DISK_SIZE)
    sgdisk = shutil.which("sgdisk") or str(otatools / "bin/sgdisk")
    cmd = [
        sgdisk, "--clear",
        "--new=1:0:+64M", "--change-name=1:metadata",
        "--new=2:0:0", "--change-name=2:userdata",
        str(raw),
    ]
    run(*cmd)
    materialize_backup_gpt(raw)
    out.unlink(missing_ok=True)
    run(shutil.which("qemu-img") or "qemu-img", "convert", "-f", "raw", "-O", "vhdx", "-o", "subformat=dynamic", "-S", "4k", raw, out)
    raw.unlink()


def kernel_cmdline(partition_cmdline):
    """Returns the kernel command line shared by every DSA guest image."""
    return " ".join([
        "console=ttyS0", "printk.devkmsg=on", "audit=1", "panic=-1", "8250.nr_uarts=1",
        "firmware_class.path=/vendor/etc/", "loop.max_part=7", "init=/init", partition_cmdline,
        "androidboot.boot_devices=vmbus", "androidboot.fstab_suffix=dsa",
        "androidboot.slot_suffix=_a", "androidboot.force_normal_boot=1",
        "androidboot.hardware=cutf_cvm", "androidboot.verifiedbootstate=orange",
        "androidboot.vbmeta.device_state=unlocked",
        *(f"androidboot.vendor.apex.{module}={apex}"
          for module, apex in VENDOR_APEX_BOOTCONFIG.items()),
    ])


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--major", type=int, required=True)
    parser.add_argument("--keys", type=Path, required=True)
    parser.add_argument("--inputs", type=Path, default=REPO / ".scratch/aosp-inputs")
    parser.add_argument("--out", type=Path, default=REPO / "dsa/target/image")
    parser.add_argument("--kernel", type=Path, help="the guest kernel (bzImage) the image boots with; "
                        "without it the host's own kernel is used")
    parser.add_argument("--graphics-library", type=Path,
                        help="a libOpenglSystemCommon.so built for the selected guest")
    args = parser.parse_args()

    spec = next(i for i in json.loads((HERE / "inputs.json").read_text())["images"] if i["android_major"] == args.major)
    inputs = args.inputs / spec["build_id"]
    target_files = inputs / spec["files"]["target_files"]["name"]
    keys = args.keys.resolve()
    out = args.out / spec["version"]
    work = args.out / f".work-{spec['version']}"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    out.mkdir(parents=True, exist_ok=True)

    otatools = work / "otatools"
    with zipfile.ZipFile(inputs / spec["files"]["otatools"]["name"]) as z:
        z.extractall(otatools)
    for f in (otatools / "bin").iterdir():
        f.chmod(0o755)
    # The tools unpack gigabytes into their temporary folder; keep it inside the work folder.
    temp = work / "tmp"
    temp.mkdir()
    env = dict(os.environ, PATH=f"{otatools / 'bin'}:{os.environ['PATH']}", TMPDIR=str(temp))

    modified = work / "modified.zip"
    modify(target_files, modified, args.graphics_library)

    signed = work / "signed.zip"
    run(otatools / "bin/sign_target_files_apks", "-p", otatools, *key_arguments(modified, str(keys)), modified, signed, env=env)
    modified.unlink()
    run(otatools / "bin/add_img_to_target_files", "-p", otatools, "-a", signed, env=env)

    images = work / "images"
    with zipfile.ZipFile(signed) as z:
        for name in z.namelist():
            if name.startswith("IMAGES/") and name[len("IMAGES/"):] in {"super.img", *PARTITION_IMAGES.values()}:
                z.extract(name, work)
    (work / "IMAGES").rename(images)
    if not (images / "super.img").exists():
        run(otatools / "bin/build_super_image", "-p", otatools, signed, images / "super.img", env=env)

    notice_name = None
    with zipfile.ZipFile(signed) as z:
        for candidate in [
            "SYSTEM/etc/NOTICE.xml.gz",
            "SYSTEM/etc/NOTICE.html.gz",
            "META/NOTICE.xml.gz",
        ]:
            if candidate in z.namelist():
                notice_name = Path(candidate).name
                with open(out / notice_name, "wb") as f:
                    f.write(z.read(candidate))
                break

    ramdisk(signed, otatools, work, out / "initrd.img")
    partition_cmdline = system_disk(images, otatools, work, out / "system.vhdx")
    data_disk(otatools, work, out / "data.vhdx")
    files = ["system.vhdx", "data.vhdx", "initrd.img"]
    if notice_name:
        files.append(notice_name)
    (out / "bzImage").unlink(missing_ok=True)
    if args.kernel:
        shutil.copy(args.kernel, out / "bzImage")
        files.append("bzImage")

    cmdline = kernel_cmdline(partition_cmdline)
    manifest = {
        "version": spec["version"],
        "android_major": spec["android_major"],
        # Orders images of one Android version: a new AOSP build, or a new revision of
        # DSA's image of the same build (a fix to how the image is made), is newer.
        "build": int(spec["build_id"]) * 100 + spec.get("revision", 1),
        "source": {"branch": spec["branch"], "target": spec["target"], "build_id": spec["build_id"], "revision": spec.get("revision", 1), "security_patch": spec["security_patch"]},
        "kernel_cmdline": cmdline,
        "files": {name: {"size": (out / name).stat().st_size, "sha256": sha256(out / name)} for name in files},
    }
    (out / "image.json").write_text(json.dumps(manifest, indent=2) + "\n")
    shutil.copy(signed, out / "signed-target_files.zip")
    shutil.rmtree(work)
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    sys.exit(main())
