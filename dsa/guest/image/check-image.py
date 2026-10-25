"""Checks a guest image made by make-image.py.

    uv run dsa/guest/image/check-image.py dsa/target/image/17.0.0-16373615 .scratch/image-keys

It fails when the shell apps are still there, when the build is still debuggable, when an
app is not signed with one of the keys in the key folder, when the agent is missing or not
signed with the platform key, when the files do not match image.json, or when the ramdisk
lacks the directories and links Android's first stage init needs.

The signatures are read from the apps themselves. sign_target_files_apks copies
META/apkcerts.txt over unchanged, so that file still names AOSP's keys after signing.
"""

import base64
import gzip
import io
import shutil
import stat

import hashlib
import json
import re
import subprocess
import sys
import zipfile
from pathlib import Path

image = Path(sys.argv[1])
keys = Path(sys.argv[2])
ours = [base64.b64decode("".join(l for l in p.read_text().splitlines() if "-----" not in l))
        for p in sorted(keys.glob("*.x509.pem"))]
manifest = json.loads((image / "image.json").read_text())
problems = []

for name, expected in manifest["files"].items():
    h = hashlib.sha256()
    with open(image / name, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    if h.hexdigest() != expected["sha256"]:
        problems.append(f"{name} does not match image.json")

with zipfile.ZipFile(image / "signed-target_files.zip") as z:
    names = z.namelist()
    for app in ["Launcher3QuickStep", "SystemUI", "LatinIME"]:
        if any(f"/{app}/" in n and n.endswith(".apk") for n in names):
            problems.append(f"{app} is still in the image")
    props = z.read("SYSTEM/build.prop").decode()
    if "ro.debuggable=1" in props or "ro.build.type=userdebug" in props:
        problems.append("the system is still a debuggable build")
    if "test-keys" in props:
        problems.append("the build still says test-keys")
    presigned = set(re.findall(r'name="([^"]+)" certificate="PRESIGNED"', z.read("META/apkcerts.txt").decode()))
    unsigned = []
    apks = [n for n in names if n.endswith(".apk") and n.rsplit("/", 1)[-1] not in presigned]
    for name in apks:
        data = z.read(name)
        # v2 and v3 signatures keep the certificate uncompressed in the signing block;
        # a v1-only app keeps it in a compressed META-INF entry.
        if any(cert in data for cert in ours):
            continue
        with zipfile.ZipFile(io.BytesIO(data)) as apk:
            blocks = [apk.read(n) for n in apk.namelist()
                      if re.fullmatch(r"META-INF/[^/]+\.(RSA|DSA|EC)", n)]
        if not any(cert in b for b in blocks for cert in ours):
            unsigned.append(name)
    print(f"{len(apks) - len(unsigned)} of {len(apks)} apps are signed with our keys")
    if unsigned:
        problems.append(f"{len(unsigned)} apps are not signed with our keys, e.g. {unsigned[:3]}")
    # The platform apps get their SELinux domain from the platform signer here. Left with
    # AOSP's certificate, they get seinfo "default", and zygote aborts starting them.
    # Bluetooth's comes the same way, from its own signer.
    if "SYSTEM/etc/selinux/plat_mac_permissions.xml" in names:
        mac_permissions = z.read("SYSTEM/etc/selinux/plat_mac_permissions.xml").decode().lower()
        for role in ["platform", "bluetooth"]:
            pem = keys / f"{role}.x509.pem"
            if not pem.exists() or f'<seinfo value="{role}"' not in mac_permissions:
                continue
            cert = base64.b64decode(
                "".join(l for l in pem.read_text().splitlines() if "-----" not in l)
            ).hex()
            if f'signature="{cert}"' not in mac_permissions:
                problems.append(f"plat_mac_permissions.xml does not name our {role} certificate")
    if "VENDOR/etc/fstab.dsa" not in names:
        problems.append("the Hyper-V mount table is missing")
    if "SYSTEM/etc/selinux/plat_sepolicy.cil" not in names:
        problems.append("plat_sepolicy.cil is missing from the image")
    else:
        plat_sepolicy = z.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        if not re.search(r"\(allow\s+appdomain\s+self\s+\(\s*vsock_socket\s*\(.*connect", plat_sepolicy):
            problems.append("the SELinux policy does not allow appdomain to connect vsock_socket")

    if "VENDOR/lib64/hw/vulkan.ranchu.so" not in names:
        problems.append("VENDOR/lib64/hw/vulkan.ranchu.so is missing from the image")
    if "VENDOR/lib64/libOpenglSystemCommon.so" not in names:
        problems.append("VENDOR/lib64/libOpenglSystemCommon.so is missing from the image")

    # The control channel daemon, which makes the session active, and its own domain: the
    # only one that may read the boot's secret.
    if "VENDOR/bin/dsa-channeld" not in names:
        problems.append("VENDOR/bin/dsa-channeld is missing from the image")
    if "VENDOR/etc/init/dsa-channeld.rc" not in names:
        problems.append("VENDOR/etc/init/dsa-channeld.rc is missing, so init never starts the daemon")
    if "SYSTEM/etc/selinux/plat_sepolicy.cil" in names and "dsa_channeld" not in z.read(
            "SYSTEM/etc/selinux/plat_sepolicy.cil").decode():
        problems.append("plat_sepolicy.cil has no dsa_channeld domain, so init cannot start the daemon")
    if "META/file_contexts.bin" in names and b"dsa_channeld_exec" not in z.read("META/file_contexts.bin"):
        problems.append("META/file_contexts.bin does not label dsa-channeld, so the vendor image leaves it "
                        "vendor_file and init refuses to start it")

    # The binder service the agent and the hwcomposer reach the daemon through.
    # servicemanager refuses it unless the vendor manifest declares it, and checks who may
    # add and find it against the label vendor_service_contexts gives it.
    channel = "io.github.thisisthepy.dsa.channel.IChannel/default"
    if "VENDOR/etc/vintf/manifest/dsa-channel.xml" not in names:
        problems.append("VENDOR/etc/vintf/manifest/dsa-channel.xml is missing, so servicemanager refuses "
                        "the daemon's binder service")
    elif "<fqname>IChannel/default</fqname>" not in z.read("VENDOR/etc/vintf/manifest/dsa-channel.xml").decode():
        problems.append("dsa-channel.xml does not declare IChannel/default")
    service_contexts = (z.read("VENDOR/etc/selinux/vendor_service_contexts").decode()
                        if "VENDOR/etc/selinux/vendor_service_contexts" in names else "")
    if f"{channel} u:object_r:dsa_channel_service:s0" not in service_contexts.splitlines():
        problems.append(f"vendor_service_contexts does not label {channel} dsa_channel_service, so "
                        "the agent and the hwcomposer cannot find the daemon")
    if "SYSTEM/etc/selinux/plat_sepolicy.cil" in names:
        plat_sepolicy = z.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        if "(allow dsa_channeld dsa_channel_service (service_manager (add" not in plat_sepolicy:
            problems.append("plat_sepolicy.cil does not let dsa_channeld register its binder service")
        for client in ["system_app", "platform_app", "hal_graphics_composer_server"]:
            if f"(allow {client} dsa_channel_service (service_manager (find)))" not in plat_sepolicy:
                problems.append(f"plat_sepolicy.cil does not let {client} find the daemon's binder service")
    # The agent, which tells the host about each app task, so each gets its own window. It
    # uses framework interfaces only a platform app may, so it must carry our platform
    # certificate, and the framework refuses to boot without its privileged permission list.
    agent = "SYSTEM_EXT/priv-app/DsaAgent/DsaAgent.apk"
    if agent not in names:
        problems.append(f"{agent} is missing from the image, so the host never learns of a task")
    else:
        platform_pem = keys / "platform.x509.pem"
        if platform_pem.exists():
            platform_cert = base64.b64decode(
                "".join(l for l in platform_pem.read_text().splitlines() if "-----" not in l)
            )
            if platform_cert not in z.read(agent):
                problems.append("DsaAgent.apk is not signed with our platform key, so the framework "
                                "refuses the agent the interfaces it needs")
    if "SYSTEM_EXT/etc/permissions/privapp-permissions-dsa-agent.xml" not in names:
        problems.append("SYSTEM_EXT/etc/permissions/privapp-permissions-dsa-agent.xml is missing, so the "
                        "framework refuses to boot with the agent")

    if "VENDOR/etc/selinux/vendor_file_contexts" in names:
        vendor_fc = z.read("VENDOR/etc/selinux/vendor_file_contexts").decode()
        for expected in ["vulkan", "libOpenglSystemCommon"]:
            if not any(expected in line and "same_process_hal_file" in line for line in vendor_fc.splitlines()):
                problems.append(f"vendor_file_contexts does not label {expected} as same_process_hal_file")

    vendor_props = z.read("VENDOR/build.prop").decode() if "VENDOR/build.prop" in names else ""
    for prop, val in [
        ("ro.hardware.egl", "angle"),
        ("ro.hardware.vulkan", "ranchu"),
        ("ro.hardware.gralloc", "minigbm"),
        ("ro.cpuvulkan.version", "0"),
    ]:
        if f"{prop}={val}" not in props and f"{prop}={val}" not in vendor_props:
            problems.append(f"neither SYSTEM nor VENDOR build.prop sets {prop}={val}")

    # With a value, the loader would take Cuttlefish's APEX driver instead of the vendor
    # one. The APEX's init script sets it unless a build.prop has claimed it empty first.
    for name in sorted(n for n in names if n.endswith("build.prop") or n.endswith("prop.default")):
        for line in z.read(name).decode(errors="replace").splitlines():
            if line.startswith("ro.vulkan.apex=") and line != "ro.vulkan.apex=":
                problems.append(f"{name} sets {line}, so the Vulkan loader skips the vendor driver")
    if "ro.vulkan.apex=" not in vendor_props.splitlines():
        problems.append("VENDOR/build.prop does not claim ro.vulkan.apex empty, so the Vulkan APEX sets it")

disks_to_check = [d for d in ["system.vhdx", "data.vhdx"] if (image / d).exists()]
if not disks_to_check and (image / "disk.vhdx").exists():
    disks_to_check = ["disk.vhdx"]
if not disks_to_check:
    problems.append("no guest disk image found")
for disk_file in disks_to_check:
    try:
        qemu_img = shutil.which("qemu-img") or "qemu-img"
        info = json.loads(subprocess.run([qemu_img, "info", "--output=json", str(image / disk_file)], check=True, capture_output=True, text=True).stdout)
        if info.get("format") != "vhdx":
            problems.append(f"{disk_file} is {info.get('format')}, not vhdx")
        print(f"{disk_file}: {info['virtual-size'] >> 30} GiB virtual, {info.get('actual-size', 0) >> 20} MiB on disk")
    except Exception as e:
        problems.append(f"could not inspect {disk_file}: {e}")
if "system.vhdx" in disks_to_check:
    if "androidboot.boot_devices=" not in manifest["kernel_cmdline"]:
        problems.append("the kernel command line does not name the boot devices")
else:
    if "androidboot.boot_part_uuid=" not in manifest["kernel_cmdline"]:
        problems.append("the kernel command line does not name the disk")
if "blkdevparts=sda:" not in manifest["kernel_cmdline"]:
    problems.append("the kernel command line does not restore Hyper-V GPT partition names")
if "androidboot.selinux=permissive" in manifest["kernel_cmdline"]:
    problems.append("the kernel command line sets SELinux to permissive")
for boot_arg in [
    "androidboot.hardware.gltransport=hvsocket",
    "androidboot.hardware.egl=angle",
    "androidboot.hardware.vulkan=ranchu",
    "androidboot.hardware.gralloc=minigbm",
    "androidboot.cpuvulkan.version=0",
]:
    if boot_arg not in manifest["kernel_cmdline"]:
        problems.append(f"the kernel command line does not contain {boot_arg}")

def ramdisk_entries(data):
    """Name to mode of every entry in a ramdisk of gzip'd newc cpio archives."""
    entries = {}
    archive = gzip.decompress(data)  # every gzip member, one after another
    at = 0
    while at + 110 <= len(archive):
        header = archive[at:at + 110]
        if header[:6] not in (b"070701", b"070702"):
            # Padding after an archive's trailer; the next archive, if any, follows it.
            following = archive.find(b"0707", at)
            if following < 0:
                break
            at = following
            continue
        mode = int(header[14:22], 16)
        size = int(header[54:62], 16)
        name_size = int(header[94:102], 16)
        name = archive[at + 110:at + 110 + name_size - 1].decode()
        at = (at + 110 + name_size + 3) & ~3
        at = (at + size + 3) & ~3
        if name == "TRAILER!!!":
            continue  # the next archive follows
        entries[name.lstrip("./")] = mode
    return entries


entries = ramdisk_entries((image / "initrd.img").read_bytes())
# First stage init mounts onto these and fails without them.
mount_points = ["dev", "proc", "sys", "mnt", "debug_ramdisk", "second_stage_resources", "metadata"]
missing = [d for d in mount_points if not stat.S_ISDIR(entries.get(d, 0))]
if missing:
    problems.append(f"the ramdisk has no {', '.join(missing)} directories, which first stage init mounts onto")
links = sum(1 for mode in entries.values() if stat.S_ISLNK(mode))
print(f"initrd.img: {len(entries)} entries, {links} symbolic links")
if links == 0:
    problems.append("the ramdisk has no symbolic links; they were written as plain files")

for p in problems:
    print("problem:", p)
print("image ok" if not problems else f"{len(problems)} problems")
sys.exit(1 if problems else 0)
