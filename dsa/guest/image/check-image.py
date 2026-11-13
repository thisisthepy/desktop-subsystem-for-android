"""Checks a guest image made by make-image.py.

    uv run dsa/guest/image/check-image.py dsa/target/image/17.0.0-16373615 .scratch/image-keys

It fails when the shell apps are still there, when the build is still debuggable, when adbd
could start without developer mode, listen on TCP or ask for an authorisation, when an
app is not signed with one of the keys in the key folder, when a system screen overlay is
missing or was built against another framework-res or another build of the app it overlays,
when Settings' pages would open with their large title folded, when the trace script is missing, when the agent is missing or not signed with the platform key, when
Settings still has the preferences or code that crash it without Bluetooth, when Settings'
home list would add Android's freeform padding to the navigation bar inset, when the window
manager would not give a freeform window its status bar and navigation bar insets, when the
package manager would refuse a multiArch app with arm64-v8a code only while ARM translation is
on, or services.jar stores its dex unaligned, when the framework overlay's status bar heights
are not the window title bar's or its navigation bar heights are not 24 dp, when a system
app users open is not compiled for the guest's boot image and its own dex, when an app the
published build shipped compiled lost its odex, vdex or app image, or has one made for
other dex, another boot image or another compiler filter, when the files
do not match image.json, when an arm64 image carries an ARM translation package or an x86_64
image's default one does not match image.json or would be refused by the host, when the patched
framework image.json names (SurfaceFlinger, libhwui and libEGL from the Framework build) is not
in the image as built, when DSA's KeyMint from the same build, when image.json names it, is not
in the image as built, would not start in the KeyMint HAL's domain or lacks a library, or runs
beside a KeyMint APEX the kernel command line still selects, when the kernel or a DSA guest
binary is built for another
architecture than the image's, when the ramdisk lacks the directories and links Android's
first stage init needs, when the network adapter is not shown to Android as Wi-Fi with its
address and DNS servers from DHCP, or when the kernel command line, a mount table or an init script
names a disk by its kernel name (/dev/block/sda), which follows the order the disks were
probed in.

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

# The guest architecture: an image.json without one is from before arm64 images, so x86_64.
arch = manifest.get("arch") or make_image.DEFAULT_ARCH
if arch not in make_image.ARCHES:
    problems.append(f"image.json names architecture {arch}, which DSA makes no image for")
    arch = make_image.DEFAULT_ARCH
make_image.use_arch(arch)
target = manifest.get("source", {}).get("target", "")
if arch == "arm64" and "arm64_only" not in target:
    problems.append(f"the arm64 image is made from {target or 'an unnamed target'}, not a 64-bit only one, "
                    "so it would carry 32-bit ARM code")
if arch == "x86_64" and "arm64" in target:
    problems.append(f"the x86_64 image is made from the arm64 target {target}")

# The kernel, when the image has one, must be in its architecture's format and name.
kernel = make_image.kernel_name(arch)
for other in {a["kernel"] for a in make_image.ARCHES.values()} - {kernel}:
    if other in manifest["files"]:
        problems.append(f"the {arch} image carries {other}, a kernel name for another architecture")
if kernel in manifest["files"]:
    with open(image / kernel, "rb") as f:
        found = make_image.kernel_format(f.read(0x1000))
    if found != arch:
        problems.append(f"{kernel} is not a kernel for {arch} (its header says {found or 'neither'})")

# An x86_64 image's release may carry the default ARM translation package; an arm64 one never.
problems += make_image.check_default_arm_translation(manifest, image)

for name, expected in manifest["files"].items():
    h = hashlib.sha256()
    with open(image / name, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    if h.hexdigest() != expected["sha256"]:
        problems.append(f"{name} does not match image.json")

with zipfile.ZipFile(image / "signed-target_files.zip") as z:
    names = z.namelist()
    # Mount tables, init scripts and shell scripts find disks by partition name or UUID; a
    # kernel disk name (sda) points at whichever disk Hyper-V's SCSI controller probed first.
    for name in names:
        base = name.rsplit("/", 1)[-1]
        if base.startswith("fstab.") or base.endswith((".rc", ".sh", ".prop")):
            for reference in sorted(set(make_image.kernel_disk_names(z.read(name).decode(errors="replace")))):
                problems.append(f"{name} names a disk by its probe order ({reference}), "
                                "which changes when the host attaches another disk")
    # LatinIME was also the guest's spell checker, and PicoTts would be a text-to-speech engine
    # of its own: the agent's speech and spelling services, which answer with the host's, are
    # the only ones.
    for app in ["Launcher3QuickStep", "SystemUI", "LatinIME", "CuttlefishService", "PicoTts"]:
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
    # Settings' home list is padded by the navigation bar inset alone, as its other lists are,
    # not by Android 17's freeform padding on top of it.
    for name in names:
        if name.endswith(make_image.SETTINGS_APK):
            left = make_image.apk_edits_left(z.read(name), code=make_image.SETTINGS_CODE_FREEFORM_PADDING)
            if left:
                problems.append(f"{name} still has {'; '.join(left)}, so its home list ends higher above "
                                "the window's bottom edge than its other lists")
    # Settings' pages take whether their large title opens expanded from the theme, which the
    # Settings overlay sets by the window's height.
    for name in names:
        if name.endswith(make_image.SETTINGS_APK):
            left = make_image.apk_edits_left(z.read(name),
                                             theme_attributes=make_image.SETTINGS_TITLE_EXPANDED)
            if left:
                problems.append(f"{name} still has {'; '.join(left)}, so its pages open with the large "
                                "title folded in every window")
    # The window manager hands a freeform window its status bar inset, so an app that pads its
    # toolbar by that inset keeps it below the window's title bar, and its navigation bar
    # inset, so its bottom bar keeps clear of the window's bottom edge. services.jar keeps its dex
    # aligned, or system_server copies 20 MiB of it into memory; its odex is checked with the
    # apps the published build compiled, below.
    if make_image.SERVICES_JAR not in names:
        problems.append(f"{make_image.SERVICES_JAR} is missing")
    else:
        services = z.read(make_image.SERVICES_JAR)
        left = make_image.apk_edits_left(services, code=make_image.SERVICES_INSETS_CODE)
        if left:
            problems.append(f"{make_image.SERVICES_JAR} still has {'; '.join(left)}, so a freeform window "
                            "gets no status bar or navigation bar inset, and an app that pads by them draws "
                            "under the title bar and on the window's bottom edge")
        # The package manager takes every 64-bit ABI the guest lists for a multiArch app, so an
        # app with arm64-v8a code only installs while ARM translation is on.
        left = make_image.apk_edits_left(services, code=make_image.SERVICES_ABI_CODE)
        if left:
            problems.append(f"{make_image.SERVICES_JAR} still has {'; '.join(left)}, so Android refuses a "
                            "multiArch app with arm64-v8a code only even while ARM translation is on")
        # The WebView zygote starts with the first WebView renderer, so nothing in services.jar
        # starts it at boot.
        callers = make_image.jar_callers(services, make_image.WEBVIEW_ZYGOTE_START)
        if callers:
            problems.append(f"{make_image.SERVICES_JAR} still starts the WebView zygote from "
                            f"{', '.join(callers)}, so it holds its memory from boot whether or not an "
                            "app uses WebView")
        misaligned = make_image.zip_misaligned(services)
        if misaligned:
            problems.append(f"{make_image.SERVICES_JAR} stores {', '.join(misaligned)} unaligned, so "
                            "system_server copies them into memory")
    # The system apps users open, compiled when the image was made: each odex must be there,
    # made for the APK's own (edited, signed) dex and the guest's boot image, or ART drops it
    # and the app runs interpreted again.
    problems += make_image.compiled_app_problems(z)
    # The apps the published build shipped compiled keep their odex, vdex and app image,
    # valid for their dex and the guest's boot image, with the filter they were compiled with.
    problems += make_image.prebuilt_oat_problems(z)
    if "VENDOR/etc/fstab.dsa" not in names:
        problems.append("the Hyper-V mount table is missing")
    elif b"zram_backingdev_size=" not in z.read("VENDOR/etc/fstab.dsa"):
        problems.append("the mount table gives zram no backing file, so its cold pages stay in memory")
    # fs_mgr makes zram's backing file only in a folder that exists, which this script makes.
    if make_image.ZRAM_BACKING_RC not in names:
        problems.append(f"{make_image.ZRAM_BACKING_RC} is missing, so zram starts without its backing file")
    plat_contexts = "SYSTEM/etc/selinux/plat_file_contexts"
    if plat_contexts in names and make_image.ZRAM_BACKING_CONTEXT not in z.read(plat_contexts).decode().splitlines():
        problems.append("plat_file_contexts does not label zram's backing folder, so the loop driver "
                        "may not read or write the file")
    problems += make_image.direct_mapped_problems(names, z.read, manifest.get("direct_mapped"))
    if "SYSTEM/etc/selinux/plat_sepolicy.cil" not in names:
        problems.append("plat_sepolicy.cil is missing from the image")
    else:
        plat_sepolicy = z.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        if not re.search(r"\(allow\s+appdomain\s+self\s+\(\s*vsock_socket\s*\(.*connect", plat_sepolicy):
            problems.append("the SELinux policy does not allow appdomain to connect vsock_socket")
        # system_server renders its own overlays and dialogs through the same ANGLE and Vulkan
        # driver. Without the socket its RenderThread aborts and takes the framework down.
        if not re.search(r"\(allow\s+system_server\s+self\s+\(\s*vsock_socket\s*\(.*connect", plat_sepolicy):
            problems.append("the SELinux policy does not allow system_server to connect vsock_socket, "
                            "so any window system_server draws aborts it")
        # The guest kernel puts every wakeup source under /sys/devices/virtual/wakeup. The
        # suspend service may read them only there and through /sys/class/wakeup, as
        # sysfs_wakeup; anywhere else they are plain sysfs and each read is refused.
        for path in ["/devices/virtual/wakeup", "/class/wakeup"]:
            if not re.search(r'\(genfscon\s+sysfs\s+"' + re.escape(path) + r'"\s+\(u\s+object_r\s+sysfs_wakeup\b',
                             plat_sepolicy):
                problems.append(f"the SELinux policy does not label sysfs {path} sysfs_wakeup, so the "
                                "suspend service cannot read the wakeup sources")
        if not re.search(r"\(allow\s+vendor_init\s+gpu_device\s+\(\s*chr_file\s*\([^)]*getattr", plat_sepolicy):
            problems.append("the SELinux policy does not let vendor_init check the gpu_device DRM cards, so "
                            "init's wait for /dev/dri/card0 times out and holds up boot")
        # Shared memory is a memfd, labelled with its maker's tmpfs type. Every memfd the media
        # services hand an app or each other must be usable on the other side, or the binder
        # call that carries it fails: with SELinux enforcing, no app played a sound.
        problems += make_image.memfd_sharing_problems(plat_sepolicy)
        # The sensors HAL gets its event queue as a memfd of system_server's; refused, it has
        # nowhere to write events and no app sees a sensor.
        if "VENDOR/bin/hw/dsa-sensors" in names:
            problems += make_image.memfd_sharing_problems(
                plat_sepolicy, sharing=make_image.MEMFD_SHARING_SENSORS, to_apps=[])

    # One declaration of each HAL instance across the vendor manifest, its fragments and the
    # active vendor APEXes, and one for every instance a DSA service registers.
    problems += make_image.vendor_vintf_problems(names, z.read, manifest["kernel_cmdline"])

    if "VENDOR/lib64/hw/vulkan.ranchu.so" not in names:
        problems.append("VENDOR/lib64/hw/vulkan.ranchu.so is missing from the image")
    if "VENDOR/lib64/libOpenglSystemCommon.so" not in names:
        problems.append("VENDOR/lib64/libOpenglSystemCommon.so is missing from the image")

    # Every binary DSA builds for the guest must be built for its architecture: one built for
    # another is refused by the loader, and its service never starts.
    machine = make_image.ARCHES[arch]["elf_machine"]
    for name in make_image.DSA_GUEST_BINARIES:
        if name not in names:
            continue
        with z.open(name) as f:
            found = make_image.elf_machine(f.read(64))
        if found != machine:
            problems.append(f"{name} is built for ELF machine {found}, not {arch} ({machine})")

    # The control channel daemon, which makes the session active, and its own domain: the
    # only one that may read the boot's secret.
    if "VENDOR/bin/dsa-channeld" not in names:
        problems.append("VENDOR/bin/dsa-channeld is missing from the image")
    if "VENDOR/etc/init/dsa-channeld.rc" not in names:
        problems.append("VENDOR/etc/init/dsa-channeld.rc is missing, so init never starts the daemon")
    if "SYSTEM/etc/selinux/plat_sepolicy.cil" in names and "dsa_channeld" not in z.read(
            "SYSTEM/etc/selinux/plat_sepolicy.cil").decode():
        problems.append("plat_sepolicy.cil has no dsa_channeld domain, so init cannot start the daemon")
    # The guest kernel's last log stays on the small pstore disk until the daemon unlinks the
    # records the host has collected; without that the disk fills and keeps old crashes.
    if "SYSTEM/etc/selinux/plat_sepolicy.cil" in names and not re.search(
            r"\(allow\s+dsa_channeld\s+pstorefs\s+\(\s*file\s+\([^)]*\bunlink\b",
            z.read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()):
        problems.append("the SELinux policy does not let dsa_channeld clear pstore records the host "
                        "has collected")
    if "META/file_contexts.bin" in names and b"dsa_channeld_exec" not in z.read("META/file_contexts.bin"):
        problems.append("META/file_contexts.bin does not label dsa-channeld, so the vendor image leaves it "
                        "vendor_file and init refuses to start it")
    # Root elevation: the su client apps run, the elevation helper only it reaches, and the su
    # guard that hides su from every app the host did not allow.
    if "VENDOR/bin/dsa-channeld" in names:
        problems += make_image.root_elevation_problems(names, z.read)
        # KeyMint's root key from the host, which only KeyMint may read.
        problems += make_image.keymint_key_problems(names, z.read)
        # The host's root certificates among Android's system certificates.
        problems += make_image.cacerts_problems(names, z.read)
        # The guest's DNS, answered by the host's resolver through the daemon's stub.
        problems += make_image.dns_problems(names, z.read)

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
    if agent in names and make_image.AGENT_SYSCONFIG not in names:
        problems.append(f"{make_image.AGENT_SYSCONFIG} is missing, so Android's backup manager does not use "
                        "the agent's backup transport and app data never reaches the host's backup")

    # The network: eth0 shown as Wi-Fi, with its address and DNS servers from DHCP.
    problems.extend(make_image.network_problems(names, z.read, manifest["kernel_cmdline"]))

    # The system screen overlays: the framework's (the tap timeout, the dialogs, toasts and
    # colours), Settings', Files', PermissionController's, PackageInstaller's and the share
    # sheet's, each built against this image's framework-res and the app it overlays.
    problems += make_image.system_overlay_problems(names, z.read)
    if "SYSTEM/etc/init/dsa-trace.rc" not in names:
        problems.append("SYSTEM/etc/init/dsa-trace.rc is missing, so the input trace has no main thread timing")

    # ARM app translation: an installed package is mounted and its properties imported only
    # when the host says so at boot; without one, nothing in the image may turn a bridge on.
    for needed in ["SYSTEM/etc/init/dsa-arm-translation.rc", "SYSTEM/etc/dsa/arm-translation-on.prop",
                   "SYSTEM/etc/dsa/arm-translation-off.prop"]:
        if needed not in names:
            problems.append(f"{needed} is missing, so an installed ARM translation package is never used")
    if "PRODUCT/etc/build.prop" in names:
        # Signing rewrites every build.prop and ends it with a blank line, which init skips.
        product_lines = [line for line in z.read("PRODUCT/etc/build.prop").decode(errors="replace").splitlines()
                         if line.strip()]
        wanted_import = "import /system/etc/dsa/arm-translation-${ro.boot.dsa.arm_translation:-off}.prop"
        if not product_lines or product_lines[-1].strip() != wanted_import:
            problems.append("PRODUCT/etc/build.prop does not end with the ARM translation import, so an "
                            "installed package's ABIs never reach the package manager")
    if "SYSTEM/etc/init/dsa-arm-translation.rc" in names:
        rc = z.read("SYSTEM/etc/init/dsa-arm-translation.rc").decode(errors="replace")
        if "on post-fs-data && property:ro.boot.dsa.arm_translation=on" not in rc:
            problems.append("dsa-arm-translation.rc mounts the ARM translation package without waiting "
                            "for the host to say one is installed")
        if "on post-fs-data && property:ro.boot.dsa.arm_translation=default" not in rc:
            problems.append("dsa-arm-translation.rc never lays the image's default ARM translation package "
                            "over the system")
    # The default translator: in an x86_64 image's system partition when image.json describes
    # one, and nowhere in an arm64 image.
    problems += make_image.check_builtin_arm_translation(manifest, names, z.read)
    # Where a package is bound: an x86_64 image's empty folders, empty linker configuration and
    # links into the mount, and none of them in an arm64 image. Nothing is laid over a folder
    # of the system's programs, by either script.
    problems += make_image.check_arm_translation_reach(manifest, names, z.read, z.getinfo)
    for rc_name in ["dsa-arm-translation.rc", "dsa-gapps.rc"]:
        if f"SYSTEM/etc/init/{rc_name}" in names:
            problems += make_image.program_folder_problems(
                rc_name, z.read(f"SYSTEM/etc/init/{rc_name}").decode(errors="replace"))
    # The patched framework: the Framework build's files, for this build and architecture, and
    # its KeyMint, the only one Android runs.
    problems += make_image.check_framework(manifest, names, z.read)
    # The ABIs translation adds come after the guest's own, so the Play Store hands out the
    # x86_64 builds of Google Play services and the other system apps, not their ARM builds.
    if "SYSTEM/etc/dsa/arm-translation-on.prop" in names:
        for line in z.read("SYSTEM/etc/dsa/arm-translation-on.prop").decode(errors="replace").splitlines():
            key, _, value = line.partition("=")
            if ".cpu.abilist" in key and "x86_64" in value.split(",") and not value.startswith("x86_64"):
                problems.append(f"arm-translation-on.prop sets {line}, so x86_64 is not the ABI apps "
                                "are installed for first and the Play Store hands out ARM builds")

    # Google Play: the image the user installed is laid over the system, and the boot made an
    # upgrade, only when the host says one is installed.
    for needed in ["SYSTEM/etc/init/dsa-gapps.rc", "SYSTEM/etc/dsa/gapps-on.prop",
                   "SYSTEM/etc/dsa/gapps-off.prop"]:
        if needed not in names:
            problems.append(f"{needed} is missing, so the Google apps a user installs are never used")
    if "PRODUCT/etc/build.prop" in names:
        product_lines = [line.strip() for line in
                         z.read("PRODUCT/etc/build.prop").decode(errors="replace").splitlines()]
        if "import /system/etc/dsa/gapps-${ro.boot.dsa.gapps:-off}.prop" not in product_lines:
            problems.append("PRODUCT/etc/build.prop does not import the Google Play properties, so "
                            "the Google apps a user installs never get their default permissions")
    if "SYSTEM/etc/init/dsa-gapps.rc" in names:
        rc = z.read("SYSTEM/etc/init/dsa-gapps.rc").decode(errors="replace")
        sections = rc.split("\non ")
        for section in sections:
            first = section.split("\n", 1)[0]
            trigger = first[3:] if first.startswith("on ") else first
            if "mount " in section and not trigger.startswith("post-fs-data && property:ro.boot.dsa.gapps=on"):
                problems.append("dsa-gapps.rc lays the Google apps over the system without waiting "
                                "for the host to say they are installed")
        if "on post-fs-data && property:ro.boot.dsa.gapps=on" not in rc:
            problems.append("dsa-gapps.rc never lays the Google apps over the system")
        # Each folder the Google apps are laid over is in the system, so its overlay mounts.
        problems += make_image.gapps_folder_problems(names)
    # The add-on disk the Google apps and a user's ARM translation package are mounted from.
    problems += make_image.addon_disk_problems(names, z.read)
    problems += make_image.widevine_problems(names, z.read)
    if "SYSTEM/etc/dsa/gapps-on.prop" in names:
        on = [line.strip() for line in
              z.read("SYSTEM/etc/dsa/gapps-on.prop").decode(errors="replace").splitlines()]
        if make_image.GAPPS_PRIVAPP_LOG not in on:
            problems.append("gapps-on.prop does not set " + make_image.GAPPS_PRIVAPP_LOG + ", so a "
                            "privileged permission LiteGapps' lists miss stops Android at every boot")
    if "SYSTEM/etc/dsa/gapps-off.prop" in names:
        off = [line for line in z.read("SYSTEM/etc/dsa/gapps-off.prop").decode(errors="replace").splitlines()
               if line.strip() and not line.startswith("#")]
        if off:
            problems.append("gapps-off.prop sets properties, so a boot without Google Play is not the "
                            "image as made")

    for name in sorted(n for n in names if n.endswith("build.prop") or n.endswith("prop.default")):
        for line in z.read(name).decode(errors="replace").splitlines():
            if line.startswith("ro.dalvik.vm.native.bridge=") and line not in (
                    "ro.dalvik.vm.native.bridge=", "ro.dalvik.vm.native.bridge=0"):
                problems.append(f"{name} sets {line}, so a native bridge is on with no package installed")

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

    # DSA's GNSS HAL: without it the guest's location is Cuttlefish's made-up one, not the
    # host's.
    if "VENDOR/bin/hw/dsa-gnss" not in names:
        problems.append("VENDOR/bin/hw/dsa-gnss is missing from the image")
    if "VENDOR/etc/init/dsa-gnss.rc" not in names:
        problems.append("VENDOR/etc/init/dsa-gnss.rc is missing, so init never starts dsa-gnss")
    gnss_manifest = "VENDOR/etc/vintf/manifest/dsa-gnss.xml"
    if gnss_manifest not in names:
        problems.append(f"{gnss_manifest} is missing, so the GNSS HAL is not declared")
    elif "<fqname>IGnss/default</fqname>" not in z.read(gnss_manifest).decode():
        problems.append("dsa-gnss.xml does not declare IGnss/default")
    if "META/file_contexts.bin" in names and b"/vendor/bin/hw/dsa-gnss" not in z.read("META/file_contexts.bin"):
        problems.append("META/file_contexts.bin does not label dsa-gnss, so init refuses to start it")
    for apex in ["com.google.cf.gnss", "com.android.hardware.gnss"]:
        if f"VENDOR/apex/{apex}.apex" in names:
            problems.append(f"{apex} is still in the image and would serve the GNSS HAL too")

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

    # DSA's sensors HAL: without it apps see none of the host's sensors, or Cuttlefish's
    # made-up ones.
    if "VENDOR/bin/hw/dsa-sensors" not in names:
        problems.append("VENDOR/bin/hw/dsa-sensors is missing from the image")
    if "VENDOR/etc/init/dsa-sensors.rc" not in names:
        problems.append("VENDOR/etc/init/dsa-sensors.rc is missing, so init never starts the sensors HAL")
    sensors_manifest = "VENDOR/etc/vintf/manifest/dsa-sensors.xml"
    if sensors_manifest not in names:
        problems.append(f"{sensors_manifest} is missing, so the sensors HAL is not declared")
    elif "<fqname>ISensors/default</fqname>" not in z.read(sensors_manifest).decode():
        problems.append("dsa-sensors.xml does not declare ISensors/default")
    if "META/file_contexts.bin" in names and b"/vendor/bin/hw/dsa-sensors" not in z.read("META/file_contexts.bin"):
        problems.append("META/file_contexts.bin does not label dsa-sensors, so init refuses to start it")
    for apex in ["com.android.hardware.sensors", "com.google.cf.sensors"]:
        if f"VENDOR/apex/{apex}.apex" in names:
            problems.append(f"{apex} is still in the image and would serve its sensors HAL too")
    for name in sorted(names):
        if (name.startswith("VENDOR/etc/vintf/manifest/") and name != sensors_manifest
                and b"<name>android.hardware.sensors</name>" in z.read(name)):
            problems.append(f"{name} declares a second sensors HAL, which fails the VINTF check")
    # A sensors HAL installed as plain files beside DSA's (Android 17's Cuttlefish multi-HAL):
    # init starts it, its sub-HAL cannot reach Cuttlefish's sensors control socket, and it
    # aborts every five seconds, pushing everything else out of the log.
    for name in sorted(names):
        base = name.rsplit("/", 1)[-1]
        if (name.startswith(("VENDOR/bin/hw/", "VENDOR/etc/init/", "VENDOR/lib/hw/", "VENDOR/lib64/hw/"))
                and "sensors" in base.lower()
                and name not in ("VENDOR/bin/hw/dsa-sensors", "VENDOR/etc/init/dsa-sensors.rc")):
            problems.append(f"{name} is a second sensors HAL, which init starts and which aborts in a loop")
        elif name.startswith("VENDOR/etc/sensors/") and base:
            problems.append(f"{name} lists sub-HALs for a sensors multi-HAL the image must not have")
    # DSA's camera HAL: without it apps see Cuttlefish's emulated cameras, or none.
    if "VENDOR/bin/hw/dsa-camera" not in names:
        problems.append("VENDOR/bin/hw/dsa-camera is missing from the image")
    if "VENDOR/etc/init/dsa-camera.rc" not in names:
        problems.append("VENDOR/etc/init/dsa-camera.rc is missing, so init never starts the camera HAL")
    camera_manifest = "VENDOR/etc/vintf/manifest/dsa-camera.xml"
    if camera_manifest not in names:
        problems.append(f"{camera_manifest} is missing, so the camera HAL is not declared")
    elif "<fqname>ICameraProvider/internal/0</fqname>" not in z.read(camera_manifest).decode():
        problems.append("dsa-camera.xml does not declare ICameraProvider/internal/0")
    if "META/file_contexts.bin" in names and b"/vendor/bin/hw/dsa-camera" not in z.read("META/file_contexts.bin"):
        problems.append("META/file_contexts.bin does not label dsa-camera, so init refuses to start it")
    for name in sorted(names):
        if name.startswith("VENDOR/apex/com.google.emulated.camera.provider.hal"):
            problems.append(f"{name} is still in the image; it declares the camera provider DSA's "
                            "HAL declares, which fails the vendor VINTF manifest")

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
    # The OpenGL ES version PackageManager reports as reqGlEsVersion. Without it Google Play
    # hides every app that asks for OpenGL ES (YouTube, Chrome, most games) as incompatible.
    gles = f"ro.opengles.version={make_image.OPENGLES_VERSION}"
    if gles not in vendor_props.splitlines():
        problems.append(f"VENDOR/build.prop does not set {gles} (OpenGL ES 3.2), so Android reports "
                        "no OpenGL ES and Google Play hides the apps that need it")
    for name in sorted(n for n in names if n.endswith("build.prop") or n.endswith("prop.default")):
        for line in z.read(name).decode(errors="replace").splitlines():
            if line.startswith("ro.opengles.version=") and line != gles:
                problems.append(f"{name} sets {line}, not {gles}")
    # What else Google Play compares with an app's manifest: the features every app it lists
    # takes for granted, declared in the vendor permissions and not taken away by DSA's list
    # of unavailable features. The Vulkan features come from the Cuttlefish Vulkan APEX.
    declared, unavailable = set(), set()
    for name in sorted(n for n in names if re.match(r"(VENDOR|SYSTEM|SYSTEM_EXT|PRODUCT|ODM)/etc/permissions/[^/]+\.xml$", n)):
        text = z.read(name).decode(errors="replace")
        declared.update(re.findall(r'<feature\s+name="([^"]+)"', text))
        unavailable.update(re.findall(r'<unavailable-feature\s+name="([^"]+)"', text))
    for feature in ["android.software.app_widgets", "android.software.home_screen",
                    "android.software.input_methods", "android.hardware.screen.portrait",
                    "android.hardware.screen.landscape", "android.hardware.touchscreen"]:
        if feature not in declared:
            problems.append(f"no permissions file declares {feature}, so Google Play hides the apps that need it")
        if feature in unavailable:
            problems.append(f"a permissions file marks {feature} unavailable, so Google Play hides the apps that need it")
    if not any(re.match(r"VENDOR/apex/[^/]*vulkan[^/]*\.apex$", n) for n in names):
        problems.append("the image has no Vulkan APEX (com.google.cf.vulkan), so Android declares no "
                        "Vulkan features and Google Play hides the apps that need Vulkan")
    # The density every app starts at is the boot's (the user's main monitor's). A build.prop
    # value comes before the script that sets it and would hold every boot at one density.
    for name, text in [("SYSTEM", props), ("VENDOR", vendor_props)]:
        if any(line.startswith("ro.sf.lcd_density=") for line in text.splitlines()):
            problems.append(f"{name} build.prop sets ro.sf.lcd_density, so the boot's density "
                            "never reaches SurfaceFlinger")
    if make_image.DISPLAY_RC not in names or "setprop ro.sf.lcd_density ${ro.boot.lcd_density" not in \
            z.read(make_image.DISPLAY_RC).decode(errors="replace"):
        problems.append(f"{make_image.DISPLAY_RC} does not set ro.sf.lcd_density from the boot, so "
                        "SurfaceFlinger falls back to 213 dpi and every app is relaunched after it opens")

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

    # DSA's device identity, in every partition, and nothing left of Cuttlefish's.
    identity_files = sorted(n for n in names if n.endswith("build.prop") or n.endswith("prop.default"))
    for name in identity_files:
        for line in z.read(name).decode(errors="replace").splitlines():
            if "=" not in line or line.startswith("#"):
                continue
            key, value = line.split("=", 1)
            m = make_image.PRODUCT_KEY.match(key)
            if m and value != make_image.IDENTITY[m.group(2)]:
                problems.append(f"{name} sets {line}, not DSA's {m.group(2)}")
            if make_image.FINGERPRINT_KEY.match(key):
                device = make_image.IDENTITY["device"]
                if not value.startswith(f"DSA/{make_image.IDENTITY['name']}/{device}:") or ":user/" not in value:
                    problems.append(f"{name} sets {line}, not DSA's user build fingerprint")
            if key == "ro.build.characteristics" and value != make_image.CHARACTERISTICS:
                problems.append(f"{name} sets {line}, not {make_image.CHARACTERISTICS}")
            if (m or make_image.FINGERPRINT_KEY.match(key) or key.startswith("ro.build.")) and any(
                    old in value for old in ("Cuttlefish", "vsoc_", "aosp_cf_", "userdebug")):
                problems.append(f"{name} still names Cuttlefish's build in {line}")
    product_props = z.read(make_image.PRODUCT_PROP_FILE).decode() if make_image.PRODUCT_PROP_FILE in names else ""
    if f"ro.build.characteristics={make_image.CHARACTERISTICS}" not in product_props.splitlines():
        problems.append(f"{make_image.PRODUCT_PROP_FILE} does not set ro.build.characteristics={make_image.CHARACTERISTICS}")
    for key in make_image.IDENTITY_IMPORTS:
        if f"import {make_image.RAMDISK_PROP} {key}" not in product_props.splitlines():
            problems.append(f"{make_image.PRODUCT_PROP_FILE} does not import {key} from the identity the boot carries")
    print(f"device identity checked in {len(identity_files)} property files")

identity = manifest.get("identity") or {}
if not all(identity.get(k) for k in ("release", "id", "incremental", "tags")):
    problems.append(f"image.json does not name the build's release, ID, incremental version and tags: {identity}")

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
if f"blkdevparts={make_image.SYSTEM_DISK_GUID}:" not in manifest["kernel_cmdline"]:
    problems.append("the kernel command line does not restore Hyper-V GPT partition names on the "
                    f"system disk by its GUID {make_image.SYSTEM_DISK_GUID}")
for reference in make_image.kernel_disk_names(manifest["kernel_cmdline"]):
    problems.append(f"the kernel command line names a disk by its probe order ({reference}), "
                    "which changes when the host attaches another disk")
if "androidboot.selinux=permissive" in manifest["kernel_cmdline"]:
    problems.append("the kernel command line sets SELinux to permissive")
for boot_arg in [
    "androidboot.hardware.gltransport=hvsocket",
    "androidboot.hardware.egl=angle",
    "androidboot.hardware.vulkan=ranchu",
    "androidboot.hardware.gralloc=minigbm",
    "androidboot.cpuvulkan.version=0",
    f"androidboot.opengles.version={make_image.OPENGLES_VERSION}",
]:
    if boot_arg not in manifest["kernel_cmdline"]:
        problems.append(f"the kernel command line does not contain {boot_arg}")
# The freeform display DSA's composer sets up, and nothing left over from Cuttlefish's.
for boot_arg in ["androidboot.dsa.display_width=", "androidboot.dsa.display_height=", "androidboot.dsa.display_dpi=",
                 "androidboot.lcd_density="]:
    if boot_arg not in manifest["kernel_cmdline"]:
        problems.append(f"the kernel command line does not contain {boot_arg}")
for boot_arg in ["androidboot.hardware.hwcomposer.mode=", "androidboot.vendor.apex.com.android.hardware.graphics.composer="]:
    if boot_arg in manifest["kernel_cmdline"]:
        problems.append(f"the kernel command line still sets {boot_arg} for Cuttlefish's composer")
if "androidboot.vendor.apex.com.google.emulated.camera.provider.hal=" in manifest["kernel_cmdline"]:
    problems.append("the kernel command line still selects Cuttlefish's emulated cameras")

def ramdisk_entries(data, texts=None):
    """Name to mode of every entry in a ramdisk of gzip'd newc cpio archives. With `texts`,
    also fills it with the contents of the mount tables and init scripts."""
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
        base = name.rsplit("/", 1)[-1]
        if texts is not None and stat.S_ISREG(mode) and (base.startswith("fstab.") or base.endswith((".rc", ".sh"))):
            texts[name.lstrip("./")] = archive[at:at + size].decode(errors="replace")
        at = (at + size + 3) & ~3
        if name == "TRAILER!!!":
            continue  # the next archive follows
        entries[name.lstrip("./")] = mode
    return entries


ramdisk_texts = {}
entries = ramdisk_entries((image / "initrd.img").read_bytes(), ramdisk_texts)
for name, text in sorted(ramdisk_texts.items()):
    for reference in sorted(set(make_image.kernel_disk_names(text))):
        problems.append(f"initrd.img's {name} names a disk by its probe order ({reference}), "
                        "which changes when the host attaches another disk")
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
