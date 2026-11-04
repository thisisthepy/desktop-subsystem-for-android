"""Checks a guest image made by make-image.py.

    uv run dsa/guest/image/check-image.py dsa/target/image/17.0.0-16373615 .scratch/image-keys

It fails when the shell apps are still there, when the build is still debuggable, when adbd
could start without developer mode, listen on TCP or ask for an authorisation, when an
app is not signed with one of the keys in the key folder, when the framework overlay or the
trace script is missing, when the agent is missing or not signed with the platform key, when
Settings still has the preferences or code that crash it without Bluetooth, when a system
app users open is not compiled for the guest's boot image and its own dex, when the files
do not match image.json, or when the ramdisk lacks the directories and links Android's first
stage init needs.

The signatures are read from the apps themselves. sign_target_files_apks copies
META/apkcerts.txt over unchanged, so that file still names AOSP's keys after signing.
"""

import base64
import gzip
import io
import shutil
import stat

import hashlib
import importlib.util
import json
import re
import subprocess
import sys
import zipfile
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("make_image", Path(__file__).with_name("make-image.py"))
make_image = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(make_image)

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
    for app in ["Launcher3QuickStep", "SystemUI", "LatinIME", "CuttlefishService"]:
        if any(f"/{app}/" in n and n.endswith(".apk") for n in names):
            problems.append(f"{app} is still in the image")
    features_xml = "VENDOR/etc/permissions/dsa-unavailable-features.xml"
    features = z.read(features_xml).decode() if features_xml in names else ""
    if 'name="android.software.device_lock"' not in features:
        problems.append("the device lock feature is still declared, so its service and controller start")
    props = z.read("SYSTEM/build.prop").decode()
    if "ro.debuggable=1" in props or "ro.build.type=userdebug" in props:
        problems.append("the system is still a debuggable build")
    # adb: present but stopped until the user turns developer mode on, never authorised on
    # the device (there is no SystemUI to ask), and listening on vsock only, never on TCP.
    lines = props.splitlines()
    if "ro.debuggable=0" not in lines:
        problems.append("SYSTEM/build.prop does not say ro.debuggable=0")
    if "ro.adb.secure=0" not in lines:
        problems.append("adbd would ask for an authorisation no screen can give (ro.adb.secure is not 0)")
    if f"service.adb.listen_addrs=vsock:{make_image.ADB_VSOCK_PORT}" not in lines:
        problems.append(f"adbd does not listen on vsock port {make_image.ADB_VSOCK_PORT} alone")
    if "persist.sys.usb.config=none" not in lines:
        problems.append("the USB configuration would start adbd without developer mode")
    for prop_file in make_image.PROP_FILES:
        if prop_file in names:
            text = z.read(prop_file).decode(errors="replace")
            for key in make_image.DROPPED_PROPS:
                if re.search(rf"^{re.escape(key)}=", text, re.M):
                    problems.append(f"{prop_file} still has {key}, so adbd would listen on TCP")
    channeld_rc = make_image.CHANNELD_RC
    if channeld_rc in names:
        rc = z.read(channeld_rc).decode()
        for trigger in ["on property:vendor.dsa.adb=1", "on property:vendor.dsa.adb=0"]:
            if trigger not in rc:
                problems.append(f"{channeld_rc} lacks '{trigger}', so developer mode cannot switch adbd")
    property_contexts = make_image.VENDOR_PROPERTY_CONTEXTS
    if property_contexts not in names or make_image.ADB_PROPERTY_CONTEXT not in z.read(property_contexts).decode():
        problems.append(f"{make_image.ADB_PROPERTY} has no property context, so dsa-channeld cannot set it")
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
    # lmkd attaches its memory event programs to tracepoints when boot completes, and on
    # Hyper-V that waits behind every tracefs write made at the same time. Wi-Fi's trace
    # instance setup made enough of them to hold lmkd's main loop past its watchdog, which
    # then killed the app just opened.
    if "SYSTEM/etc/init/wifi.rc" in names and re.search(
            r"^on property:sys\.boot_completed=1 && property:sys\.wifitracing\.started=0\s*$",
            z.read("SYSTEM/etc/init/wifi.rc").decode(), re.MULTILINE):
        problems.append("wifi.rc still sets up a kernel trace instance when boot completes, which "
                        "holds lmkd past its watchdog and gets a just-opened app killed")
    # Without the Bluetooth service, these controllers take Settings down when their screen opens.
    for name in names:
        if not name.endswith(make_image.SETTINGS_APK):
            continue
        with zipfile.ZipFile(io.BytesIO(z.read(name))) as apk:
            files = set(apk.namelist())
            for xml, controllers in make_image.SETTINGS_CONTROLLERS_WITHOUT_BLUETOOTH.items():
                if xml not in files:
                    continue
                _, left = make_image.remove_xml_elements(apk.read(xml), "controller", set(controllers))
                if left:
                    problems.append(f"{name}:{xml} still has {', '.join(left)}, which crash Settings "
                                    "without the Bluetooth service")
            for dex in sorted(f for f in files if re.fullmatch(r"classes\d*\.dex", f)):
                content = apk.read(dex)
                if content[:4] != b"dex\n":
                    continue
                _, left = make_image.edit_dex(content, make_image.SETTINGS_CODE_WITHOUT_BLUETOOTH)
                if left:
                    problems.append(f"{name}:{dex} still has the code in {', '.join(sorted(set(left)))} "
                                    "that crashes Settings without the Bluetooth adapter")
    # The system apps users open, compiled when the image was made: each odex must be there,
    # made for the APK's own (edited, signed) dex and the guest's boot image, or ART drops it
    # and the app runs interpreted again.
    problems += make_image.compiled_app_problems(z)
    if "VENDOR/etc/fstab.dsa" not in names:
        problems.append("the Hyper-V mount table is missing")
    if "SYSTEM/etc/selinux/plat_sepolicy.cil" not in names:
        problems.append("plat_sepolicy.cil is missing from the image")
    else:
        plat_sepolicy = z.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        if not re.search(r"\(allow\s+appdomain\s+self\s+\(\s*vsock_socket\s*\(.*connect", plat_sepolicy):
            problems.append("the SELinux policy does not allow appdomain to connect vsock_socket")
        # The guest kernel puts every wakeup source under /sys/devices/virtual/wakeup. The
        # suspend service may read them only there and through /sys/class/wakeup, as
        # sysfs_wakeup; anywhere else they are plain sysfs and each read is refused.
        for path in ["/devices/virtual/wakeup", "/class/wakeup"]:
            if not re.search(r'\(genfscon\s+sysfs\s+"' + re.escape(path) + r'"\s+\(u\s+object_r\s+sysfs_wakeup\b',
                             plat_sepolicy):
                problems.append(f"the SELinux policy does not label sysfs {path} sysfs_wakeup, so the "
                                "suspend service cannot read the wakeup sources")

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

    # The framework overlay, without which every list row shows a mouse press 100 ms late.
    if "VENDOR/overlay/DsaFrameworkOverlay.apk" not in names:
        problems.append("VENDOR/overlay/DsaFrameworkOverlay.apk is missing, so a press on a list row "
                        "shows only after the 100 ms tap timeout")
    if "SYSTEM/etc/init/dsa-trace.rc" not in names:
        problems.append("SYSTEM/etc/init/dsa-trace.rc is missing, so the input trace has no main thread timing")

    # DSA's composer, which sends the host each task's frames: with Cuttlefish's in its place,
    # no task window ever gets one.
    if "VENDOR/bin/hw/dsa-hwcomposer" not in names:
        problems.append("VENDOR/bin/hw/dsa-hwcomposer is missing from the image")
    if "VENDOR/etc/init/dsa-hwcomposer.rc" not in names:
        problems.append("VENDOR/etc/init/dsa-hwcomposer.rc is missing, so init never starts the composer")
    composer_manifest = "VENDOR/etc/vintf/manifest/dsa-hwcomposer.xml"
    if composer_manifest not in names:
        problems.append(f"{composer_manifest} is missing, so the composer HAL is not declared")
    elif "<fqname>IComposer/default</fqname>" not in z.read(composer_manifest).decode():
        problems.append("dsa-hwcomposer.xml does not declare IComposer/default")
    for apex in ["com.android.hardware.graphics.composer.ranchu", "com.android.hardware.graphics.composer.drm_hwcomposer"]:
        if f"VENDOR/apex/{apex}.apex" in names:
            problems.append(f"{apex} is still in the image and would serve the composer HAL too")
    if "META/file_contexts.bin" in names and b"/vendor/bin/hw/dsa-hwcomposer" not in z.read("META/file_contexts.bin"):
        problems.append("META/file_contexts.bin does not label dsa-hwcomposer, so init refuses to start it")

    # DSA's power and health HALs: without them the host never hears the guest's power hints,
    # and the guest's battery is Cuttlefish's made-up one.
    for name, what in [("dsa-power", "IPower/default"), ("dsa-health", "IHealth/default")]:
        if f"VENDOR/bin/hw/{name}" not in names:
            problems.append(f"VENDOR/bin/hw/{name} is missing from the image")
        if f"VENDOR/etc/init/{name}.rc" not in names:
            problems.append(f"VENDOR/etc/init/{name}.rc is missing, so init never starts {name}")
        hal_manifest = f"VENDOR/etc/vintf/manifest/{name}.xml"
        if hal_manifest not in names:
            problems.append(f"{hal_manifest} is missing, so its HAL is not declared")
        elif f"<fqname>{what}</fqname>" not in z.read(hal_manifest).decode():
            problems.append(f"{name}.xml does not declare {what}")
        if "META/file_contexts.bin" in names and f"/vendor/bin/hw/{name}".encode() not in z.read("META/file_contexts.bin"):
            problems.append(f"META/file_contexts.bin does not label {name}, so init refuses to start it")
    for apex in ["com.android.hardware.power", "com.google.cf.health"]:
        if f"VENDOR/apex/{apex}.apex" in names:
            problems.append(f"{apex} is still in the image and would serve its HAL too")

    # DSA's codec HAL: without it every video decodes on the guest's processors.
    if "VENDOR/bin/hw/dsa-codec2" not in names:
        problems.append("VENDOR/bin/hw/dsa-codec2 is missing from the image")
    if "VENDOR/etc/init/dsa-codec2.rc" not in names:
        problems.append("VENDOR/etc/init/dsa-codec2.rc is missing, so init never starts the codec HAL")
    codec2_manifest = "VENDOR/etc/vintf/manifest/dsa-codec2.xml"
    if codec2_manifest not in names:
        problems.append(f"{codec2_manifest} is missing, so the codec HAL is not declared")
    elif "<fqname>IComponentStore/default</fqname>" not in z.read(codec2_manifest).decode():
        problems.append("dsa-codec2.xml does not declare IComponentStore/default")
    if "VENDOR/etc/media_codecs_c2.xml" not in names:
        problems.append("VENDOR/etc/media_codecs_c2.xml is missing, so the framework lists none of the host's decoders")
    if "META/file_contexts.bin" in names and b"/vendor/bin/hw/dsa-codec2" not in z.read("META/file_contexts.bin"):
        problems.append("META/file_contexts.bin does not label dsa-codec2, so init refuses to start it")

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

    # init reads these as vendor_init, which may not set the user build's properties.
    for name in sorted(make_image.VENDOR_PROP_FILES & set(names)):
        for line in z.read(name).decode(errors="replace").splitlines():
            if line.split("=", 1)[0] in make_image.VENDOR_FORBIDDEN_PROPS:
                problems.append(f"{name} sets {line}, which vendor_init may not set")

    # Without the image's own tag, PackageManager takes a new image for the one before it.
    incremental = [l for l in props.splitlines() if l.startswith("ro.build.version.incremental=")]
    if not incremental or not re.search(r"\.dsa[0-9a-f]{12}$", incremental[0]):
        problems.append(f"the system build.prop has no image tag in {incremental or 'ro.build.version.incremental'}")

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
# The freeform display DSA's composer sets up, and nothing left over from Cuttlefish's.
for boot_arg in ["androidboot.dsa.display_width=", "androidboot.dsa.display_height=", "androidboot.dsa.display_dpi="]:
    if boot_arg not in manifest["kernel_cmdline"]:
        problems.append(f"the kernel command line does not contain {boot_arg}")
for boot_arg in ["androidboot.hardware.hwcomposer.mode=", "androidboot.vendor.apex.com.android.hardware.graphics.composer="]:
    if boot_arg in manifest["kernel_cmdline"]:
        problems.append(f"the kernel command line still sets {boot_arg} for Cuttlefish's composer")

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
