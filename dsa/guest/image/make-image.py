"""Makes a DSA guest image from the published AOSP build inputs of one Android version.

    uv run dsa/guest/image/make-image.py --major 17 --keys .scratch/image-keys

It takes the pinned target_files, otatools and dexpreopt_tools (fetch.sh downloads them) and:

1. removes the launcher, SystemUI and the keyboard app, because the host is the shell, and
   the apps a desktop subsystem has no use for (UNNEEDED_APPS);
2. switches the build to user (not debuggable), with adbd stopped until the user turns
   developer mode on (ADB_PROPS);
3. drops the published kernel modules (they belong to Google's kernel, and DSA ships its
   own kernel with the drivers built in) and installs the Hyper-V mount table;
   with --channeld and --agent, it adds the control channel daemon and the agent, the
   platform app that reports each app task to the host;
4. re-signs every APK, APEX and verified boot image with the keys in --keys, because the
   published build is signed with AOSP's test keys, whose private halves are public;
5. compiles the system apps users open (COMPILED_APPS) with the build's own dex2oat
   (dexpreopt_tools.zip), against the guest's boot image, so they do not run interpreted, and
   compiles again, with its own filter, any app the published build shipped compiled whose
   dex no longer matches its odex (PREBUILT_OAT_RECORD);
6. builds the images, and lays them out on the template of a user's Android disk.

The results go to dsa/target/image/<version>/:

- system.vhdx: the read-only system disk every user's VM shares (dynamic, so it holds only
  what is written): the super partition with Android itself and its verified boot data
  (the vbmeta images, and the boot images verified boot checks: boot, init_boot,
  vendor_boot);
- data.vhdx: the template of a user's data disk: empty misc, metadata and userdata
  partitions, which Android writes and formats on first boot;
- initrd.img: the vendor and generic ramdisks, which the VM boots with DSA's kernel;
- image.json: the version, the kernel command line the VM needs, and the files' SHA-256;
- arm-translation.zip, with --arm-translation (x86_64 only): the default ARM translation
  package released beside the image, which image.json describes under "arm_translation". The
  system partition carries the same package's system image, which Android uses from its first
  start while the user has installed no package of their own.

With --framework, the system partition carries the patched SurfaceFlinger, libhwui and libEGL
that recover from a GPU device loss, in place of the build's own (FRAMEWORK_FILES), and
image.json describes them under "framework".
"""

import argparse
import base64
import gzip
import hashlib
import io
import json
import os
import re
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import zipfile
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]

# The host is the shell: no launcher, no SystemUI, no keyboard app in the guest.
SHELL_APPS = ["Launcher3QuickStep", "SystemUI", "LatinIME"]

# Apps a desktop subsystem has no use for, removed so they neither take disk nor start and
# hold memory (several start at boot or on every connectivity or locale change). Each is
# a leaf: no other app, provider or system service needs it, and an app that sends one of
# their intents gets the usual "no app can do this" answer. Removed only when the build has
# them; make-image prints each one it removes.
#
# Kept on purpose, because apps rely on them: WebView, every content provider (contacts,
# calendar, media, downloads, settings, telephony's database), PackageInstaller,
# PermissionController, DocumentsUI, the print spooler, the sound picker, Settings, Provision
# (it marks the device set up), Telecom and TeleService (system_server binds them at boot),
# CarrierConfig, the browser, the camera and the clock. The browser stays although web links
# go to the host's browser by default (the agent holds the browser role then): an app may
# start it by name, the user can choose to keep links in Android, which gives the role back
# to it, and a link that cannot reach the host opens in it.
UNNEEDED_APPS = [
    # Telephony without a modem (the radio HAL is removed): calls, SMS, the SIM toolkit, MMS,
    # emergency information, IMS entitlement and the carrier's captive portal app.
    "Dialer",
    "messaging",
    "Stk",
    "MmsService",
    "EmergencyInfo",
    "ImsServiceEntitlement",
    "CarrierDefaultApp",
    # Screen savers and wallpapers: the host draws the desktop, and the guest's display is
    # never seen, so there is no screen to save or wallpaper to show (SystemUI, which draws
    # it, is removed above).
    "BasicDreams",
    "PhotoTable",
    "LiveWallpapersPicker",
    "WallpaperPicker",
    # Bluetooth MIDI needs the Bluetooth stack, which has no controller here.
    "BluetoothMidiService",
    # The secure element service is a persistent process, and no secure element exists here.
    "SecureElement",
    # The search widget lives on the launcher, which is removed.
    "QuickSearchBox",
    # Cuttlefish's monitoring service (com.android.google.gce.gceservice) reports boot and
    # network state to the Cuttlefish launcher on its host, which DSA does not have.
    "CuttlefishService",
    # Old AOSP user apps whose upstream maintenance has effectively stopped. The user installs
    # the calendar, gallery and music player they want. The providers and services beside
    # them (CalendarProvider, MediaProvider, DownloadProvider, MusicFX) stay, so other apps
    # that read calendars, media or downloads keep working.
    "Calendar",
    "Gallery2",
    "Music",
    # Developer and demo apps.
    "Traceur",
    "EasterEgg",
]

REMOVED_APPS = SHELL_APPS + UNNEEDED_APPS

# Vendor HALs whose devices a Hyper-V VM does not have: oemlock, UWB and Bluetooth talk to
# the host over Cuttlefish's virtio consoles, and Thread needs a radio. They abort in a loop,
# and system_server waits for the oemlock HAL, which their APEXes declare, forever. Without
# its HAL the Bluetooth app aborts too, waiting for the controller, each time it restarts.
# NFC and the radio interface layer reach Cuttlefish's NFC and modem simulators the same way.
REMOVED_APEXES = [
    "com.google.cf.oemlock",
    "com.android.hardware.uwb",
    "com.google.cf.bt",
    "com.android.hardware.threadnetwork",
    "com.google.cf.nfc",
    "com.google.cf.rild",
]

# Settings controllers that crash without the Bluetooth service. With the Bluetooth features
# withdrawn, system_server does not start it, so SettingsLib's LocalBluetoothManager is null,
# and LocalMediaManager is left without the InfoMediaManager it calls into. These two call it
# as soon as the sound screen opens (MediaOutputPreferenceController.onStart, and
# RemoteVolumeGroupController's constructor), which takes down the Settings process and its
# window. The preferences they drive pick a media output and set a cast session's volume,
# neither of which the guest has without Bluetooth. They are removed from the compiled screen
# in Settings.apk, which is signed again with the rest. Bluetooth comes back as a host HAL,
# and these go back in with it.
SETTINGS_APK = "/priv-app/Settings/Settings.apk"
SETTINGS_CONTROLLERS_WITHOUT_BLUETOOTH = {
    "res/xml/sound_settings.xml": [
        "com.android.settings.sound.MediaOutputPreferenceController",
        "com.android.settings.notification.RemoteVolumeGroupController",
    ],
    # The hearing devices entry. Its controller asks the null LocalBluetoothManager for its
    # profile manager in its constructor, and the page behind it lists and pairs Bluetooth
    # hearing aids.
    "res/xml/accessibility_settings.xml": [
        "com.android.settings.accessibility.AccessibilityHearingAidPreferenceController",
    ],
    # The previously connected devices list on Connected devices. It counts as available
    # without Bluetooth, because the dock updater it also drives is always there, and its
    # SavedBluetoothDeviceUpdater asks the null default adapter isEnabled() as soon as the
    # screen is shown. It lists saved Bluetooth devices (and docks, which a VM has none of).
    "res/xml/connected_devices.xml": [
        "com.android.settings.connecteddevice.PreviouslyConnectedDevicePreferenceController",
    ],
    # The page behind that list's "see all", which search still opens: its saved devices
    # list counts as available for the same dock reason and calls the same forceUpdate.
    "res/xml/previously_connected_devices.xml": [
        "com.android.settings.connecteddevice.SavedDeviceGroupController",
    ],
    # The hearing devices page, which search and the accessibility shortcut still open with
    # its entry gone: the available hearing devices list registers with the null
    # LocalBluetoothManager's event manager in onStart, which runs while it is unavailable.
    "res/xml/accessibility_hearing_aids.xml": [
        "com.android.settings.accessibility.AvailableHearingDevicePreferenceController",
    ],
}

# Settings code that dereferences the missing Bluetooth adapter where no preference can be
# taken out instead, edited in the dex files of Settings.apk. Each edit keeps the size of the
# method, so nothing else in the dex file moves.
#
# BluetoothUtils.isAudioSharingSupported (SettingsLib) calls isLeAudioBroadcastSourceSupported
# on BluetoothAdapter.getDefaultAdapter(), which is null here. The connected devices screen
# asks it in its fragment's onAttach, and so do the audio sharing and media device controllers
# on it, so the screen took the Settings process down each time it opened. LE audio sharing is
# a Bluetooth feature, so "not supported" is the true answer: the method returns false.
#
# AccessibilitySettings.onAttach hands the fragment manager to the hearing aid controller.
# That controller cannot be made without Bluetooth (above), so the fragment's lookup returns
# null and the call threw. The call is skipped; nothing else uses its result.
#
# ConnectedDeviceDashboardFragment, PreviouslyConnectedDeviceDashboardFragment and
# AccessibilityHearingAidsFragment hand themselves, in onAttach, to the controllers taken out
# of their screens above, which their use() lookups no longer find: those init calls are
# skipped too.
#
# Bluetooth comes back as a host HAL, and these edits go with it.
SETTINGS_CODE_WITHOUT_BLUETOOTH = {
    "Lcom/android/settingslib/bluetooth/BluetoothUtils;->isAudioSharingSupported": ("return false", None),
    "Lcom/android/settings/accessibility/AccessibilitySettings;->onAttach": (
        "skip call",
        "Lcom/android/settings/accessibility/AccessibilityHearingAidPreferenceController;->setFragmentManager",
    ),
    "Lcom/android/settings/connecteddevice/ConnectedDeviceDashboardFragment;->onAttach": [
        ("skip call", "Lcom/android/settings/connecteddevice/PreviouslyConnectedDevicePreferenceController;->init"),
    ],
    "Lcom/android/settings/connecteddevice/PreviouslyConnectedDeviceDashboardFragment;->onAttach": [
        ("skip call", "Lcom/android/settings/connecteddevice/SavedDeviceGroupController;->init"),
    ],
    "Lcom/android/settings/accessibility/AccessibilityHearingAidsFragment;->onAttach": [
        ("skip call", "Lcom/android/settings/accessibility/AvailableHearingDevicePreferenceController;->init"),
    ],
}

# Room below the last row of the lists in Settings, the permission screens and Files, about
# what a phone's navigation bar leaves there. A DSA window has no navigation bar, and the
# guest reports no bottom inset, which is the only space these lists keep below their last
# row, so it sat on the window's bottom edge. The space is the list's own padding with
# clipToPadding off, so rows still scroll to the edge and only the end of the list stops
# short of it; nothing tells apps there is a navigation bar.
LIST_BOTTOM_MARGIN_DP = 24
ANDROID_PADDING_BOTTOM = 0x010100D9  # android:paddingBottom

# Settings code that sets the list padding itself, over the layout's (the Settings overlay
# gives its preference lists the room above), edited in Settings.apk's dex files:
#
# - Android 16's home page (TopLevelSettings.onCreateRecyclerView) calls
#   setPadding(mPaddingHorizontal, 0, mPaddingHorizontal, 0) on its list. That build never
#   sets mPaddingHorizontal (its two pane padding is compiled out), so the call only drops
#   the layout's bottom padding: the list's style pads it 0 on each side and the layout 0 on
#   top. The call is skipped, and the list keeps the layout's room below its last row.
# - Android 17's home page pads its list's bottom by dashboard_padding_bottom only when
#   isRunningInFreeformWindow, which asks whether the window shows a caption bar. A DSA
#   window is a freeform window whose caption the host draws, outside the guest, so the
#   method says so, and Android's own padding applies; the Settings overlay sets it to
#   LIST_BOTTOM_MARGIN_DP like the other lists. Android 17 reads nothing else from that dimen.
SETTINGS_CODE_LIST_MARGIN = {
    "Lcom/android/settings/homepage/TopLevelSettings;->onCreateRecyclerView": [
        ("skip call", "Landroid/view/ViewGroup;->setPadding"),
    ],
    "Lcom/android/settings/homepage/TopLevelSettings;->isRunningInFreeformWindow": ("return true", None),
}
SETTINGS_HOME_PADDING_DIMEN = "dashboard_padding_bottom"
# Settings' All apps list sets no padding in code, and its layout none: the list gets room
# below its last row in the compiled layout itself. An overlay could not reach it without
# copying the whole ConstraintLayout and naming each of its ids and attributes.
SETTINGS_LIST_DIMENSIONS = {
    "res/layout/manage_applications_apps.xml": [
        ("androidx.recyclerview.widget.RecyclerView", ANDROID_PADDING_BOTTOM, LIST_BOTTOM_MARGIN_DP),
    ],
}

# PermissionController lists its rows in androidx.preference's preference_recyclerview with
# paddingBottom 0dp, as Settings did. It declares <overlayable>, so idmap2 lets no overlay
# replace that layout; the compiled layout in the APK is edited instead. The app lives in the
# permission APEX, which is unpacked, edited and packed again before signing (see
# edit_permission_apex).
PERMISSION_CONTROLLER_APK = "PermissionController.apk"
PERMISSION_CONTROLLER_DIMENSIONS = {
    "res/layout/preference_recyclerview.xml": [
        ("androidx.recyclerview.widget.RecyclerView", ANDROID_PADDING_BOTTOM, LIST_BOTTOM_MARGIN_DP),
    ],
}
PERMISSION_APEXES = ["SYSTEM/apex/com.android.permission.apex", "SYSTEM/apex/com.android.permission.capex"]


def float_bits(value):
    """The 32 bits of a float, as const/high16 loads them."""
    return struct.unpack("<I", struct.pack("<f", value))[0]


# Files (DocumentsUI) sets its list's padding in code: on Android 16 the bottom is the height
# of the bar below the list (getSaveLayoutHeight: the save bar, or with Material 3 the
# breadcrumb and save bar), and 0 where the window has none, as in wide windows. That method
# now returns at least LIST_BOTTOM_MARGIN_DP in pixels for the display's density, rounded as
# getDimensionPixelSize rounds. Android 17's list keeps 48 dp of its own
# (list_container_padding_bottom) and has no such method.
DOCUMENTSUI_APK = "/priv-app/DocumentsUI/DocumentsUI.apk"
_DOCS = "Lcom/android/documentsui"


def _documentsui_bar_height(bar, label):
    """Android 16's lookup of the bar below the list, in getSaveLayoutHeight: its height, or
    0 (v1) when the window has no such bar."""
    return [
        f"invoke-virtual {{v2}}, {_DOCS}/dirlist/DirectoryFragment;->getBaseActivity",
        "move-result-object v2",
        f"sget v0, {_DOCS}/R$id;->{bar}",
        f"invoke-static {{v0}}, {_DOCS}/util/Material3Config;->getRes",
        "move-result v0",
        "invoke-virtual {v2, v0}, Landroidx/appcompat/app/AppCompatActivity;->findViewById",
        "move-result-object v2",
        f"if-nez v2, :{label}",
        "return v1",
        f":{label}",
        "invoke-virtual {v2}, Landroid/view/View;->getHeight",
        "move-result v2",
        "return v2",
    ]


DOCUMENTSUI_CODE = {
    f"{_DOCS}/dirlist/DirectoryFragment;->getSaveLayoutHeight": ("replace code", {
        # Android 16 (BP4A.251205.006), as d8 compiled it: this is v2.
        "old": [
            f"invoke-static {{}}, {_DOCS}/util/FlagUtils;->isUseMaterial3FlagEnabled",
            "move-result v0",
            "const/4 v1, 0",
            "if-eqz v0, :legacy",
            *_documentsui_bar_height("bottom_container", "bottom"),
            ":legacy",
            *_documentsui_bar_height("container_save", "save"),
        ],
        # The same bar, then the larger of its height and the margin: this is v3.
        "registers": 4,
        "new": [
            f"invoke-static {{}}, {_DOCS}/util/FlagUtils;->isUseMaterial3FlagEnabled",
            "move-result v0",
            "if-eqz v0, :legacy",
            f"sget v0, {_DOCS}/R$id;->bottom_container",
            "goto :find",
            ":legacy",
            f"sget v0, {_DOCS}/R$id;->container_save",
            ":find",
            f"invoke-static {{v0}}, {_DOCS}/util/Material3Config;->getRes",
            "move-result v0",
            f"invoke-virtual {{v3}}, {_DOCS}/dirlist/DirectoryFragment;->getBaseActivity",
            "move-result-object v1",
            "invoke-virtual {v1, v0}, Landroidx/appcompat/app/AppCompatActivity;->findViewById",
            "move-result-object v1",
            "const/4 v2, 0",
            "if-eqz v1, :margin",
            "invoke-virtual {v1}, Landroid/view/View;->getHeight",
            "move-result v2",
            ":margin",
            "invoke-virtual {v3}, Landroidx/fragment/app/Fragment;->getResources",
            "move-result-object v0",
            "invoke-virtual {v0}, Landroid/content/res/Resources;->getDisplayMetrics",
            "move-result-object v0",
            "iget v0, v0, Landroid/util/DisplayMetrics;->density",
            f"const/high16 v1, {float_bits(LIST_BOTTOM_MARGIN_DP):#x}",
            "mul-float/2addr v0, v1",
            f"const/high16 v1, {float_bits(0.5):#x}",
            "add-float/2addr v0, v1",
            "float-to-int v0, v0",
            "if-ge v2, v0, :bar",
            "return v0",
            ":bar",
            "return v2",
        ],
    }),
}

# Cuttlefish's logger writes to a virtio console Hyper-V does not have, so it exits as soon as
# it starts. init starts it again every five seconds, and each exit runs flags_health_check.
# Nothing takes its place: Android's log stays in logd's memory, and the control daemon sends
# it to the host only while someone reads it (`dsa logcat`).
REMOVED_INIT_SCRIPTS = ["VENDOR/etc/init/seriallogging.rc"]

# The user's disk. The super partition's UUID is how first-stage init finds the disk, so it
# is fixed and passed on the kernel command line.
SUPER_UUID = "6b1ad5ad-0d5a-4dd3-9a6b-5a0a1d5a5a17"
MISC_UUID = "811d3bbb-a7ff-437a-857f-f54af996b190"
# The system disk's own GPT GUID. The kernel command line names the system disk by it, not by
# its kernel name (sda, sdb): a disk's kernel name follows the order Hyper-V's SCSI controller
# probes the disks in, which changes when the host attaches another disk.
SYSTEM_DISK_GUID = "d5a0d15c-5157-4e6d-8d5a-0000000005d5"
DISK_SIZE = 64 << 30
SYSTEM_SMALL_PARTITIONS = [
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

# The user's data disk, the one the VM can write. misc is here and not on the read-only
# system disk because the boot control HAL writes the boot state into it, and aborts when
# it cannot.
DATA_PARTITIONS = [
    ("misc", 1 << 20),
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

# ro.adb.secure is 0 because the guest has no SystemUI to show the prompt that authorises an
# adb key, so a secure adbd could never be used. adbd honours it on a user build because the
# boot is unlocked (androidboot.verifiedbootstate=orange). adbd itself stays stopped, and
# reachable only through the host's loopback bridge, until the user turns developer mode on.
USER_PROPS = {
    "ro.debuggable": "0",
    "ro.adb.secure": "0",
    "ro.secure": "1",
    "ro.force.debuggable": "0",
}

# adb: adbd listens on vsock only, on the port the host bridges to its loopback address, and
# never on TCP. The USB configuration does not start it: init starts and stops it as
# dsa-channeld sets ADB_PROPERTY, which follows the user's developer mode setting.
ADB_VSOCK_PORT = 5555
ADB_PROPS = {
    "service.adb.listen_addrs": f"vsock:{ADB_VSOCK_PORT}",
    "persist.sys.usb.config": "none",
}
# Cuttlefish's product properties make adbd listen on TCP port 5555 as well; that goes.
DROPPED_PROPS = {"persist.adb.tcp.port", "service.adb.tcp.port"}

# Shared memory goes through memfd: libcutils falls back to /dev/ashmem unless this is
# set, and DSA's kernel has no ashmem driver (its memfds answer the ashmem ioctls).
#
# The guest swaps to zram (fstab.dsa, dsa-memory.rc): idle apps' memory is kept compressed
# rather than resident, so the host gets more of it back. lmkd decides from memory pressure
# (PSI), and with swap it must also watch how full the swap is: it starts killing cached apps
# once less than a tenth of the swap is free or the swap is 90% used, instead of letting the
# guest thrash through a full zram. Thrashing past 30% of the time reclaiming counts as
# pressure too, as on Android Go devices, which have the same small-memory, zram-backed shape.
#
# ART keeps less free space in each process's heap, with the values of a 1 GiB phone: the
# heap is 75% full rather than 60% after a collection, with at most 8 MiB free. Cuttlefish's
# values are for a 4 GiB phone that has nothing else to do with its memory. The cost is more
# frequent collections in apps that allocate a lot. The limits on how far a heap may grow stay
# as they are, so no app has less memory to work with.
#
# ro.config.low_ram is not set, whatever it would save: it turns off multi-window, and every
# task here is a freeform window.
MEMORY_PROPS = {
    "sys.use_memfd": "true",
    "ro.lmk.use_psi": "true",
    "ro.lmk.use_minfree_levels": "false",
    "ro.lmk.swap_free_low_percentage": "10",
    "ro.lmk.swap_util_max": "90",
    "ro.lmk.thrashing_limit": "30",
    "dalvik.vm.heaptargetutilization": "0.75",
    "dalvik.vm.heapminfree": "512k",
    "dalvik.vm.heapmaxfree": "8m",
}

GRAPHICS_PROPS = {
    "ro.hardware.egl": "angle",
    "ro.hardware.vulkan": "ranchu",
    "ro.hardware.gralloc": "minigbm",
    "ro.cpuvulkan.version": "0",
}

# ro.vulkan.apex makes the Vulkan loader take Cuttlefish's driver from its APEX, which
# reaches the host only through virtio-gpu or the goldfish pipe; empty, the loader takes
# the vendor driver, which uses the Hyper-V socket. The APEX's init script sets it "on
# init", so build.prop, which init reads first, claims the read-only property empty.
LOADER_PROPS = {
    "ro.vulkan.apex": "",
}

# Factory reset protection is a phone's theft deterrent and needs an frp partition, which
# the DSA disks do not have. With ro.frp.pst empty, system_server starts neither
# PersistentDataBlockService nor, without an oemlock HAL, OemLockService.
FRP_PROPS = {
    "ro.frp.pst": "",
}

# logd keeps the kernel's log in its own buffer too, so `dsa logcat -b kernel` and the
# diagnostics have it. A user build leaves it out by default. The serial console carries the
# kernel's messages only until boot completes (dsa-console.rc).
LOG_PROPS = {
    "ro.logd.kernel": "true",
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


def aosp_test_certificates(otatools, root, target_files=None):
    """Lays AOSP's test certificates out at the paths META/apkcerts.txt names them by.

    sign_target_files_apks replaces those certificates with ours in every
    mac_permissions.xml, opening them relative to where it runs, and skips any it cannot
    open without a word. Running it in the returned folder lets it find them; otatools
    ships them under build/target, while apkcerts.txt names build/make/target.

    A certificate otatools does not ship (Bluetooth's, from packages/modules/Bluetooth) is
    taken from target_files' mac_permissions.xml, from the signer whose seinfo is the last
    part of the certificate's name (com.android.bluetooth: bluetooth).
    """
    otatools = Path(otatools)
    found = sorted(otatools.rglob("product/security/platform.x509.pem"))
    if not found:
        raise RuntimeError(f"{otatools} has no AOSP test certificates (product/security/platform.x509.pem)")
    root = Path(root)
    for prefix in ["build/make/target/product/security", "build/target/product/security"]:
        dest = root / prefix
        dest.mkdir(parents=True, exist_ok=True)
        for cert in found[0].parent.iterdir():
            if cert.is_file():
                shutil.copy2(cert, dest / cert.name)
    if target_files is None:
        return root

    with zipfile.ZipFile(target_files) as z:
        names = z.namelist()
        certificates = set(re.findall(r'certificate="([^"]+\.x509\.pem)"', z.read("META/apkcerts.txt").decode()))
        signers = {}
        for name in names:
            if name.endswith("mac_permissions.xml"):
                text = z.read(name).decode()
                for signature, body in re.findall(r'<signer signature="([0-9a-fA-F]+)"\s*>(.*?)</signer>', text, re.S):
                    for seinfo in re.findall(r'<seinfo value="([^"]+)"', body):
                        signers.setdefault(seinfo, signature)
    for certificate in sorted(certificates):
        path = root / certificate
        if path.exists():
            continue
        seinfo = Path(certificate).name[: -len(".x509.pem")].split(".")[-1]
        if seinfo not in signers:
            continue
        der = bytes.fromhex(signers[seinfo])
        body = base64.b64encode(der).decode()
        lines = [body[i : i + 64] for i in range(0, len(body), 64)]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("-----BEGIN CERTIFICATE-----\n" + "\n".join(lines) + "\n-----END CERTIFICATE-----\n")
    return root


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
    an AOSP super image changing size does not change the mapping by assumption. The disk is
    named by its GPT GUID, which DSA's kernel reads from the disk, so the names land on the
    system disk whichever kernel name it gets.
    """
    entries = []
    uuids = {} if uuids is None else uuids
    for name, (offset, size) in partitions.items():
        identifier = f"{name}:{uuids[name]}" if name in uuids else name
        entries.append(f"{size}@{offset}({identifier})")
    return f"blkdevparts={SYSTEM_DISK_GUID}:" + ",".join(entries)


# A disk's kernel name (sda, sdb) follows probe order, so nothing in the image may name one.
KERNEL_DISK_NAME = re.compile(r"/dev/(?:block/)?(?:sd|vd)[a-z]+\d*|blkdevparts=(?:[^ ;]*;)*(?:sd|vd)[a-z]+:")


def kernel_disk_names(text):
    """The disk references in `text` that name a disk by its kernel name, not its identity."""
    return KERNEL_DISK_NAME.findall(text)


def without_kernel_disk_entries(text):
    """Returns a mount table without the entries that name a disk by its kernel name. The
    build's own fstab.cf.* tables mount a Cuttlefish disk by probe order (/dev/block/vdc1);
    the VM has no such disk, and on Hyper-V that name could reach another one."""
    return "".join(line for line in text.splitlines(keepends=True)
                   if not kernel_disk_names(line))


def removed_apexes(hwcomposer=False, power=False, gnss=False, sensors=False,
                   camera=False):
    """The vendor APEXes the guest must not have; Cuttlefish's composers too when DSA's
    composer replaces them, its power and health HALs when DSA's replace them, its GNSS HAL
    when DSA's replaces it, its sensors HAL when DSA's replaces it, and its emulated cameras
    when DSA's camera HAL lends the host's."""
    return (REMOVED_APEXES + (CUTTLEFISH_COMPOSER_APEXES if hwcomposer else [])
            + (CUTTLEFISH_POWER_APEXES if power else [])
            + (CUTTLEFISH_GNSS_APEXES if gnss else [])
            + (CUTTLEFISH_SENSORS_APEXES if sensors else [])
            + (CUTTLEFISH_CAMERA_APEXES if camera else []))


def removed(name, hwcomposer=False, power=False, gnss=False, sensors=False, camera=False):
    """Whether a target_files member belongs to an app, a HAL or a service the guest must not have."""
    return (
        name in REMOVED_INIT_SCRIPTS
        or any(f"/{app}/" in name for app in REMOVED_APPS)
        or any(
            name in (f"VENDOR/apex/{apex}.apex", f"VENDOR/apex/{apex}.capex")
            for apex in removed_apexes(hwcomposer, power, gnss, sensors, camera)
        )
        or (gnss and cuttlefish_gnss_file(name))
    )


def cuttlefish_gnss_file(name):
    """Whether a target_files member is part of a GNSS HAL installed as plain files (its
    binary, init script or VINTF fragment), other than DSA's own."""
    if name in GNSS_FILES:
        return False
    return any(name.startswith(folder) and "gnss" in name[len(folder):].lower()
               for folder in CUTTLEFISH_GNSS_FOLDERS)


def other_sensors_manifest(name, data):
    """Whether a target_files member is a vendor VINTF fragment, not DSA's, that declares a
    sensors HAL. Two declarations of android.hardware.sensors would fail the device's VINTF
    check, so the other one goes when DSA's sensors HAL is installed."""
    return (
        name.startswith("VENDOR/etc/vintf/manifest/")
        and name not in SENSORS_FILES
        and b"<name>android.hardware.sensors</name>" in data
    )


VENDOR_APEX_DIR = "VENDOR/apex/"


def _varint(data, at):
    value = shift = 0
    while at < len(data):
        byte = data[at]
        at += 1
        value |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return value, at
        shift += 7
    raise ValueError("truncated varint")


def apex_module_name(data):
    """The module name in an APEX's (or a compressed APEX's) apex_manifest.pb, or None.

    Variants of one module share it: the nonsecure and the remote KeyMint, the emulated
    cameras and their fastscenecycle build. apexd activates one of them, the one bootconfig
    selects, or whichever is left when nothing selects one."""
    try:
        manifest = zipfile.ZipFile(io.BytesIO(data)).read("apex_manifest.pb")
    except (zipfile.BadZipFile, KeyError):
        return None
    try:
        at = 0
        while at < len(manifest):
            key, at = _varint(manifest, at)
            field, wire = key >> 3, key & 7
            if wire == 2:
                length, at = _varint(manifest, at)
                value = manifest[at:at + length]
                at += length
                if field == 1:
                    return value.decode(errors="replace")
            elif wire == 0:
                _, at = _varint(manifest, at)
            elif wire == 5:
                at += 4
            elif wire == 1:
                at += 8
            else:
                return None
    except ValueError:
        return None
    return None


def vendor_apex_members(names):
    """The vendor APEXes in target_files, by member name."""
    return sorted(n for n in names if n.startswith(VENDOR_APEX_DIR) and n.endswith((".apex", ".capex")))


def apex_file_stem(name):
    """com.google.cf.gnss for VENDOR/apex/com.google.cf.gnss.apex."""
    return name[len(VENDOR_APEX_DIR):].rsplit(".", 1)[0]


def replaced_apex_variants(names, read, removed_names):
    """The vendor APEXes that removed_names does not list but that belong to a module one of
    them belongs to. Removing one variant of a module makes apexd activate another, which
    declares the same HAL as the one that replaced it, and a second declaration of an instance
    fails the whole vendor VINTF manifest: no vendor HAL can register then."""
    removed_names = set(removed_names)
    members = vendor_apex_members(names)
    modules = set(removed_names)
    for name in members:
        if apex_file_stem(name) in removed_names:
            modules.add(apex_module_name(read(name)))
    modules.discard(None)
    return [name for name in members
            if apex_file_stem(name) not in removed_names and apex_module_name(read(name)) in modules]


VINTF_HAL = re.compile(rb"<hal\b([^>]*)>(.*?)</hal>", re.S)
VINTF_COMMENT = re.compile(rb"<!--.*?-->", re.S)


def vintf_instances(data):
    """Every HAL instance a VINTF manifest (or a raw image holding manifests) declares, as
    (format, package, interface, instance). AIDL instances conflict whatever their version;
    HIDL ones only within a major version, which joins the package name."""
    found = set()
    for attributes, body in VINTF_HAL.findall(VINTF_COMMENT.sub(b"", data)):
        fmt = re.search(rb'format="(\w+)"', attributes)
        fmt = fmt.group(1).decode() if fmt else "hidl"
        if fmt not in ("aidl", "hidl"):
            continue
        name = re.search(rb"<name>\s*([^<\s]+)\s*</name>", body)
        if not name:
            continue
        package = name.group(1).decode()
        version = re.search(rb"<version>\s*([0-9]+)", body)
        for fq in re.findall(rb"<fqname>\s*([^<\s]+)\s*</fqname>", body):
            fq = fq.decode()
            major = None
            if fq.startswith("@"):
                major = fq[1:].split(".", 1)[0]
                fq = fq.split("::", 1)[1]
            interface, _, instance = fq.partition("/")
            if fmt == "hidl":
                package_key = f"{package}@{major or (version.group(1).decode() if version else '?')}"
            else:
                package_key = package
            found.add((fmt, package_key, interface, instance))
        for interface_body in re.findall(rb"<interface>(.*?)</interface>", body, re.S):
            interface = re.search(rb"<name>\s*([^<\s]+)\s*</name>", interface_body)
            if not interface:
                continue
            package_key = package if fmt == "aidl" else f"{package}@{version.group(1).decode() if version else '?'}"
            for instance in re.findall(rb"<instance>\s*([^<\s]+)\s*</instance>", interface_body):
                found.add((fmt, package_key, interface.group(1).decode(), instance.decode()))
    return found


# The instance each DSA vendor service registers with servicemanager, by its binary.
# servicemanager refuses a VINTF-stable service no manifest declares, and the service exits.
# dsa-audio serves the primary module the image's audio APEX declares; the rest declare their
# own.
DSA_SERVED_INSTANCES = {
    "VENDOR/bin/dsa-channeld": ("aidl", "io.github.thisisthepy.dsa.channel", "IChannel", "default"),
    "VENDOR/bin/hw/dsa-hwcomposer": ("aidl", "android.hardware.graphics.composer3", "IComposer", "default"),
    "VENDOR/bin/hw/dsa-power": ("aidl", "android.hardware.power", "IPower", "default"),
    "VENDOR/bin/hw/dsa-health": ("aidl", "android.hardware.health", "IHealth", "default"),
    "VENDOR/bin/hw/dsa-gnss": ("aidl", "android.hardware.gnss", "IGnss", "default"),
    "VENDOR/bin/hw/dsa-codec2": ("aidl", "android.hardware.media.c2", "IComponentStore", "default"),
    "VENDOR/bin/hw/dsa-sensors": ("aidl", "android.hardware.sensors", "ISensors", "default"),
    "VENDOR/bin/hw/dsa-camera": ("aidl", "android.hardware.camera.provider", "ICameraProvider", "internal/0"),
    "VENDOR/bin/hw/dsa-audio": ("aidl", "android.hardware.audio.core", "IModule", "default"),
}


def vendor_vintf_problems(names, read, cmdline):
    """What would fail the device's VINTF manifest, or leave a DSA service undeclared, as lines
    for check-image.py. libvintf merges the vendor manifest, its fragments and the fragments
    of every active vendor APEX, and one instance declared twice fails the whole manifest:
    servicemanager then has no device manifest at all, and every vendor HAL exits.

    An APEX counts as active when bootconfig (`cmdline`) selects it for its module, or when
    nothing selects one for that module, as apexd then activates whichever is there."""
    problems = []
    selected = {}
    for part in cmdline.split():
        if part.startswith("androidboot.vendor.apex.") and "=" in part:
            module, value = part[len("androidboot.vendor.apex."):].split("=", 1)
            selected[module] = value
    sources = []
    for name in sorted(names):
        if ((name.startswith(("VENDOR/etc/vintf/", "ODM/etc/vintf/")) and name.endswith(".xml")
             and "compatibility_matrix" not in name)):
            sources.append((name, read(name)))
    for name in vendor_apex_members(names):
        data = read(name)
        module = apex_module_name(data)
        if module in selected and selected[module] != apex_file_stem(name):
            continue
        try:
            outer = zipfile.ZipFile(io.BytesIO(data))
            if CAPEX_ORIGINAL in outer.namelist():
                outer = zipfile.ZipFile(io.BytesIO(outer.read(CAPEX_ORIGINAL)))
            payload = outer.read("apex_payload.img")
        except (zipfile.BadZipFile, KeyError):
            continue
        sources.append((name, payload))
    declared = {}
    for source, data in sources:
        for instance in vintf_instances(data):
            declared.setdefault(instance, []).append(source)
    for (fmt, package, interface, instance), where in sorted(declared.items()):
        if len(where) > 1:
            problems.append(f"{package}.{interface}/{instance} is declared by {', '.join(where)}; two "
                            "declarations fail the vendor VINTF manifest and no vendor HAL can register")
    for binary, instance in DSA_SERVED_INSTANCES.items():
        if binary in names and instance not in declared:
            problems.append(f"no vendor manifest declares {instance[1]}.{instance[2]}/{instance[3]}, so "
                            f"servicemanager refuses {binary.rsplit('/', 1)[-1]} and it exits")
    return problems


def framework_overlay_sources(dest):
    """Writes the framework overlay's manifest and values into `dest`."""
    (dest / "res/values").mkdir(parents=True, exist_ok=True)
    (dest / "AndroidManifest.xml").write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<manifest xmlns:android="http://schemas.android.com/apk/res/android"\n'
        f'    package="{FRAMEWORK_OVERLAY_PACKAGE}">\n'
        # Android 14 and later refuse a package that targets an old SDK, and the default is 1.
        '    <uses-sdk android:minSdkVersion="34" android:targetSdkVersion="35" />\n'
        '    <application android:hasCode="false" />\n'
        '    <overlay android:targetPackage="android" android:isStatic="true" android:priority="100" />\n'
        '</manifest>\n'
    )
    integers = "".join(f'    <integer name="{name}">{value}</integer>\n'
                       for name, value in FRAMEWORK_OVERLAY_INTEGERS.items())
    (dest / "res/values/dsa.xml").write_text(
        f'<?xml version="1.0" encoding="utf-8"?>\n<resources>\n{integers}</resources>\n')


def build_framework_overlay(aapt2, framework_res, work):
    """Builds the framework overlay APK (unsigned; signing gives it the platform key)."""
    sources = work / "framework-overlay"
    shutil.rmtree(sources, ignore_errors=True)
    framework_overlay_sources(sources)
    compiled = sources / "compiled.zip"
    apk = sources / "DsaFrameworkOverlay.apk"
    run(aapt2, "compile", "--dir", sources / "res", "-o", compiled)
    run(aapt2, "link", "-o", apk, "--manifest", sources / "AndroidManifest.xml", "-I", framework_res, compiled)
    return apk


def settings_overlay_sources(dest, home_padding=False):
    """Writes the Settings overlay's manifest, its resource map, its back button drawables and
    its preference list layout into `dest`, and with `home_padding` the home list's bottom
    padding (SETTINGS_HOME_PADDING_DIMEN), for a Settings that has it."""
    overlaid = SETTINGS_OVERLAID + ([f"dimen/{SETTINGS_HOME_PADDING_DIMEN}"] if home_padding else [])
    (dest / "res").mkdir(parents=True, exist_ok=True)
    (dest / "AndroidManifest.xml").write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<manifest xmlns:android="http://schemas.android.com/apk/res/android"\n'
        f'    package="{SETTINGS_OVERLAY_PACKAGE}">\n'
        '    <uses-sdk android:minSdkVersion="34" android:targetSdkVersion="35" />\n'
        '    <application android:hasCode="false" />\n'
        f'    <overlay android:targetPackage="{SETTINGS_OVERLAY_TARGET}" android:isStatic="true" '
        f'android:priority="100" android:resourcesMap="@xml/{SETTINGS_OVERLAY_MAP}" />\n'
        '</manifest>\n'
    )
    for folder, theme in (("drawable", "light"), ("drawable-night", "dark")):
        (dest / "res" / folder).mkdir(exist_ok=True)
        (dest / "res" / folder / f"{SETTINGS_BACK_ICON}.xml").write_text(settings_back_icon(theme))
    (dest / "res/layout").mkdir(exist_ok=True)
    (dest / "res/layout" / f"{SETTINGS_LIST_LAYOUT}.xml").write_text(settings_preference_list())
    (dest / "res/values").mkdir(exist_ok=True)
    (dest / "res/values/dsa.xml").write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n<resources>\n'
        f'    <attr name="{SETTINGS_LIST_STYLE_ATTR}" format="reference" />\n'
        + (f'    <dimen name="{SETTINGS_HOME_PADDING_DIMEN}">{SETTINGS_LIST_BOTTOM_MARGIN_DP}dp</dimen>\n'
           if home_padding else '')
        + '</resources>\n')
    (dest / "res/xml").mkdir(exist_ok=True)
    items = "".join(f'    <item target="{name}" value="@{name}" />\n' for name in overlaid)
    (dest / "res/xml" / f"{SETTINGS_OVERLAY_MAP}.xml").write_text(
        f'<?xml version="1.0" encoding="utf-8"?>\n<overlay>\n{items}</overlay>\n')


def settings_preference_list():
    """The RecyclerView every preference screen in Settings lists its rows in.

    androidx.preference's own layout, with room below the last row. Settings takes the space
    under a list from the navigation bar alone: SettingsLib's EdgeToEdgeUtils pads the
    content view by the system bar inset, and the list's own bottom padding is 0. A DSA
    window has no navigation bar, so the last row met the window's bottom edge. The padding
    is inside the list (clipToPadding is false), so rows still scroll to the edge and only
    the end of the list stops short of it. The id and the style attribute are Settings' own:
    the resource map rewrites the overlay's references to them to Settings' ids."""
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<androidx.recyclerview.widget.RecyclerView\n'
        '    xmlns:android="http://schemas.android.com/apk/res/android"\n'
        '    android:id="@+id/recycler_view"\n'
        '    android:layout_width="match_parent"\n'
        '    android:layout_height="match_parent"\n'
        '    android:paddingTop="0dp"\n'
        f'    android:paddingBottom="{SETTINGS_LIST_BOTTOM_MARGIN_DP}dp"\n'
        '    android:clipToPadding="false"\n'
        f'    style="?attr/{SETTINGS_LIST_STYLE_ATTR}" />\n'
    )


def settings_back_icon(theme):
    """The toolbar back icon with its circle centred in the 56 dp navigation button.

    SettingsLib's own icon is a 56 dp layer list whose 40 dp circle starts 16 dp in, so the
    circle's centre sits 8 dp right of the button's. The framework draws the button's hover
    layer and ripple around the button's centre, 8 dp left of the circle. Here the circle has
    8 dp on each side: the same 56 dp, so the button and the title keep their places."""
    inset = f'android:start="{SETTINGS_BACK_INSET_DP}dp" android:end="{SETTINGS_BACK_INSET_DP}dp"'
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<layer-list xmlns:android="http://schemas.android.com/apk/res/android">\n'
        f'    <item {inset}>\n'
        '        <shape>\n'
        '            <size android:width="40dp" android:height="40dp" />\n'
        f'            <solid android:color="@*android:color/system_surface_container_highest_{theme}" />\n'
        '            <corners android:radius="100dp" />\n'
        '        </shape>\n'
        '    </item>\n'
        f'    <item android:width="16dp" android:height="16dp" android:gravity="center" {inset}>\n'
        '        <vector android:width="16dp" android:height="16dp"\n'
        '            android:viewportWidth="16" android:viewportHeight="16"\n'
        f'            android:tint="@*android:color/system_on_surface_variant_{theme}"\n'
        '            android:autoMirrored="true">\n'
        f'            <path android:fillColor="@android:color/white" android:pathData="{SETTINGS_BACK_ARROW}" />\n'
        '        </vector>\n'
        '    </item>\n'
        '</layer-list>\n'
    )


def build_settings_overlay(aapt2, framework_res, work, home_padding=False):
    """Builds the Settings overlay APK (unsigned; signing gives it the platform key)."""
    sources = work / "settings-overlay"
    shutil.rmtree(sources, ignore_errors=True)
    settings_overlay_sources(sources, home_padding)
    compiled = sources / "compiled.zip"
    apk = sources / "DsaSettingsOverlay.apk"
    run(aapt2, "compile", "--dir", sources / "res", "-o", compiled)
    run(aapt2, "link", "-o", apk, "--manifest", sources / "AndroidManifest.xml", "-I", framework_res, compiled)
    return apk


def settings_has_home_padding(apk):
    """Whether a Settings.apk pads its home list by SETTINGS_HOME_PADDING_DIMEN (Android 17's
    does; Android 16's has no such dimen, and an overlay of it would only log a warning)."""
    field = f"Lcom/android/settings/R$dimen;->{SETTINGS_HOME_PADDING_DIMEN}"
    with zipfile.ZipFile(io.BytesIO(apk)) as z:
        for dex in (n for n in z.namelist() if re.fullmatch(r"classes\d*\.dex", n)):
            content = z.read(dex)
            if content[:4] == b"dex\n" and field in dex_ids(content)[2]:
                return True
    return False


def find_aapt2():
    """aapt2 from the Android SDK's newest build-tools, or None."""
    for variable in ("ANDROID_HOME", "ANDROID_SDK_ROOT"):
        sdk = os.environ.get(variable)
        if sdk:
            found = sorted(Path(sdk).glob("build-tools/*/aapt2"),
                           key=lambda p: [int(x) if x.isdigit() else 0 for x in re.split(r"[.-]", p.parent.name)])
            if found:
                return found[-1]
    return None


# Android's compiled XML (ResXMLTree): a file of chunks, each with a type, a header size and
# a size. The tree is a string pool, a resource map, then one node chunk per namespace,
# element start, element end and text.
XML_FILE = 0x0003
XML_STRING_POOL = 0x0001
XML_START_ELEMENT = 0x0102
XML_END_ELEMENT = 0x0103
XML_NO_INDEX = 0xFFFFFFFF
XML_TYPE_STRING = 0x03
XML_UTF8 = 0x100


def xml_strings(axml, offset):
    """The strings of the compiled XML string pool at `offset`."""
    _, header_size, _, count, _, flags, strings_start, _ = struct.unpack_from("<HHIIIIII", axml, offset)
    utf8 = flags & XML_UTF8
    strings = []
    for i in range(count):
        (start,) = struct.unpack_from("<I", axml, offset + header_size + 4 * i)
        at = offset + strings_start + start
        if utf8:
            # The length in characters, then in bytes; each is one byte, or two with the high bit set.
            for _ in range(2):
                length = axml[at]
                at += 1
                if length & 0x80:
                    length = ((length & 0x7F) << 8) | axml[at]
                    at += 1
            strings.append(axml[at:at + length].decode("utf-8"))
        else:
            (length,) = struct.unpack_from("<H", axml, at)
            at += 2
            if length & 0x8000:
                (low,) = struct.unpack_from("<H", axml, at)
                length = ((length & 0x7FFF) << 16) | low
                at += 2
            strings.append(axml[at:at + 2 * length].decode("utf-16-le"))
    return strings


def remove_xml_elements(axml, attribute, values):
    """Removes from compiled XML each element, with everything inside it, whose `attribute`
    (in any namespace) is one of `values`. Returns the new file and the values it removed."""
    file_type, file_header, _ = struct.unpack_from("<HHI", axml, 0)
    if file_type != XML_FILE:
        raise ValueError("not a compiled Android XML file")
    strings = []
    kept = [axml[:file_header]]
    removed_values = []
    skipping = 0
    offset = file_header
    while offset < len(axml):
        chunk_type, header_size, size = struct.unpack_from("<HHI", axml, offset)
        chunk = axml[offset:offset + size]
        if chunk_type == XML_STRING_POOL:
            strings = xml_strings(axml, offset)
        if skipping:
            if chunk_type == XML_START_ELEMENT:
                skipping += 1
            elif chunk_type == XML_END_ELEMENT:
                skipping -= 1
        elif chunk_type == XML_START_ELEMENT:
            attribute_start, attribute_size, attribute_count = struct.unpack_from(
                "<HHH", axml, offset + header_size + 8)
            for i in range(attribute_count):
                at = offset + header_size + attribute_start + i * attribute_size
                _, name, raw, _, _, data_type, data = struct.unpack_from("<IIIHBBI", axml, at)
                index = raw if raw != XML_NO_INDEX else (data if data_type == XML_TYPE_STRING else None)
                if strings[name] == attribute and index is not None and strings[index] in values:
                    removed_values.append(strings[index])
                    skipping = 1
                    break
            if not skipping:
                kept.append(chunk)
        else:
            kept.append(chunk)
        offset += size
    result = b"".join(kept)
    return result[:4] + struct.pack("<I", len(result)) + result[8:], removed_values


XML_RESOURCE_MAP = 0x0180
XML_TYPE_DIMENSION = 0x05
XML_COMPLEX_UNIT_DIP = 0x01
ANDROID_NS = "http://schemas.android.com/apk/res/android"


def xml_dimension_dp(dp):
    """A whole number of dp as compiled XML stores a dimension: the value, then its unit."""
    return dp << 8 | XML_COMPLEX_UNIT_DIP


def set_xml_dimensions(axml, edits):
    """Compiled XML with, for each (element, attribute, dp) in `edits`, that framework
    attribute (by resource id, such as android:paddingBottom) of every `element` set to `dp`
    dp, added in its place where the element lacks it. An added attribute's name must already
    be in the file, as aapt2 keeps one name per attribute any element uses. Returns the new
    file and the number of attributes it changed or added."""
    file_type, file_header, _ = struct.unpack_from("<HHI", axml, 0)
    if file_type != XML_FILE:
        raise ValueError("not a compiled Android XML file")
    strings, resource_ids = [], []
    out = [axml[:file_header]]
    changed = 0
    offset = file_header
    while offset < len(axml):
        chunk_type, header_size, size = struct.unpack_from("<HHI", axml, offset)
        chunk = bytearray(axml[offset:offset + size])
        if chunk_type == XML_STRING_POOL:
            strings = xml_strings(axml, offset)
        elif chunk_type == XML_RESOURCE_MAP:
            entries = (size - header_size) // 4
            resource_ids = list(struct.unpack_from(f"<{entries}I", axml, offset + header_size))
        elif chunk_type == XML_START_ELEMENT:
            _, name, attribute_start, attribute_size, count = struct.unpack_from("<IIHHH", chunk, header_size)
            for element, attribute, dp in edits:
                if strings[name] != element:
                    continue
                value = xml_dimension_dp(dp)
                first = header_size + attribute_start
                ids = []
                for i in range(count):
                    _, attribute_name = struct.unpack_from("<II", chunk, first + i * attribute_size)
                    ids.append(resource_ids[attribute_name] if attribute_name < len(resource_ids) else None)
                if attribute in ids:
                    at = first + ids.index(attribute) * attribute_size
                    if struct.unpack_from("<BI", chunk, at + 15) == (XML_TYPE_DIMENSION, value):
                        continue
                    struct.pack_into("<IHBBI", chunk, at + 8, XML_NO_INDEX, 8, 0, XML_TYPE_DIMENSION, value)
                    changed += 1
                    continue
                if attribute not in resource_ids or ANDROID_NS not in strings:
                    raise ValueError(f"the compiled XML names no attribute {attribute:#x} to add to {element}")
                # aapt2 orders an element's attributes by resource id, and the framework reads
                # them in step with the attributes a view asks for, which are in that order too.
                position = sum(1 for i in ids if i is not None and i < attribute)
                entry = struct.pack("<IIIHBBI", strings.index(ANDROID_NS), resource_ids.index(attribute),
                                    XML_NO_INDEX, 8, 0, XML_TYPE_DIMENSION, value)
                at = first + position * attribute_size
                chunk[at:at] = entry + bytes(attribute_size - len(entry))
                count += 1
                struct.pack_into("<H", chunk, header_size + 12, count)
                # The id, class and style attributes are named by their position, from 1.
                for field in (14, 16, 18):
                    (index,) = struct.unpack_from("<H", chunk, header_size + field)
                    if index > position:
                        struct.pack_into("<H", chunk, header_size + field, index + 1)
                struct.pack_into("<I", chunk, 4, len(chunk))
                changed += 1
        out.append(bytes(chunk))
        offset += size
    result = b"".join(out)
    return result[:4] + struct.pack("<I", len(result)) + result[8:], changed


def _uleb128(data, at):
    value = shift = 0
    while True:
        byte = data[at]
        at += 1
        value |= (byte & 0x7F) << shift
        shift += 7
        if byte < 0x80:
            return value, at


# Width in 16-bit code units of each Dalvik opcode (the payload pseudo-instructions are
# handled where they are met).
DEX_INSTRUCTION_UNITS = [1] * 256
for _ops, _units in [
    ((0x02, 0x05, 0x08, 0x13, 0x15, 0x16, 0x19, 0x1A, 0x1C, 0x1F, 0x20, 0x22, 0x23, 0x29, 0xFE, 0xFF), 2),
    ((0x03, 0x06, 0x09, 0x14, 0x17, 0x1B, 0x24, 0x25, 0x26, 0x2A, 0x2B, 0x2C, 0xFC, 0xFD), 3),
    ((0x18,), 5),
    ((0xFA, 0xFB), 4),
    (tuple(range(0x2D, 0x3E)) + tuple(range(0x44, 0x6E)) + tuple(range(0x90, 0xB0)) + tuple(range(0xD0, 0xE3)), 2),
    (tuple(range(0x6E, 0x73)) + tuple(range(0x74, 0x79)), 3),
]:
    for _op in _ops:
        DEX_INSTRUCTION_UNITS[_op] = _units
DEX_INVOKES = set(range(0x6E, 0x73)) | set(range(0x74, 0x79))
DEX_MOVE_RESULTS = {0x0A, 0x0B, 0x0C}


def dex_ids(dex):
    """The "Lclass;->name" of each method a dex file refers to and its shorty, by method
    index, and the "Lclass;->name" of each field it refers to, by field index."""
    if dex[:4] != b"dex\n":
        raise ValueError("not a dex file")
    (_, string_off, type_count, type_off, proto_count, proto_off, field_count, field_off,
     method_count, method_off) = struct.unpack_from("<10I", dex, 56)

    def string(index):
        _, at = _uleb128(dex, struct.unpack_from("<I", dex, string_off + 4 * index)[0])
        return dex[at:dex.index(0, at)].decode("utf-8", "replace")

    types = [string(struct.unpack_from("<I", dex, type_off + 4 * i)[0]) for i in range(type_count)]
    shorties = [string(struct.unpack_from("<I", dex, proto_off + 12 * i)[0]) for i in range(proto_count)]
    refs, method_shorty, fields = [], [], []
    for i in range(method_count):
        class_index, proto_index, name_index = struct.unpack_from("<HHI", dex, method_off + 8 * i)
        refs.append(f"{types[class_index]}->{string(name_index)}")
        method_shorty.append(shorties[proto_index])
    for i in range(field_count):
        class_index, _, name_index = struct.unpack_from("<HHI", dex, field_off + 8 * i)
        fields.append(f"{types[class_index]}->{string(name_index)}")
    return refs, method_shorty, fields


def dex_methods(dex):
    """{"Lclass;->name": [(code_off, shorty)]} for each method a dex file defines with code,
    and the "Lclass;->name" of each method it refers to, by method index."""
    refs, method_shorty, _ = dex_ids(dex)
    class_count, class_off = struct.unpack_from("<2I", dex, 96)
    defined = {}
    for i in range(class_count):
        data_off = struct.unpack_from("<I", dex, class_off + 32 * i + 24)[0]
        if not data_off:
            continue
        counts = []
        at = data_off
        for _ in range(4):
            count, at = _uleb128(dex, at)
            counts.append(count)
        for _ in range(2 * (counts[0] + counts[1])):
            _, at = _uleb128(dex, at)
        for group in (counts[2], counts[3]):
            index = 0
            for _ in range(group):
                delta, at = _uleb128(dex, at)
                index += delta
                _, at = _uleb128(dex, at)
                code_off, at = _uleb128(dex, at)
                if code_off:
                    defined.setdefault(refs[index], []).append((code_off, method_shorty[index]))
    return defined, refs


def dex_instructions(dex, code_off):
    """(offset, opcode) of each instruction in a code item, skipping switch and array data."""
    count = struct.unpack_from("<I", dex, code_off + 12)[0]
    start = code_off + 16
    unit = 0
    while unit < count:
        at = start + 2 * unit
        first = struct.unpack_from("<H", dex, at)[0]
        if first == 0x0100:
            unit += 4 + 2 * struct.unpack_from("<H", dex, at + 2)[0]
        elif first == 0x0200:
            unit += 2 + 4 * struct.unpack_from("<H", dex, at + 2)[0]
        elif first == 0x0300:
            width, size = struct.unpack_from("<HI", dex, at + 2)
            unit += 4 + (width * size + 1) // 2
        else:
            yield at, first & 0xFF
            unit += DEX_INSTRUCTION_UNITS[first & 0xFF]


# The Dalvik instructions the code replacements below are written in, by mnemonic: the
# opcode and the instruction format, named as the Dalvik executable format names them.
DEX_ASSEMBLY = {
    "nop": (0x00, "10x"), "move": (0x01, "12x"), "move-result": (0x0A, "11x"),
    "move-result-object": (0x0C, "11x"), "return-void": (0x0E, "10x"), "return": (0x0F, "11x"),
    "return-object": (0x11, "11x"), "const/4": (0x12, "11n"), "const/high16": (0x15, "21h"),
    "goto": (0x28, "10t"), "if-ge": (0x35, "22t"), "if-eqz": (0x38, "21t"),
    "if-nez": (0x39, "21t"),
    "iget": (0x52, "22c"), "iget-object": (0x54, "22c"), "sget": (0x60, "21c"),
    "invoke-virtual": (0x6E, "35c"), "invoke-super": (0x6F, "35c"), "invoke-direct": (0x70, "35c"),
    "invoke-static": (0x71, "35c"), "float-to-int": (0x87, "12x"),
    "add-float/2addr": (0xC6, "12x"), "mul-float/2addr": (0xC8, "12x"),
}
DEX_FORMAT_UNITS = {"10x": 1, "11x": 1, "12x": 1, "11n": 1, "10t": 1, "21t": 2, "22t": 2, "21h": 2,
                    "21c": 2, "22c": 2, "35c": 3}


def assemble_dex(lines, dex):
    """The code of `lines` in Dalvik assembly, one instruction or ":label" each, written as
    smali writes them ("invoke-virtual {v1, v0}, Lclass;->method", "if-eqz v0, :label"). Each
    method and field is named "Lclass;->name" and must be one `dex` refers to exactly once.
    Returns the code and the most registers a call in it passes."""
    refs, _, fields = dex_ids(dex)

    def index(table, name):
        found = [i for i, ref in enumerate(table) if ref == name]
        if len(found) != 1:
            raise ValueError(f"the dex file refers to {name} {len(found)} times, not once")
        return found[0]

    def register(text, limit):
        if not re.fullmatch(r"v\d+", text) or int(text[1:]) > limit:
            raise ValueError(f"{text} is not a register this instruction can name")
        return int(text[1:])

    parsed, labels, unit = [], {}, 0
    for line in lines:
        if line.startswith(":"):
            labels[line[1:]] = unit
            continue
        mnemonic, _, rest = line.partition(" ")
        opcode, form = DEX_ASSEMBLY[mnemonic]
        registers = []
        if rest.startswith("{"):
            inside, _, rest = rest[1:].partition("}")
            registers = [r.strip() for r in inside.split(",") if r.strip()]
        operands = registers + [a.strip() for a in rest.split(",") if a.strip()]
        parsed.append((unit, line, opcode, form, operands, len(registers)))
        unit += DEX_FORMAT_UNITS[form]

    code, outs = [], 0
    for at, line, opcode, form, operands, count in parsed:
        def target(text):
            offset = labels[text.lstrip(":")] - at
            if offset == 0:
                raise ValueError(f"{line} branches to itself")
            return offset

        if form == "10x":
            code += [opcode]
        elif form == "11x":
            code += [opcode | register(operands[0], 255) << 8]
        elif form == "12x":
            code += [opcode | register(operands[0], 15) << 8 | register(operands[1], 15) << 12]
        elif form == "11n":
            value = int(operands[1], 0)
            if not -8 <= value <= 7:
                raise ValueError(f"{line}: const/4 holds -8 to 7")
            code += [opcode | register(operands[0], 15) << 8 | (value & 0xF) << 12]
        elif form == "10t":
            offset = target(operands[0])
            if not -128 <= offset <= 127:
                raise ValueError(f"{line}: goto reaches 127 code units")
            code += [opcode | (offset & 0xFF) << 8]
        elif form == "21t":
            code += [opcode | register(operands[0], 255) << 8, target(operands[1]) & 0xFFFF]
        elif form == "22t":
            code += [opcode | register(operands[0], 15) << 8 | register(operands[1], 15) << 12,
                     target(operands[2]) & 0xFFFF]
        elif form == "21h":
            value = int(operands[1], 0)
            if value & 0xFFFF:
                raise ValueError(f"{line}: const/high16 holds only the high 16 bits")
            code += [opcode | register(operands[0], 255) << 8, value >> 16]
        elif form == "21c":
            code += [opcode | register(operands[0], 255) << 8, index(fields, operands[1])]
        elif form == "22c":
            code += [opcode | register(operands[0], 15) << 8 | register(operands[1], 15) << 12,
                     index(fields, operands[2])]
        elif form == "35c":
            if count > 5:
                raise ValueError(f"{line}: a call passes at most five registers")
            args = [register(r, 15) for r in operands[:count]] + [0] * (5 - count)
            outs = max(outs, count)
            code += [opcode | args[4] << 8 | count << 12, index(refs, operands[count]),
                     args[0] | args[1] << 4 | args[2] << 8 | args[3] << 12]
    return struct.pack(f"<{len(code)}H", *code), outs


def edit_dex(dex, edits):
    """Applies {"Lclass;->method": edit, or a list of edits} to a dex file. An edit is
    ("return false", None) or ("return true", None), which makes a method that returns a
    boolean return that at once; ("skip call", "Lclass;->method"), which turns each call to
    that method, whose result is not used, into no-ops; or ("replace code", {"registers": n,
    "old": [...], "new": [...]}), which replaces a method whose code is exactly `old` with
    `new` (Dalvik assembly, see assemble_dex) in n registers, padded with no-ops to the same
    size. Returns the new file and the methods it changed. A method already edited, or not in
    this file, is not changed; one whose code is neither `old` nor `new` is refused."""
    defined, refs = dex_methods(dex)
    out = bytearray(dex)
    changed = []
    for method, kind, target in (
            (method, kind, target)
            for method, edit in edits.items()
            for kind, target in (edit if isinstance(edit, list) else [edit])):
        for code_off, shorty in defined.get(method, []):
            registers, ins, outs, tries = struct.unpack_from("<4H", dex, code_off)
            insns = code_off + 16
            if kind in ("return false", "return true"):
                if shorty[0] != "Z" or registers < 1:
                    raise ValueError(f"{method} does not return a boolean")
                # const/4 v0, 0 (or 1); return v0, over whole instructions, the rest of which
                # become no-ops so the verifier still reads valid code. What follows is never
                # reached.
                returned = b"\x12\x00\x0f\x00" if kind == "return false" else b"\x12\x10\x0f\x00"
                if bytes(out[insns:insns + 4]) == returned:
                    continue
                end = insns
                # A move-result left behind without its call would not verify, so it goes too.
                for at, op in dex_instructions(dex, code_off):
                    if end - insns >= 4 and op not in DEX_MOVE_RESULTS:
                        break
                    end = at + 2 * DEX_INSTRUCTION_UNITS[op]
                if end - insns < 4:
                    raise ValueError(f"{method} is too short to {kind}")
                out[insns:end] = returned + bytes(end - insns - 4)
                changed.append(method)
            elif kind == "skip call":
                instructions = list(dex_instructions(dex, code_off))
                for (at, op), following in zip(instructions, instructions[1:] + [(None, None)]):
                    if op not in DEX_INVOKES or refs[struct.unpack_from("<H", dex, at + 2)[0]] != target:
                        continue
                    if following[1] in DEX_MOVE_RESULTS:
                        raise ValueError(f"{method} uses the result of {target}")
                    out[at:at + 2 * DEX_INSTRUCTION_UNITS[op]] = bytes(2 * DEX_INSTRUCTION_UNITS[op])
                    changed.append(method)
            elif kind == "replace code":
                (count,) = struct.unpack_from("<I", dex, code_off + 12)
                current = bytes(out[insns:insns + 2 * count])
                new, new_outs = assemble_dex(target["new"], dex)
                if len(new) > 2 * count:
                    raise ValueError(f"the code that replaces {method} is longer than the method")
                new += bytes(2 * count - len(new))
                if current == new and registers == target["registers"]:
                    continue
                old, _ = assemble_dex(target["old"], dex)
                # A try block names instructions by their address, which new code moves.
                if tries or current != old:
                    raise ValueError(f"{method} is not the code this edit replaces: the app was "
                                     "built from other sources, and the edit must be written for them")
                if target["registers"] < ins:
                    raise ValueError(f"{method} takes {ins} registers of arguments")
                struct.pack_into("<HHH", out, code_off, target["registers"], ins, max(outs, new_outs))
                out[insns:insns + 2 * count] = new
                changed.append(method)
            else:
                raise ValueError(f"unknown dex edit {kind}")
    if changed:
        out[12:32] = hashlib.sha1(bytes(out[32:])).digest()
        out[8:12] = struct.pack("<I", zlib.adler32(bytes(out[12:])))
    return bytes(out), changed


def edit_apk(data, name, code=None, controllers=None, dimensions=None):
    """An APK with `code` edited in its dex files (see edit_dex), the preference elements
    whose controller `controllers` names removed from its compiled XML ({xml: [controller]}),
    and the dimensions `dimensions` names set in its compiled XML ({xml: [(element,
    attribute, dp)]}, see set_xml_dimensions). Prints each change; returns the APK as it was
    when nothing changed."""
    replacements = {}
    with zipfile.ZipFile(io.BytesIO(data)) as apk:
        files = set(apk.namelist())
        for dex in (sorted(f for f in files if re.fullmatch(r"classes\d*\.dex", f)) if code else []):
            content = apk.read(dex)
            if content[:4] != b"dex\n":
                continue
            edited, changed = edit_dex(content, code)
            for method in changed:
                print(f"editing {method} in {name}:{dex}", flush=True)
            if changed:
                replacements[dex] = edited
        for xml, removed in (controllers or {}).items():
            if xml not in files:
                continue
            edited, removed_controllers = remove_xml_elements(apk.read(xml), "controller", set(removed))
            for controller in removed_controllers:
                print(f"removing {controller} from {name}:{xml}", flush=True)
            if removed_controllers:
                replacements[xml] = edited
        for xml, edits in (dimensions or {}).items():
            if xml not in files:
                continue
            edited, changed = set_xml_dimensions(replacements.get(xml) or apk.read(xml), edits)
            if changed:
                for element, attribute, dp in edits:
                    print(f"setting {element} attribute {attribute:#x} to {dp} dp in {name}:{xml}", flush=True)
                replacements[xml] = edited
        if not replacements:
            return data
        out = io.BytesIO()
        # Each entry keeps its compression; signing aligns the stored ones again and replaces
        # the signature this edit invalidates.
        with zipfile.ZipFile(out, "w") as edited_apk:
            for info in apk.infolist():
                edited_apk.writestr(info, replacements.get(info.filename) or apk.read(info))
    return out.getvalue()


def edit_settings_apk(data, name):
    """Settings.apk without the preferences, and the code, that need the Bluetooth service,
    and with room below the last row of the lists that set their own padding."""
    return edit_apk(data, name, code={**SETTINGS_CODE_WITHOUT_BLUETOOTH, **SETTINGS_CODE_LIST_MARGIN},
                    controllers=SETTINGS_CONTROLLERS_WITHOUT_BLUETOOTH, dimensions=SETTINGS_LIST_DIMENSIONS)


def edit_documentsui_apk(data, name):
    """DocumentsUI.apk (Files) with room below its list's last row in windows that have no bar
    below the list."""
    return edit_apk(data, name, code=DOCUMENTSUI_CODE)


def edit_permission_controller_apk(data, name):
    """PermissionController.apk with room below the last row of its preference lists."""
    return edit_apk(data, name, dimensions=PERMISSION_CONTROLLER_DIMENSIONS)


# Properties the vendor and odm build.prop files may not set. init reads those two files as
# vendor_init, and the platform's policy gives vendor_init no right to set these: each line
# was refused, and logged as a denial, on every boot. The system build.prop sets them.
# adbd's listening address and the USB configuration are the system's to set too.
VENDOR_FORBIDDEN_PROPS = {*USER_PROPS, *ADB_PROPS, "sys.use_memfd"}
VENDOR_PROP_FILES = {"VENDOR/build.prop", "ODM/etc/build.prop"}

# The build's own fingerprint is AOSP's, and it stays the same however often DSA's image
# changes. PackageManager keys its parsed package cache, its compiled code and its upgrade
# handling on the fingerprints of the partitions, so with the same fingerprint it went on
# using what it had parsed from the agent the image had before: the input method's metadata
# pointed at a resource the new agent had moved, the input method failed to load, and
# permissions the new agent asks for were never granted. Each image adds a tag of its own
# content to the incremental version, which every fingerprint carries, so a new image is an
# upgrade, as an over-the-air update is.
FINGERPRINT_KEY = re.compile(r"^ro\.(?:[a-z_]+\.)?build\.fingerprint$")
INCREMENTAL_KEY = re.compile(r"^ro\.(?:[a-z_]+\.)?build\.version\.incremental$")


def image_build_tag(payloads):
    """Returns the tag that names one image's content: a digest of the files DSA adds to the
    image and of the files in this folder that say how the image is made."""
    h = hashlib.sha256()
    for payload in payloads:
        h.update(hashlib.sha256(payload or b"").digest())
    for path in sorted(p for p in HERE.rglob("*") if p.is_file() and "__pycache__" not in p.parts):
        h.update(path.relative_to(HERE).as_posix().encode() + b"\0")
        h.update(hashlib.sha256(path.read_bytes()).digest())
    return "dsa" + h.hexdigest()[:12]


def tag_build_version(key, value, build_tag):
    """Returns a build property's value with `build_tag` added to its incremental version."""
    if INCREMENTAL_KEY.match(key):
        return f"{value}.{build_tag}"
    if FINGERPRINT_KEY.match(key) and ":" in value:
        # brand/product/device:release/id/incremental:type/tags
        head, tail = value.rsplit(":", 1)
        return f"{head}.{build_tag}:{tail}"
    return value


# The device identity the image reports instead of Cuttlefish's. At each boot the host hands
# the guest a property file in the boot's initrd with the computer's manufacturer, brand,
# model, model name and chip, and what the user changed in the settings; first stage init
# keeps it for the second stage at RAMDISK_PROP, and the product build.prop, which init reads
# last, ends with imports that take the identity properties from it. The device and product
# name follow the architecture (use_arch): dsa_x86_64 or dsa_arm64.
IDENTITY = {
    "brand": "DSA",
    "manufacturer": "thisisthepy",
    "model": "Desktop Subsystem for Android",
    "device": "dsa_x86_64",
    "name": "dsa_x86_64",
}
# Large-screen layouts without keeping phone apps out: apps pick their layout from the
# window's size, and AOSP's tablet configurations use this value.
CHARACTERISTICS = "tablet"
BUILD_TYPE = "user"
PRODUCT_KEY = re.compile(r"^ro\.product\.(?:([a-z_]+)\.)?(brand|manufacturer|model|device|name)$")
RAMDISK_PROP = "/second_stage_resources/system/etc/ramdisk/build.prop"
IDENTITY_PARTITIONS = ["system", "system_ext", "vendor", "product", "odm", "vendor_dlkm",
                       "odm_dlkm", "system_dlkm"]
IDENTITY_IMPORTS = [
    "ro.product.*",
    "ro.build.characteristics",
    "ro.build.product",
    "ro.build.flavor",
    "ro.build.description",
    "ro.build.fingerprint",
    *(f"ro.{p}.build.fingerprint" for p in IDENTITY_PARTITIONS),
    # Build.SOC_MANUFACTURER and Build.SOC_MODEL: the computer's CPU.
    "ro.soc.*",
]
# Read only for DSA's identity, not for the rest of what edit_props sets: the dlkm partitions'
# files and the generic ramdisk's, which first stage init hands the second stage.
IDENTITY_PROP_FILES = {
    "VENDOR_DLKM/etc/build.prop",
    "ODM_DLKM/etc/build.prop",
    "SYSTEM_DLKM/etc/build.prop",
    "INIT_BOOT/RAMDISK/system/etc/ramdisk/build.prop",
    "BOOT/RAMDISK/system/etc/ramdisk/build.prop",
}
PRODUCT_PROP_FILE = "PRODUCT/etc/build.prop"


def identity_value(key, value):
    """Returns a build property's value with DSA's device identity in place of the build's."""
    m = PRODUCT_KEY.match(key)
    if m:
        return IDENTITY[m.group(2)]
    if key == "ro.build.characteristics":
        return CHARACTERISTICS
    if key == "ro.build.product":
        return IDENTITY["device"]
    if key == "ro.build.flavor":
        return f"{IDENTITY['name']}-{BUILD_TYPE}"
    if key == "ro.build.description":
        # flavor release id incremental tags
        words = value.split(" ")
        return " ".join([f"{IDENTITY['name']}-{BUILD_TYPE}", *words[1:]])
    if key == "ro.build.display.id":
        # A user build shows only the build ID; Cuttlefish's says its flavor first.
        words = value.split(" ")
        return words[2] if len(words) >= 3 and "-" in words[0] else value
    if FINGERPRINT_KEY.match(key) and value.count(":") == 2 and value.count("/") == 5:
        # brand/name/device:release/id/incremental:type/tags
        _, build, kind = value.split(":")
        tags = kind.split("/", 1)[1]
        return f"{IDENTITY['brand']}/{IDENTITY['name']}/{IDENTITY['device']}:{build}:{BUILD_TYPE}/{tags}"
    return value


def edit_identity(text, build_tag=None):
    """Returns a property file with DSA's device identity, and with `build_tag` in its
    incremental version and fingerprints."""
    out = []
    for line in text.splitlines():
        if "=" in line and not line.startswith("#"):
            key, value = line.split("=", 1)
            value = identity_value(key, value)
            if build_tag:
                value = tag_build_version(key, value, build_tag)
            line = f"{key}={value}"
        out.append(line)
    return "\n".join(out) + "\n"


def identity_imports(text):
    """Returns the product build.prop ending with the imports that let the host's property
    file, when the boot carries one, replace the identity of every partition."""
    lines = text.splitlines()
    if not any(line.startswith("ro.build.characteristics=") for line in lines):
        lines.append(f"ro.build.characteristics={CHARACTERISTICS}")
    lines.append("# The device identity chosen in DSA's settings, when the boot carries one.")
    lines += [f"import {RAMDISK_PROP} {key}" for key in IDENTITY_IMPORTS]
    return "\n".join(lines) + "\n"


def image_identity(system_props):
    """The parts of the image's fingerprint a user does not choose, for image.json."""
    props = dict(line.split("=", 1) for line in system_props.splitlines()
                 if "=" in line and not line.startswith("#"))
    return {
        "release": props.get("ro.build.version.release_or_codename") or props.get("ro.build.version.release", ""),
        "id": props.get("ro.build.id", ""),
        "incremental": props.get("ro.build.version.incremental", ""),
        "tags": props.get("ro.build.tags", "release-keys"),
    }


def edit_props(text, codec2=False, vendor=False, build_tag=None, product=False, network=False):
    """Returns a build.prop with DSA's properties and device identity. With `vendor`, it is one
    init reads as vendor_init, which gets only the properties vendor_init may set. With
    `build_tag`, the incremental version and the fingerprints carry it. With `product`, it ends
    with the imports of the identity the user chose. With `network`, it is the vendor build.prop
    that chooses how the VM's network adapter reaches Android (NETWORK_PROPS)."""
    props_to_apply = {**USER_PROPS, **ADB_PROPS, **GRAPHICS_PROPS, **MEMORY_PROPS, **LOADER_PROPS, **FRP_PROPS,
                      **LOG_PROPS, **(CODEC2_PROPS if codec2 else {}), **(NETWORK_PROPS if network else {})}
    if vendor:
        props_to_apply = {k: v for k, v in props_to_apply.items() if k not in VENDOR_FORBIDDEN_PROPS}
    out = []
    for line in text.splitlines():
        key = line.split("=", 1)[0]
        if key in props_to_apply or key in DROPPED_PROPS or (vendor and key in VENDOR_FORBIDDEN_PROPS):
            continue
        if key.endswith("build.type"):
            line = f"{key}=user"
        if "=" in line and not line.startswith("#"):
            value = identity_value(key, line.split("=", 1)[1])
            if build_tag:
                value = tag_build_version(key, value, build_tag)
            line = f"{key}={value}"
        out.append(line)
    out += [f"{k}={v}" for k, v in props_to_apply.items()]
    text = "\n".join(out) + "\n"
    return identity_imports(text) if product else text


# The network. The host attaches one Hyper-V synthetic adapter (hv_netvsc), eth0, to a NAT
# network it makes for the VM, or to the LAN in advanced networking. Android shows it as
# Wi-Fi, the way Cuttlefish shows its virtio adapter: Cuttlefish's setup_wifi renames eth0
# to buried_eth0 and puts a virt_wifi link, wlan0, on top, which the Wi-Fi HAL and
# wpa_supplicant see as a radio joined to the open network "VirtWifi". Apps then get an
# unmetered Wi-Fi network that NetworkMonitor validates, and ConnectivityManager reports it
# and each change of it. The address, the route and the DNS servers come by DHCP from the
# host (Android's IpClient asks), so nothing here is static: a host network change reaches
# the guest through its next lease, and the NAT gateway's DNS proxy follows the host's own
# DNS and VPN. Cuttlefish's rename_eth0 would hand eth0 to the radio as rmnet0, and DSA has no
# modem, so it is turned off. setup_wifi derives wlan0's MAC address from the boot property
# wifi_mac_prefix, a number it fails without (see kernel_cmdline).
NETWORK_PROPS_FILE = "VENDOR/build.prop"
NETWORK_INTERFACE = "eth0"
NETWORK_PROPS = {
    "ro.vendor.wifi_impl": "virt_wifi",
    "ro.vendor.virtwifi.port": NETWORK_INTERFACE,
    "ro.vendor.disable_rename_eth0": "1",
}
WIFI_MAC_PREFIX = "5554"
# setup_wifi and its init service ship in Cuttlefish's Wi-Fi APEX, not in /vendor/bin, with
# their SELinux label (setup_wifi_exec) in the vendor policy.
SETUP_WIFI_APEX = "VENDOR/apex/com.google.cf.wifi.apex"
SETUP_WIFI_SERVICE = b"/bin/setup_wifi --interface=${ro.vendor.virtwifi.port}"
# Kernel command line arguments that would give the guest a static address in place of DHCP.
STATIC_NETWORK_ARGS = ["androidboot.auto_eth_guest_addr=", "ip="]


def network_problems(names, read, cmdline):
    """What keeps the guest from reaching the network through DHCP and DNS from the host, as
    lines for check-image.py. `names` lists the target files, `read` returns one's bytes and
    `cmdline` is the image's kernel command line."""
    problems = []
    if NETWORK_PROPS_FILE not in names:
        problems.append(f"{NETWORK_PROPS_FILE} is missing, so nothing turns the network adapter into Wi-Fi")
    else:
        props = {}
        for line in read(NETWORK_PROPS_FILE).decode(errors="replace").splitlines():
            if "=" in line and not line.startswith("#"):
                key, value = line.split("=", 1)
                props[key.strip()] = value.strip()
        for key, value in NETWORK_PROPS.items():
            if props.get(key) != value:
                problems.append(f"{NETWORK_PROPS_FILE} sets {key} to {props.get(key)!r}, not {value!r}, so "
                                "Android does not show the VM's network adapter as Wi-Fi")
        for key in props:
            if key.startswith("net.dns") or key.startswith("ro.vendor.net.dns"):
                problems.append(f"{NETWORK_PROPS_FILE} pins the DNS server ({key}); the guest must use the "
                                "servers DHCP gives it")
    if SETUP_WIFI_APEX not in names:
        problems.append(f"{SETUP_WIFI_APEX} is missing, so nothing makes eth0 into wlan0 and apps have no network")
    else:
        payload = zipfile.ZipFile(io.BytesIO(read(SETUP_WIFI_APEX))).read("apex_payload.img")
        if SETUP_WIFI_SERVICE not in payload:
            problems.append(f"{SETUP_WIFI_APEX} has no setup_wifi service on ro.vendor.virtwifi.port, so "
                            "eth0 never becomes wlan0 and apps have no network")
    if not any(name.startswith("VENDOR/etc/init/") and b"setup_wifi" in read(name)
               for name in names if name.endswith(".rc") and name.startswith("VENDOR/etc/init/")):
        problems.append("no vendor init script starts setup_wifi")
    if f"androidboot.wifi_mac_prefix={WIFI_MAC_PREFIX}" not in cmdline.split():
        problems.append("the kernel command line has no androidboot.wifi_mac_prefix, and setup_wifi "
                        "exits without it")
    for arg in STATIC_NETWORK_ARGS:
        if any(part.startswith(arg) for part in cmdline.split()):
            problems.append(f"the kernel command line sets {arg}, a static address in place of DHCP")
    return problems


# Wi-Fi's init script makes a kernel trace instance when boot completes, for WifiService to
# record connection attempts into. Its wlan0 is a virt_wifi link with nothing to trace for a
# desktop user, and on Hyper-V each of its tracefs writes takes up to two seconds of kernel time. At
# the same moment lmkd attaches its BPF memory event programs to tracepoints, which waits
# behind those writes, inside the two seconds its watchdog gives its main loop. When the
# watchdog fires it kills the process lmkd last saw with the highest oom_score_adj, and lmkd
# has not yet read the update that made the app the user just opened a foreground one.
WIFI_RC = "SYSTEM/etc/init/wifi.rc"
WIFI_TRACING_TRIGGER = "on property:sys.boot_completed=1 && property:sys.wifitracing.started=0"


def edit_wifi_rc(text):
    """Returns Wi-Fi's init script without the section that sets up its trace instance."""
    out = []
    skipping = False
    found = False
    for line in text.splitlines(keepends=True):
        if line.strip() == WIFI_TRACING_TRIGGER:
            skipping = found = True
            continue
        # An rc section runs until the next line that starts another one at column 0.
        if skipping and line[:1] not in ("", " ", "\t", "\n", "\r", "#"):
            skipping = False
        if not skipping:
            out.append(line)
    if not found:
        raise ValueError(f"{WIFI_RC} has no '{WIFI_TRACING_TRIGGER}' section; check how this build "
                         "sets up the Wi-Fi trace instance before making the image")
    return "".join(out)


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
VULKAN_RANCHU_LIBRARY = "VENDOR/lib64/hw/vulkan.ranchu.so"

# DSA's composer HAL (dsa/guest/hwcomposer), which sends the host each task's frames, and
# Cuttlefish's composers, which compose the whole display and go when DSA's is installed.
HWCOMPOSER_DIR = REPO / "dsa/guest/hwcomposer"
HWCOMPOSER_BINARY = "VENDOR/bin/hw/dsa-hwcomposer"
HWCOMPOSER_RC = "VENDOR/etc/init/dsa-hwcomposer.rc"
HWCOMPOSER_MANIFEST = "VENDOR/etc/vintf/manifest/dsa-hwcomposer.xml"
# The vendor policy's composer domain: init starts the binary in it, and it may serve the
# composer HAL, reach the host renderer over vsock and call dsa-channeld.
HWCOMPOSER_CONTEXT = "/vendor/bin/hw/dsa-hwcomposer u:object_r:hal_graphics_composer_default_exec:s0"
CUTTLEFISH_COMPOSER_APEXES = [
    "com.android.hardware.graphics.composer.ranchu",
    "com.android.hardware.graphics.composer.drm_hwcomposer",
]
# DSA's power and health HALs (dsa/guest/power), which take the guest's power hints to the host
# and give the guest the host's battery, and the Cuttlefish APEXes whose HALs they replace.
# Cuttlefish's power APEX also holds its power statistics HAL, a mock the framework does not
# need. Each runs in the vendor policy's own domain for its HAL; power.cil lets both reach
# dsa-channeld.
POWER_DIR = REPO / "dsa/guest/power"
POWER_FILES = {
    "VENDOR/bin/hw/dsa-power": None,
    "VENDOR/bin/hw/dsa-health": None,
    "VENDOR/etc/init/dsa-power.rc": POWER_DIR / "dsa-power.rc",
    "VENDOR/etc/init/dsa-health.rc": POWER_DIR / "dsa-health.rc",
    "VENDOR/etc/vintf/manifest/dsa-power.xml": POWER_DIR / "dsa-power.xml",
    "VENDOR/etc/vintf/manifest/dsa-health.xml": POWER_DIR / "dsa-health.xml",
}
POWER_CONTEXTS = [
    (r"/vendor/bin/hw/dsa-power", "/vendor/bin/hw/dsa-power u:object_r:hal_power_default_exec:s0"),
    (r"/vendor/bin/hw/dsa-health", "/vendor/bin/hw/dsa-health u:object_r:hal_health_default_exec:s0"),
]
CUTTLEFISH_POWER_APEXES = [
    "com.android.hardware.power",
    "com.google.cf.health",
]
# DSA's primary audio module (dsa/guest/audio), which plays the guest's sound on the host's
# default output device. It runs in the audio HAL's domain and serves IModule/default, which
# the image's audio APEX already declares. That APEX's service makes a module for each one
# its policy file lists, so the primary one leaves the file and the service keeps the rest
# (remote submix, Bluetooth), the configuration and the effects.
AUDIO_DIR = REPO / "dsa/guest/audio"
AUDIO_BINARY = "VENDOR/bin/hw/dsa-audio"
AUDIO_RC = "VENDOR/etc/init/dsa-audio.rc"
AUDIO_CONTEXT = "/vendor/bin/hw/dsa-audio u:object_r:hal_audio_default_exec:s0"
AUDIO_POLICY_CONFIG = re.compile(r"^VENDOR/etc/audio_policy_configuration[^/]*\.xml$")
PRIMARY_AUDIO_INCLUDE = re.compile(
    r'[ \t]*(<!--[^>]*Primary Audio HAL[^>]*-->[ \t]*\n[ \t]*)?'
    r'<xi:include href="primary_audio_policy_configuration\.xml"\s*/>[ \t]*\n?'
)


def without_primary_audio_module(text):
    """The audio policy file without its primary module, which dsa-audio serves instead."""
    return PRIMARY_AUDIO_INCLUDE.sub("", text)


# DSA's GNSS HAL (dsa/guest/gnss), which gives the guest the host computer's location, and the
# Cuttlefish GNSS HAL it replaces, whichever way the image carries it: an APEX, or a binary
# with its init script and VINTF fragment. Two HALs would both claim IGnss/default. It runs in
# the vendor policy's GNSS domain; gnss.cil lets it reach dsa-channeld.
GNSS_DIR = REPO / "dsa/guest/gnss"
GNSS_FILES = {
    "VENDOR/bin/hw/dsa-gnss": None,
    "VENDOR/etc/init/dsa-gnss.rc": GNSS_DIR / "dsa-gnss.rc",
    "VENDOR/etc/vintf/manifest/dsa-gnss.xml": GNSS_DIR / "dsa-gnss.xml",
}
GNSS_CONTEXT = "/vendor/bin/hw/dsa-gnss u:object_r:hal_gnss_default_exec:s0"
CUTTLEFISH_GNSS_APEXES = [
    "com.google.cf.gnss",
    "com.android.hardware.gnss",
]
CUTTLEFISH_GNSS_FOLDERS = [
    "VENDOR/bin/hw/",
    "VENDOR/etc/init/",
    "VENDOR/etc/vintf/manifest/",
]
# DSA's Codec 2.0 HAL (dsa/guest/codec2), whose video decoders decode on the host's GPU. It
# runs in the vendor codec domain (mediacodec); codec2.cil lets it reach the host's video port
# and map the buffers it writes pictures into. Its decoders are listed in
# media_codecs_c2.xml, which the framework reads after the vendor's media_codecs.xml.
CODEC2_DIR = REPO / "dsa/guest/codec2"
CODEC2_FILES = {
    "VENDOR/bin/hw/dsa-codec2": None,
    "VENDOR/etc/init/dsa-codec2.rc": CODEC2_DIR / "dsa-codec2.rc",
    "VENDOR/etc/vintf/manifest/dsa-codec2.xml": CODEC2_DIR / "dsa-codec2.xml",
    "VENDOR/etc/media_codecs_c2.xml": CODEC2_DIR / "media_codecs_c2.xml",
}
CODEC2_CONTEXT = "/vendor/bin/hw/dsa-codec2 u:object_r:mediacodec_exec:s0"
CODEC2_INSTANCE = "android.hardware.media.c2.IComponentStore/default"
CODEC2_SERVICE_CONTEXT = f"{CODEC2_INSTANCE} u:object_r:hal_codec2_service:s0"
# The framework's codec buffers come from plain allocations, passed as handles, instead of
# buffer pools: the guest's codec HAL takes its input as handles. Only the output to a
# surface keeps its pool (BUFFERQUEUE, 1 << 18), which reaches the HAL as the surface's
# allocator. The guest's software decoders take handles as well.
CODEC2_PROPS = {"debug.stagefright.c2-poolmask": str(1 << 18)}
# DSA's sensors HAL (dsa/guest/sensors), which shows the host's accelerometer, gyroscope,
# orientation and light sensors to Android, and the Cuttlefish APEXes whose sensors HAL it
# replaces. It runs in the vendor sensors domain; sensors.cil lets it reach the host's sensor
# port over vsock.
SENSORS_DIR = REPO / "dsa/guest/sensors"
SENSORS_FILES = {
    "VENDOR/bin/hw/dsa-sensors": None,
    "VENDOR/etc/init/dsa-sensors.rc": SENSORS_DIR / "dsa-sensors.rc",
    "VENDOR/etc/vintf/manifest/dsa-sensors.xml": SENSORS_DIR / "dsa-sensors.xml",
}
SENSORS_CONTEXT = "/vendor/bin/hw/dsa-sensors u:object_r:hal_sensors_default_exec:s0"
CUTTLEFISH_SENSORS_APEXES = [
    "com.android.hardware.sensors",
    "com.google.cf.sensors",
]
# DSA's camera provider HAL (dsa/guest/camera), which lends the host computer's cameras to
# Android, and the Cuttlefish APEX whose emulated cameras it replaces. It runs in the vendor
# camera HAL domain (hal_camera_default); camera.cil lets it reach the host's camera port and
# map the apps' buffers it writes pictures into. Apps reach it through cameraserver, so
# Android's camera permission applies. The features file declares the cameras a computer
# has, so apps that need a camera install and offer it.
CAMERA_DIR = REPO / "dsa/guest/camera"
CAMERA_FILES = {
    "VENDOR/bin/hw/dsa-camera": None,
    "VENDOR/etc/init/dsa-camera.rc": CAMERA_DIR / "dsa-camera.rc",
    "VENDOR/etc/vintf/manifest/dsa-camera.xml": CAMERA_DIR / "dsa-camera.xml",
    "VENDOR/etc/permissions/dsa-camera-features.xml": CAMERA_DIR / "dsa-camera-features.xml",
}
CAMERA_CONTEXT = "/vendor/bin/hw/dsa-camera u:object_r:hal_camera_default_exec:s0"
CAMERA_INSTANCE = "android.hardware.camera.provider.ICameraProvider/internal/0"
CAMERA_SERVICE_CONTEXT = f"{CAMERA_INSTANCE} u:object_r:hal_camera_service:s0"
CAMERA_APEX_MODULE = "com.google.emulated.camera.provider.hal"
# Both variants: apexd activates whichever is left, and either declares the same provider
# instance as DSA's camera HAL, which fails the whole vendor VINTF manifest.
CUTTLEFISH_CAMERA_APEXES = [
    "com.google.emulated.camera.provider.hal",
    "com.google.emulated.camera.provider.hal.fastscenecycle",
]
# The freeform display every task window lives on, as the hwcomposer reads it from
# ro.boot.dsa.*. The host shows windows, not this display, so its size bounds a window's.
# These are defaults: the host replaces the width and height from the user's monitors.
DISPLAY_BOOTCONFIG = {
    "display_width": 1920,
    "display_height": 1080,
    "display_dpi": 160,
}

# Turns on the zram swap fstab.dsa declares, and tunes the kernel for it.
MEMORY_RC = "VENDOR/etc/init/dsa-memory.rc"
# Main thread timing for the input trace (dsa-trace.rc). A system script, so init itself runs it.
TRACE_RC = "SYSTEM/etc/init/dsa-trace.rc"
# Quiets the serial console once boot completes, so it carries only the kernel's early boot and
# a panic. A system script, because only init itself may write the kernel's printk levels.
CONSOLE_RC = "SYSTEM/etc/init/dsa-console.rc"
SYSTEM_FS_CONFIG = "META/filesystem_config.txt"
TRACE_FS_CONFIG = [
    "system/etc/init/dsa-trace.rc 0 0 644 capabilities=0x0",
    "system/etc/init/dsa-console.rc 0 0 644 capabilities=0x0",
]
# ARM app translation. The host puts androidboot.dsa.arm_translation=on in the boot's
# bootconfig while the user has a translation package installed (`dsa arm-translation
# install`) and on, and androidboot.dsa.arm_translation=default while they have none and the
# image carries its own default package (ARM_TRANSLATION_DEFAULT_IMAGE). The product
# build.prop, read last, then imports the native bridge and ABI properties from the "on" or
# "default" file; otherwise from the "off" file, which sets nothing. An image without a default
# package has no "default" file, so a boot told to use one still installs x86_64 apps only.
# ro.* properties are fixed once read, so this is decided at boot. The init script mounts the
# package over /system before the zygote starts; a system script, because only init itself may
# mount there.
ARM_TRANSLATION_RC = "SYSTEM/etc/init/dsa-arm-translation.rc"
ARM_TRANSLATION_PROPS = {
    "SYSTEM/etc/dsa/arm-translation-on.prop": HERE / "arm-translation-on.prop",
    "SYSTEM/etc/dsa/arm-translation-off.prop": HERE / "arm-translation-off.prop",
}
# The default package's system image and properties, in an x86_64 image made with
# --arm-translation. The properties are the "on" file's: the default package is laid over the
# system exactly as a user's is.
ARM_TRANSLATION_DEFAULT_IMAGE = "SYSTEM/etc/dsa/arm-translation/default.img"
ARM_TRANSLATION_DEFAULT_PROPS = "SYSTEM/etc/dsa/arm-translation-default.prop"
ARM_TRANSLATION_DEFAULT_GUEST_PATH = "/system/etc/dsa/arm-translation/default.img"
ARM_TRANSLATION_DEFAULT_FS_CONFIG = [
    "system/etc/dsa/arm-translation 0 0 755 capabilities=0x0",
    "system/etc/dsa/arm-translation/default.img 0 0 644 capabilities=0x0",
    "system/etc/dsa/arm-translation-default.prop 0 0 644 capabilities=0x0",
]
ARM_TRANSLATION_PRODUCT_PROPS = "PRODUCT/etc/build.prop"
ARM_TRANSLATION_IMPORT = "import /system/etc/dsa/arm-translation-${ro.boot.dsa.arm_translation:-off}.prop"
ARM_TRANSLATION_FS_CONFIG = [
    "system/etc/init/dsa-arm-translation.rc 0 0 644 capabilities=0x0",
    "system/etc/dsa 0 0 755 capabilities=0x0",
    "system/etc/dsa/arm-translation-on.prop 0 0 644 capabilities=0x0",
    "system/etc/dsa/arm-translation-off.prop 0 0 644 capabilities=0x0",
]


# The default ARM translation package of an x86_64 image (Digitalis, built by the "Translator
# build" workflow). Its system image goes into the system partition
# (ARM_TRANSLATION_DEFAULT_IMAGE), so it is signed and verified with the image, updated with
# it, and shared by every user of the computer. image.json describes it under
# "arm_translation", which is how the host knows the image has one ("system_image" says where).
# The package folder `dsa arm-translation install` takes is also zipped beside the image, so a
# user can install it as their own; the host never downloads the zip. An arm64 image runs ARM
# apps itself, so it never has one.
ARM_TRANSLATION_ZIP = "arm-translation.zip"
ARM_TRANSLATION_MANIFEST = "arm-translation.json"
ARM_TRANSLATION_IMAGE = "system.img"
ARM_TRANSLATION_ABIS = ["arm64-v8a"]
ARM_TRANSLATION_LIBRARY = re.compile(r"^[A-Za-z0-9._-]+\.so$")
EROFS_MAGIC = 0xE0F5E1E2


def arm_translation_problems(description, image_head):
    """What the host would refuse in a translation package: its description (the parsed
    arm-translation.json) and the first bytes of its system.img."""
    problems = []
    for key, longest in (("name", 64), ("version", 64)):
        value = description.get(key)
        if not isinstance(value, str) or not value or len(value) > longest or value.strip() != value:
            problems.append(f"the translation package's {key} must have from 1 to {longest} characters")
    library = description.get("library")
    if not isinstance(library, str) or len(library) > 128 or not ARM_TRANSLATION_LIBRARY.match(library):
        problems.append(f"the translation package's bridge library {library!r} is not a library file name")
    if description.get("abis") != ARM_TRANSLATION_ABIS:
        problems.append(f"the translation package translates {description.get('abis')}, not only arm64-v8a")
    license_id = description.get("license", "")
    if not isinstance(license_id, str) or len(license_id) > 128:
        problems.append("the translation package's licence is not a short SPDX expression")
    if len(image_head) < 1028 or int.from_bytes(image_head[1024:1028], "little") != EROFS_MAGIC:
        problems.append(f"the translation package's {ARM_TRANSLATION_IMAGE} is not an erofs image")
    return problems


def checked_arm_translation(package, arch):
    """The description of the translation package folder `package`, once it is one the host
    takes and `arch` is an architecture that needs a translator."""
    if arch != "x86_64":
        raise SystemExit(f"an {arch} image runs ARM apps without a translator, so it takes no "
                         "ARM translation package")
    package = Path(package)
    description = json.loads((package / ARM_TRANSLATION_MANIFEST).read_text())
    with open(package / ARM_TRANSLATION_IMAGE, "rb") as f:
        head = f.read(1028)
    problems = arm_translation_problems(description, head)
    if problems:
        raise SystemExit("; ".join(problems))
    return description


def default_arm_translation(package, arch, out):
    """Zips the translation package folder `package` into `out` as the image's default
    translator, and returns what image.json says of it."""
    description = checked_arm_translation(package, arch)
    package = Path(package)
    target = Path(out) / ARM_TRANSLATION_ZIP
    with zipfile.ZipFile(target, "w", zipfile.ZIP_STORED) as z:
        z.write(package / ARM_TRANSLATION_MANIFEST, ARM_TRANSLATION_MANIFEST)
        z.write(package / ARM_TRANSLATION_IMAGE, ARM_TRANSLATION_IMAGE)
    return {
        "file": ARM_TRANSLATION_ZIP,
        "size": target.stat().st_size,
        "sha256": sha256(target),
        **{key: description[key] for key in ("name", "version", "library", "abis")},
        "license": description.get("license", ""),
        "system_image": ARM_TRANSLATION_DEFAULT_GUEST_PATH,
        "system_image_sha256": sha256(package / ARM_TRANSLATION_IMAGE),
    }


def check_default_arm_translation(manifest, image_dir):
    """The problems with an image's default translator: an arm64 image has none, and an
    x86_64 one's zip matches image.json and holds a package the host takes."""
    image_dir = Path(image_dir)
    entry = manifest.get("arm_translation")
    arch = manifest.get("arch") or DEFAULT_ARCH
    if arch != "x86_64":
        if entry is not None or (image_dir / ARM_TRANSLATION_ZIP).exists():
            return [f"the {arch} image carries an ARM translation package, which it never uses"]
        return []
    if entry is None:
        return []
    path = image_dir / entry.get("file", "")
    if entry.get("file") != ARM_TRANSLATION_ZIP or not path.is_file():
        return [f"image.json names the ARM translation package {entry.get('file')!r}, which is not beside it"]
    problems = []
    if path.stat().st_size != entry.get("size") or sha256(path) != entry.get("sha256"):
        problems.append(f"{ARM_TRANSLATION_ZIP} does not match image.json")
    with zipfile.ZipFile(path) as z:
        if sorted(z.namelist()) != sorted([ARM_TRANSLATION_MANIFEST, ARM_TRANSLATION_IMAGE]):
            return problems + [f"{ARM_TRANSLATION_ZIP} holds {z.namelist()}, not a package's two files"]
        description = json.loads(z.read(ARM_TRANSLATION_MANIFEST))
        with z.open(ARM_TRANSLATION_IMAGE) as f:
            head = f.read(1028)
    problems += arm_translation_problems(description, head)
    for key in ("name", "version", "library", "abis", "license"):
        if description.get(key, "") != entry.get(key):
            problems.append(f"image.json's ARM translation {key} is not the package's")
    if entry.get("system_image") != ARM_TRANSLATION_DEFAULT_GUEST_PATH:
        problems.append("image.json does not say the default ARM translation package is at "
                        f"{ARM_TRANSLATION_DEFAULT_GUEST_PATH}, so the host never uses it")
    return problems


def check_builtin_arm_translation(manifest, names, read):
    """The problems with the default translator in an image's system partition, given the
    names in its target files and a function that reads one. An x86_64 image that image.json
    says has one carries its system image and properties; any other image carries neither, so
    Android never offers ARM64 without a translator to run it."""
    entry = manifest.get("arm_translation")
    arch = manifest.get("arch") or DEFAULT_ARCH
    inside = [name for name in (ARM_TRANSLATION_DEFAULT_IMAGE, ARM_TRANSLATION_DEFAULT_PROPS) if name in names]
    if arch != "x86_64":
        return [f"the {arch} image's system partition has {name}, but it runs ARM apps without a translator"
                for name in inside]
    if entry is None:
        return [f"the system partition has {name}, which image.json does not describe, so the host never "
                "uses it" for name in inside]
    problems = []
    for name in (ARM_TRANSLATION_DEFAULT_IMAGE, ARM_TRANSLATION_DEFAULT_PROPS):
        if name not in names:
            problems.append(f"{name} is missing, so Android does not translate ARM apps from its first start")
    if ARM_TRANSLATION_DEFAULT_IMAGE in names:
        found = hashlib.sha256(read(ARM_TRANSLATION_DEFAULT_IMAGE)).hexdigest()
        if found != entry.get("system_image_sha256"):
            problems.append(f"{ARM_TRANSLATION_DEFAULT_IMAGE} is not the system image of the package "
                            "image.json describes")
    if ARM_TRANSLATION_DEFAULT_PROPS in names:
        on = ARM_TRANSLATION_PROPS["SYSTEM/etc/dsa/arm-translation-on.prop"].read_bytes()
        if read(ARM_TRANSLATION_DEFAULT_PROPS) != on:
            problems.append("arm-translation-default.prop is not arm-translation-on.prop, so the default "
                            "package is not laid over the system as a user's is")
    return problems


def add_arm_translation_import(text):
    """Returns the product build.prop with the ARM translation import as its last line, so the
    imported properties win over every partition's own."""
    lines = [line for line in text.splitlines() if line.strip() != ARM_TRANSLATION_IMPORT]
    return "\n".join(lines + [ARM_TRANSLATION_IMPORT]) + "\n"


# The patched framework: SurfaceFlinger, libhwui and libEGL with DSA's recovery from a GPU
# device loss, which the "Framework build" workflow builds from the exact source of the
# published build (dsa/guest/framework). They take the place of the build's own files, beside
# that build's libraries, so an image takes them only from a build of the same build number and
# for the same architecture. image.json describes them under "framework", and check-image.py
# finds the same files in the image. SurfaceFlinger then tells the apps about a reset through a
# debug property, which GPU_RESET_POLICY lets it set.
sys.path.insert(0, str(REPO / "dsa/guest/framework"))
import framework_lock  # noqa: E402

FRAMEWORK_MANIFEST = "framework.json"
FRAMEWORK_FILES = list(framework_lock.OUTPUTS)
GPU_RESET_PROPERTY = "debug.dsa.gpu.resets"
GPU_RESET_RULE = "(allow surfaceflinger debug_prop (property_service (set)))"


def framework_problems(description, build_id, android_major, arch, read):
    """What makes a framework build's files wrong for an image: its framework.json (parsed),
    the image's build number, Android version and architecture, and a function that returns a
    file's bytes by its target_files name, or None."""
    problems = []
    if description.get("android_major") != android_major:
        problems.append(f"the framework is built for Android {description.get('android_major')}, "
                        f"not {android_major}")
    if description.get("build_id") != build_id:
        problems.append(f"the framework is built from build {description.get('build_id')}, but the "
                        f"image is made from build {build_id}, whose libraries it would run beside")
    if description.get("arch") != arch:
        problems.append(f"the framework is built for {description.get('arch')}, not {arch}")
    files = description.get("files") or {}
    if sorted(files) != sorted(FRAMEWORK_FILES):
        problems.append(f"the framework has {sorted(files)}, not {sorted(FRAMEWORK_FILES)}")
    machine = ARCHES.get(arch, {}).get("elf_machine")
    for name in FRAMEWORK_FILES:
        entry = files.get(name) or {}
        data = read(name)
        if data is None:
            problems.append(f"the framework's {name} is missing")
            continue
        if len(data) != entry.get("size") or hashlib.sha256(data).hexdigest() != entry.get("sha256"):
            problems.append(f"the framework's {name} is not the file framework.json describes")
        if elf_machine(data[:64]) != machine:
            problems.append(f"the framework's {name} is built for ELF machine {elf_machine(data[:64])}, "
                            f"not {arch} ({machine})")
    return problems


def checked_framework(folder, build_id, android_major, arch):
    """The framework build's files in `folder`, by their target_files names, and its
    description, once they are the files of the image's own build and architecture."""
    folder = Path(folder)
    description = json.loads((folder / FRAMEWORK_MANIFEST).read_text())

    def read(name):
        path = folder / name
        return path.read_bytes() if path.is_file() else None

    problems = framework_problems(description, build_id, android_major, arch, read)
    if problems:
        raise SystemExit("; ".join(problems))
    return {name: read(name) for name in FRAMEWORK_FILES}, description


def framework_entry(description):
    """What image.json says of the framework an image carries."""
    return {
        "version": description.get("version", ""),
        "build_id": description["build_id"],
        "release": description.get("release", ""),
        "arch": description["arch"],
        "files": {name: description["files"][name]["sha256"] for name in FRAMEWORK_FILES},
    }


def check_framework(manifest, names, read):
    """The problems with the patched framework in an image, given image.json, the names in its
    target files and a function that reads one: each file image.json names is in the image with
    that SHA-256, built from the image's own build for its architecture, and SurfaceFlinger may
    tell the apps about a reset."""
    entry = manifest.get("framework")
    if entry is None:
        return []
    problems = []
    source = manifest.get("source", {})
    arch = manifest.get("arch") or DEFAULT_ARCH
    if entry.get("build_id") != source.get("build_id"):
        problems.append(f"the framework is built from build {entry.get('build_id')}, but the image is "
                        f"made from build {source.get('build_id')}")
    if entry.get("arch") != arch:
        problems.append(f"the framework is built for {entry.get('arch')}, not {arch}")
    files = entry.get("files") or {}
    if sorted(files) != sorted(FRAMEWORK_FILES):
        problems.append(f"image.json's framework names {sorted(files)}, not {sorted(FRAMEWORK_FILES)}")
    machine = ARCHES.get(arch, {}).get("elf_machine")
    for name in FRAMEWORK_FILES:
        if name not in names:
            problems.append(f"{name} is missing from the image")
            continue
        data = read(name)
        if hashlib.sha256(data).hexdigest() != files.get(name):
            problems.append(f"{name} is not the framework build's file that image.json names, so the "
                            "image does not recover from a GPU device loss")
        if elf_machine(data[:64]) != machine:
            problems.append(f"{name} is built for ELF machine {elf_machine(data[:64])}, not {arch}")
    policy = "SYSTEM/etc/selinux/plat_sepolicy.cil"
    if policy in names and GPU_RESET_RULE not in read(policy).decode(errors="replace"):
        problems.append(f"SurfaceFlinger may not set {GPU_RESET_PROPERTY}, so the apps never hear of a "
                        "GPU reset")
    return problems


# Google Play, for a user who installs it (`dsa gapps install`). The host puts
# androidboot.dsa.gapps=on in the boot's bootconfig only while the user has a Google apps image
# installed. The init script then lays the image over /product and /system_ext before the
# zygote starts, and the product build.prop imports the "on" file, whose fingerprint for the
# oem partition (which the image does not have) makes the boot an upgrade, so the package
# manager installs the new system apps for every user and grants their default permissions.
# Without it the "off" file sets nothing. ro.* properties are fixed once read, so this is decided
# at boot.
GAPPS_RC = "SYSTEM/etc/init/dsa-gapps.rc"
GAPPS_PROPS = {
    "SYSTEM/etc/dsa/gapps-on.prop": HERE / "gapps-on.prop",
    "SYSTEM/etc/dsa/gapps-off.prop": HERE / "gapps-off.prop",
}
GAPPS_IMPORT = "import /system/etc/dsa/gapps-${ro.boot.dsa.gapps:-off}.prop"
GAPPS_FS_CONFIG = [
    "system/etc/init/dsa-gapps.rc 0 0 644 capabilities=0x0",
    "system/etc/dsa/gapps-on.prop 0 0 644 capabilities=0x0",
    "system/etc/dsa/gapps-off.prop 0 0 644 capabilities=0x0",
]


def add_gapps_import(text):
    """Returns the product build.prop ending with the Google Play import. The ARM translation
    import goes after it (add_arm_translation_import), as the last line."""
    lines = [line for line in text.splitlines() if line.strip() != GAPPS_IMPORT]
    return "\n".join(lines + [GAPPS_IMPORT]) + "\n"
# The property dsa-channeld sets to have init drop the caches and compact memory, so free
# page reporting hands idle memory back to the host. vendor_property_contexts gives it the
# type the daemon's policy lets it set.
VENDOR_PROPERTY_CONTEXTS = "VENDOR/etc/selinux/vendor_property_contexts"
RECLAIM_PROPERTY = "vendor.dsa.memory.reclaim"
RECLAIM_PROPERTY_CONTEXT = f"{RECLAIM_PROPERTY} u:object_r:dsa_memory_prop:s0 exact bool"
# How many MiB each of the idle reclaim's four zram steps takes; the daemon sets it before the
# reclaim property.
RECLAIM_STEP_PROPERTY = "vendor.dsa.memory.reclaim_step"
RECLAIM_STEP_PROPERTY_CONTEXT = f"{RECLAIM_STEP_PROPERTY} u:object_r:dsa_memory_prop:s0 exact int"
# The gentler property for the gradual trim while apps are open: init drops only the page cache.
TRIM_PROPERTY = "vendor.dsa.memory.trim"
TRIM_PROPERTY_CONTEXT = f"{TRIM_PROPERTY} u:object_r:dsa_memory_prop:s0 exact bool"
# How many MiB the next gradual trim takes; the daemon sets it before the trim property.
TRIM_SIZE_PROPERTY = "vendor.dsa.memory.trim_size"
TRIM_SIZE_PROPERTY_CONTEXT = f"{TRIM_SIZE_PROPERTY} u:object_r:dsa_memory_prop:s0 exact int"
# init sets this when Android has finished booting, and the daemon tells the host.
BOOT_PROPERTY = "vendor.dsa.boot_completed"
BOOT_PROPERTY_CONTEXT = f"{BOOT_PROPERTY} u:object_r:dsa_boot_prop:s0 exact bool"
# dsa-channeld sets this to 1 while the user's developer mode is on and to 0 when it is off;
# dsa-channeld.rc starts and stops adbd to match.
ADB_PROPERTY = "vendor.dsa.adb"
ADB_PROPERTY_CONTEXT = f"{ADB_PROPERTY} u:object_r:dsa_adb_prop:s0 exact bool"
# dsa-channeld sets these as the host says: which host folders are mounted in the shared
# storage, and the port and token the shared storage is served to the host on.
# dsa-channeld.rc runs the mounts and the server when they change (files.cil).
SHARES_PROPERTY = "vendor.dsa.shares"
SHARES_PROPERTY_CONTEXT = f"{SHARES_PROPERTY} u:object_r:dsa_shares_prop:s0 exact string"
FILES_PROPERTY = "vendor.dsa.files"
FILES_PROPERTY_CONTEXT = f"{FILES_PROPERTY} u:object_r:dsa_files_prop:s0 exact string"
# Features the VM lacks; SystemConfig drops them wherever else they are declared.
UNAVAILABLE_FEATURES = "VENDOR/etc/permissions/dsa-unavailable-features.xml"

# The guest's control channel daemon, its init script, and the policy that gives it a domain
# of its own: the one that may read the boot's secret from /proc/bootconfig.
CHANNELD_BINARY = "VENDOR/bin/dsa-channeld"
CHANNELD_RC = "VENDOR/etc/init/dsa-channeld.rc"
CHANNELD_CONTEXT = "/vendor/bin/dsa-channeld u:object_r:dsa_channeld_exec:s0"
# The binder service the daemon serves the agent and the hwcomposer through: the vendor
# manifest declares it, which servicemanager requires of a VINTF-stable service, and
# vendor_service_contexts gives it the label the policy grants on.
CHANNEL_INSTANCE = "io.github.thisisthepy.dsa.channel.IChannel/default"
CHANNEL_MANIFEST = "VENDOR/etc/vintf/manifest/dsa-channel.xml"
VENDOR_SERVICE_CONTEXTS = "VENDOR/etc/selinux/vendor_service_contexts"
CHANNEL_SERVICE_CONTEXT = f"{CHANNEL_INSTANCE} u:object_r:dsa_channel_service:s0"
# The agent: a privileged app in system_ext, signed with the platform key, which reports
# each app task to the host and starts and ends tasks for it. The framework refuses to boot
# when a privileged app requests a privileged permission its allowlist does not grant.
AGENT_APK = "SYSTEM_EXT/priv-app/DsaAgent/DsaAgent.apk"
AGENT_PERMISSIONS = "SYSTEM_EXT/etc/permissions/privapp-permissions-dsa-agent.xml"
AGENT_PERMISSIONS_SOURCE = REPO / "dsa/guest/agent/privapp-permissions-dsa-agent.xml"
# The runtime permissions the framework grants the agent from the first boot: the storage
# ones, without which Android does not let an installer write an app's folder under
# Android/obb/, where the agent puts the expansion files of the apps the host installs.
AGENT_DEFAULT_PERMISSIONS = "SYSTEM_EXT/etc/default-permissions/default-permissions-dsa-agent.xml"
AGENT_DEFAULT_PERMISSIONS_SOURCE = REPO / "dsa/guest/agent/default-permissions-dsa-agent.xml"
AGENT_APKCERTS = (
    'name="DsaAgent.apk" certificate="build/make/target/product/security/platform.x509.pem" '
    'private_key="build/make/target/product/security/platform.pk8" partition="system_ext"'
)
# A static overlay of framework settings, built with aapt2 against the image's own
# framework-res. The tap timeout is how long a view inside anything that scrolls waits before
# it shows that it is pressed, in case the touch turns into a scroll: 100 ms by default, and
# every list row, Settings' included, is inside a RecyclerView. A mouse press is never the
# start of a scroll, and the pressed ripple then starts 100 ms after the click. At 0 a touch
# that becomes a scroll shows the row pressed until it moves past the touch slop.
FRAMEWORK_OVERLAY_APK = "VENDOR/overlay/DsaFrameworkOverlay.apk"
FRAMEWORK_OVERLAY_PACKAGE = "io.github.thisisthepy.dsa.overlay.framework"
FRAMEWORK_OVERLAY_INTEGERS = {"config_tapTimeoutMillis": 0}
FRAMEWORK_OVERLAY_APKCERTS = (
    'name="DsaFrameworkOverlay.apk" certificate="build/make/target/product/security/platform.x509.pem" '
    'private_key="build/make/target/product/security/platform.pk8" partition="vendor"'
)
FRAMEWORK_OVERLAY_FS_CONFIG = [
    "vendor/overlay 0 0 755 capabilities=0x0",
    "vendor/overlay/DsaFrameworkOverlay.apk 0 0 644 capabilities=0x0",
]
VENDOR_OVERLAY_CONTEXT = "/vendor/overlay(/.*)? u:object_r:vendor_overlay_file:s0"
# A static overlay of Settings, built the same way. It replaces SettingsLib's toolbar back
# icon, whose circle is drawn 8 dp right of the centre of the button that holds it. The
# framework centres the button's hover layer and ripple on the button, so both showed left of
# the circle, at every density. The replacement centres the circle in the same 56 dp.
SETTINGS_OVERLAY_APK = "VENDOR/overlay/DsaSettingsOverlay.apk"
SETTINGS_OVERLAY_PACKAGE = "io.github.thisisthepy.dsa.overlay.settings"
SETTINGS_OVERLAY_TARGET = "com.android.settings"
SETTINGS_BACK_ICON = "settingslib_expressive_icon_back"
SETTINGS_BACK_INSET_DP = 8
# The same overlay gives Settings' preference lists room below their last row, about what a
# phone's navigation bar leaves there. A DSA window has no navigation bar and the guest
# reports no bottom inset, and that inset is the only space Settings keeps under a list, so
# the last row sat on the window's bottom edge. The space is the list's own, like an app's
# content margin; nothing tells apps there is a navigation bar. The home page
# (TopLevelSettings) sets its list's padding in code, which replaces this layout's: on Android
# 16 that call is skipped, and Android 17 takes the padding it gives a freeform window from
# dashboard_padding_bottom, which this overlay sets too (SETTINGS_CODE_LIST_MARGIN).
SETTINGS_LIST_LAYOUT = "preference_recyclerview"
SETTINGS_LIST_STYLE_ATTR = "preferenceFragmentListStyle"
SETTINGS_LIST_BOTTOM_MARGIN_DP = LIST_BOTTOM_MARGIN_DP
# The overlay names what it replaces in a resource map (android:resourcesMap) instead of by
# matching names. Only then does the framework rewrite the overlay layout's references to
# its own id and attribute into Settings' ids, which the code and the theme look up. The
# id and the attribute map onto themselves: nothing reads their values.
SETTINGS_OVERLAY_MAP = "dsa_overlay"
SETTINGS_OVERLAID = [
    f"drawable/{SETTINGS_BACK_ICON}",
    f"layout/{SETTINGS_LIST_LAYOUT}",
    "id/recycler_view",
    f"attr/{SETTINGS_LIST_STYLE_ATTR}",
]
# SettingsLib's arrow (packages/SettingsLib/CollapsingToolbarBaseActivity, Android 16).
SETTINGS_BACK_ARROW = (
    "M3.626,9L8.526,13.9C8.726,14.1 8.817,14.333 8.801,14.6C8.801,14.867 8.701,15.1 8.501,15.3"
    "C8.301,15.483 8.067,15.583 7.801,15.6C7.534,15.6 7.301,15.5 7.101,15.3L0.501,8.7"
    "C0.401,8.6 0.326,8.492 0.276,8.375C0.242,8.258 0.226,8.133 0.226,8"
    "C0.226,7.867 0.242,7.742 0.276,7.625C0.326,7.508 0.401,7.4 0.501,7.3L7.101,0.7"
    "C7.284,0.517 7.509,0.425 7.776,0.425C8.059,0.425 8.301,0.517 8.501,0.7"
    "C8.701,0.9 8.801,1.142 8.801,1.425C8.801,1.692 8.701,1.925 8.501,2.125L3.626,7H14.801"
    "C15.084,7 15.317,7.1 15.501,7.3C15.701,7.483 15.801,7.717 15.801,8"
    "C15.801,8.283 15.701,8.525 15.501,8.725C15.317,8.908 15.084,9 14.801,9H3.626Z"
)
SETTINGS_OVERLAY_APKCERTS = (
    'name="DsaSettingsOverlay.apk" certificate="build/make/target/product/security/platform.x509.pem" '
    'private_key="build/make/target/product/security/platform.pk8" partition="vendor"'
)
SETTINGS_OVERLAY_FS_CONFIG = [
    "vendor/overlay 0 0 755 capabilities=0x0",
    "vendor/overlay/DsaSettingsOverlay.apk 0 0 644 capabilities=0x0",
]

SYSTEM_EXT_FS_CONFIG = "META/system_ext_filesystem_config.txt"
AGENT_FS_CONFIG = [
    "system_ext/priv-app/DsaAgent 0 0 755 capabilities=0x0",
    "system_ext/priv-app/DsaAgent/DsaAgent.apk 0 0 644 capabilities=0x0",
    "system_ext/etc/permissions/privapp-permissions-dsa-agent.xml 0 0 644 capabilities=0x0",
    "system_ext/etc/default-permissions 0 0 755 capabilities=0x0",
    "system_ext/etc/default-permissions/default-permissions-dsa-agent.xml 0 0 644 capabilities=0x0",
]


def update_vendor_file_contexts(fc_text, channeld=False, hwcomposer=False, power=False, codec2=False,
                                audio=False, gnss=False, sensors=False,
                                camera=False):
    """Adds same_process_hal_file contexts for guest graphics libraries if not present, and
    opens the DRM cards to every graphics client.

    Cuttlefish keeps its cards (graphics_device) to the composer and has clients map
    buffers through virtio-gpu's render node. Under Hyper-V, minigbm's mapper, which runs
    in every process that maps a graphics buffer, opens the vkms card instead, so the cards
    get the label clients may open. The lines are rewritten in place: a second
    specification for the same path would conflict.
    """
    lines = [
        line.replace("u:object_r:graphics_device:s0", "u:object_r:gpu_device:s0")
        if line.startswith("/dev/dri/card") else line
        for line in fc_text.splitlines()
    ]
    entries = [
        (r"/vendor/lib64/hw/vulkan\.ranchu\.so", "/vendor/lib64/hw/vulkan\\.ranchu\\.so u:object_r:same_process_hal_file:s0"),
        (r"/vendor/lib64/libOpenglSystemCommon\.so", "/vendor/lib64/libOpenglSystemCommon\\.so u:object_r:same_process_hal_file:s0"),
    ]
    if channeld:
        entries.append((r"/vendor/bin/dsa-channeld", CHANNELD_CONTEXT))
    if hwcomposer:
        entries.append((r"/vendor/bin/hw/dsa-hwcomposer", HWCOMPOSER_CONTEXT))
    if power:
        entries += POWER_CONTEXTS
    if codec2:
        entries.append((r"/vendor/bin/hw/dsa-codec2", CODEC2_CONTEXT))
    if audio:
        entries.append((r"/vendor/bin/hw/dsa-audio", AUDIO_CONTEXT))
    if gnss:
        entries.append((r"/vendor/bin/hw/dsa-gnss", GNSS_CONTEXT))
    if sensors:
        entries.append((r"/vendor/bin/hw/dsa-sensors", SENSORS_CONTEXT))
    if camera:
        entries.append((r"/vendor/bin/hw/dsa-camera", CAMERA_CONTEXT))
    # Usually there already, as /(vendor|system/vendor)/overlay(/.*)?.
    entries.append(("overlay(/.*)?", VENDOR_OVERLAY_CONTEXT))
    for pattern, rule in entries:
        if not any(pattern in line for line in lines):
            lines.append(rule)
    return "\n".join(lines) + ("\n" if lines else "")


def update_vendor_service_contexts(text, codec2=False, channeld=True, camera=False):
    """Labels the daemon's binder service in vendor_service_contexts if it is not yet, the
    codec HAL's when `codec2` is set, and the camera HAL's when `camera` is set."""
    lines = text.splitlines()
    if channeld and not any(line.split()[:1] == [CHANNEL_INSTANCE] for line in lines):
        lines.append(CHANNEL_SERVICE_CONTEXT)
    if codec2 and not any(line.split()[:1] == [CODEC2_INSTANCE] for line in lines):
        lines.append(CODEC2_SERVICE_CONTEXT)
    if camera and not any(line.split()[:1] == [CAMERA_INSTANCE] for line in lines):
        lines.append(CAMERA_SERVICE_CONTEXT)
    return "\n".join(lines) + "\n"


def update_vendor_property_contexts(text):
    """Gives the daemon's properties their types in vendor_property_contexts if they have none."""
    lines = text.splitlines()
    if not any(line.split()[:1] == [RECLAIM_PROPERTY] for line in lines):
        lines.append(RECLAIM_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [RECLAIM_STEP_PROPERTY] for line in lines):
        lines.append(RECLAIM_STEP_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [TRIM_PROPERTY] for line in lines):
        lines.append(TRIM_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [TRIM_SIZE_PROPERTY] for line in lines):
        lines.append(TRIM_SIZE_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [BOOT_PROPERTY] for line in lines):
        lines.append(BOOT_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [ADB_PROPERTY] for line in lines):
        lines.append(ADB_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [SHARES_PROPERTY] for line in lines):
        lines.append(SHARES_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [FILES_PROPERTY] for line in lines):
        lines.append(FILES_PROPERTY_CONTEXT)
    return "\n".join(lines) + "\n"


# The partitions' file contexts, in the order the Android build joins them into
# META/file_contexts.bin. The partition images are labelled from that file, not from these.
PARTITION_FILE_CONTEXTS = [
    "SYSTEM/etc/selinux/plat_file_contexts",
    "SYSTEM_EXT/etc/selinux/system_ext_file_contexts",
    "PRODUCT/etc/selinux/product_file_contexts",
    "VENDOR/etc/selinux/vendor_file_contexts",
    "ODM/etc/selinux/odm_file_contexts",
]


def compile_file_contexts(otatools, text):
    """META/file_contexts.bin made from `text`: compiled with otatools' sefcontext_compile
    when it has one, otherwise the text itself, which libselinux reads as text because it
    lacks the compiled form's magic number."""
    compiler = Path(otatools) / "bin/sefcontext_compile" if otatools else None
    if compiler is None or not compiler.exists():
        return text.encode()
    compiler.chmod(0o755)
    with tempfile.TemporaryDirectory() as tmp:
        source, out = Path(tmp) / "file_contexts", Path(tmp) / "file_contexts.bin"
        source.write_text(text)
        run(compiler, "-o", out, source)
        return out.read_bytes()


def add_filesystem_config(fs_config_text, rules):
    """Adds each rule (path uid gid mode capabilities) whose path the configuration lacks."""
    lines = fs_config_text.splitlines()
    for rule in rules:
        path = rule.split(" ", 1)[0]
        if not any(line.startswith(path + " ") for line in lines):
            lines.append(rule)
    return "\n".join(lines) + ("\n" if lines else "")


def update_vendor_filesystem_config(fs_config_text, channeld=False, hwcomposer=False, power=False,
                                    codec2=False, audio=False, gnss=False, sensors=False,
                                    camera=False):
    """Adds filesystem configuration entries for the files DSA adds to vendor if not present."""
    lines = fs_config_text.splitlines()
    entries = [
        ("vendor/lib64/hw/vulkan.ranchu.so", "vendor/lib64/hw/vulkan.ranchu.so 0 0 644 capabilities=0x0"),
        ("vendor/lib64/libOpenglSystemCommon.so", "vendor/lib64/libOpenglSystemCommon.so 0 0 644 capabilities=0x0"),
        ("vendor/etc/init/dsa-memory.rc", "vendor/etc/init/dsa-memory.rc 0 0 644 capabilities=0x0"),
        ("vendor/etc/selinux/vendor_property_contexts",
         "vendor/etc/selinux/vendor_property_contexts 0 0 644 capabilities=0x0"),
        ("vendor/etc/permissions/dsa-unavailable-features.xml",
         "vendor/etc/permissions/dsa-unavailable-features.xml 0 0 644 capabilities=0x0"),
    ]
    if channeld:
        entries += [
            ("vendor/bin/dsa-channeld", "vendor/bin/dsa-channeld 0 2000 755 capabilities=0x0"),
            ("vendor/etc/init/dsa-channeld.rc", "vendor/etc/init/dsa-channeld.rc 0 0 644 capabilities=0x0"),
            ("vendor/etc/vintf/manifest/dsa-channel.xml",
             "vendor/etc/vintf/manifest/dsa-channel.xml 0 0 644 capabilities=0x0"),
            ("vendor/etc/selinux/vendor_service_contexts",
             "vendor/etc/selinux/vendor_service_contexts 0 0 644 capabilities=0x0"),
        ]
    if hwcomposer:
        entries += [
            ("vendor/bin/hw/dsa-hwcomposer", "vendor/bin/hw/dsa-hwcomposer 0 2000 755 capabilities=0x0"),
            ("vendor/etc/init/dsa-hwcomposer.rc", "vendor/etc/init/dsa-hwcomposer.rc 0 0 644 capabilities=0x0"),
            ("vendor/etc/vintf/manifest/dsa-hwcomposer.xml",
             "vendor/etc/vintf/manifest/dsa-hwcomposer.xml 0 0 644 capabilities=0x0"),
        ]
    for name in ((list(POWER_FILES) if power else []) + (list(CODEC2_FILES) if codec2 else [])
                 + (list(GNSS_FILES) if gnss else [])
                 + (list(SENSORS_FILES) if sensors else [])
                 + (list(CAMERA_FILES) if camera else [])):
        path = name.lower()
        mode = "0 2000 755" if "/bin/" in path else "0 0 644"
        entries.append((path, f"{path} {mode} capabilities=0x0"))
    if audio:
        entries += [
            ("vendor/bin/hw/dsa-audio", "vendor/bin/hw/dsa-audio 0 2000 755 capabilities=0x0"),
            ("vendor/etc/init/dsa-audio.rc", "vendor/etc/init/dsa-audio.rc 0 0 644 capabilities=0x0"),
        ]
    for path, rule in entries:
        if not any(line.startswith(path + " ") for line in lines):
            lines.append(rule)
    return "\n".join(lines) + ("\n" if lines else "")


# Minimal socket permissions needed by guest gfxstream client connections.
GFXSTREAM_ALLOWED_PERMS = {
    "create", "connect", "read", "write", "getattr", "setopt", "getopt", "shutdown"
}
SEPOLICY_FRAGMENT = HERE / "sepolicy/graphics-vsock.cil"
CHANNELD_POLICY = HERE / "sepolicy/channeld.cil"
FILES_POLICY = HERE / "sepolicy/files.cil"
POWER_POLICY = HERE / "sepolicy/power.cil"
GNSS_POLICY = HERE / "sepolicy/gnss.cil"
CODEC2_POLICY = HERE / "sepolicy/codec2.cil"
SENSORS_POLICY = HERE / "sepolicy/sensors.cil"
CAMERA_POLICY = HERE / "sepolicy/camera.cil"
AGENT_POLICY = HERE / "sepolicy/agent.cil"
MEMORY_POLICY = HERE / "sepolicy/memory.cil"
AUDIO_POLICY = HERE / "sepolicy/audio.cil"
ADB_POLICY = HERE / "sepolicy/adb.cil"
KERNEL_POLICY = HERE / "sepolicy/kernel.cil"
ARM_TRANSLATION_POLICY = HERE / "sepolicy/arm-translation.cil"
GAPPS_POLICY = HERE / "sepolicy/gapps.cil"
GPU_RESET_POLICY = HERE / "sepolicy/gpu-reset.cil"


def narrow_vsock_neverallows(text):
    """Narrows neverallow rules on vsock_socket so apps can connect to host graphics."""
    pattern = re.compile(
        r"\(neverallow\s+([^\s()]+)\s+([^\s()]+)\s+\(\s*vsock_socket\s*\(([^)]*)\)\)\)"
    )

    def replace(m):
        src, tgt, perms_str = m.group(1), m.group(2), m.group(3)
        perms = [p for p in perms_str.split() if p not in GFXSTREAM_ALLOWED_PERMS]
        if not perms:
            return ""
        return f"(neverallow {src} {tgt} (vsock_socket ({' '.join(perms)})))"

    narrowed = pattern.sub(replace, text)

    cps_pattern = re.compile(
        r"\(classpermissionset\s+([^\s()]+)\s+\(\s*vsock_socket\s*\(([^)]*)\)\)\)"
    )

    def replace_cps(m):
        name, perms_str = m.group(1), m.group(2)
        perms = [p for p in perms_str.split() if p not in GFXSTREAM_ALLOWED_PERMS]
        if not perms:
            return ""
        return f"(classpermissionset {name} (vsock_socket ({' '.join(perms)})))"

    narrowed = cps_pattern.sub(replace_cps, narrowed)
    return re.sub(r"\n{3,}", "\n\n", narrowed)


def with_versioned_app_domains(rules_text, plat_sepolicy_text):
    """Gives each rule of `rules_text` that names platform_app the same rule for every
    platform_app_<sdk> domain the platform policy declares. Android 17 runs a platform app that
    targets SDK 36 or later in platform_app_<sdk>, not platform_app, so the agent (which targets
    36) was refused everything DSA's policy grants it, the control channel first. Android 16 has
    no such domain, and its rules stay as they are."""
    domains = sorted(set(re.findall(r"\((?:type|typeattribute) (platform_app_\d+)\)", plat_sepolicy_text)))
    if not domains:
        return rules_text
    word = re.compile(r"\bplatform_app\b")
    out = []
    for line in rules_text.splitlines():
        out.append(line)
        if not line.lstrip().startswith(";") and word.search(line):
            out.extend(word.sub(domain, line) for domain in domains)
    return "\n".join(out) + ("\n" if rules_text.endswith("\n") else "")


def patch_plat_sepolicy(plat_sepolicy_text, fragment_text):
    """Narrows neverallows on vsock_socket and appends the graphics vsock policy rules."""
    narrowed = narrow_vsock_neverallows(plat_sepolicy_text)
    rules = []
    for line in fragment_text.splitlines():
        line = line.strip()
        if not line or line.startswith(";"):
            continue
        m = re.match(r"\(allow\s+([^\s()]+)", line)
        if m:
            subject = m.group(1)
            if subject in plat_sepolicy_text:
                rules.append(line)
        else:
            rules.append(line)
    return narrowed.rstrip() + "\n\n" + "\n".join(rules) + "\n"


def compute_plat_sepolicy_sha256(plat_sepolicy_bytes, mapping_bytes):
    """Computes the SHA-256 hash of plat_sepolicy.cil concatenated with the mapping file."""
    h = hashlib.sha256()
    h.update(plat_sepolicy_bytes)
    h.update(mapping_bytes)
    return h.hexdigest() + "\n"


def policy_sources(cil_map, vendor_version, genfs_version):
    """Lists the policy files init compiles at boot, in its order.

    The policy must be built from exactly these: every partition names its mapping file after
    the same version, and the build also carries mapping and genfs files for versions this
    vendor does not use. Mixing those in, or letting one partition's mapping stand in for
    another's, leaves the vendor policy's versioned types mapped to nothing."""
    keys = [
        "SYSTEM/etc/selinux/plat_sepolicy.cil",
        f"SYSTEM/etc/selinux/mapping/{vendor_version}.cil",
        f"SYSTEM/etc/selinux/mapping/{vendor_version}.compat.cil",
        "SYSTEM_EXT/etc/selinux/system_ext_sepolicy.cil",
        f"SYSTEM_EXT/etc/selinux/mapping/{vendor_version}.cil",
        f"SYSTEM_EXT/etc/selinux/mapping/{vendor_version}.compat.cil",
        "PRODUCT/etc/selinux/product_sepolicy.cil",
        f"PRODUCT/etc/selinux/mapping/{vendor_version}.cil",
        "VENDOR/etc/selinux/plat_pub_versioned.cil",
        "VENDOR/etc/selinux/vendor_sepolicy.cil",
        "ODM/etc/selinux/odm_sepolicy.cil",
        f"SYSTEM/etc/selinux/plat_sepolicy_genfs_{genfs_version}.cil",
    ]
    return [key for key in keys if key in cil_map]


def recompile_precompiled_sepolicy(otatools, cil_map, vendor_version, genfs_version, work_dir=None):
    """Recompiles precompiled_sepolicy with secilc from otatools if available."""
    if not otatools:
        return None
    secilc = Path(otatools) / "bin/secilc"
    if not secilc.exists():
        return None
    if not os.access(secilc, os.X_OK):
        try:
            secilc.chmod(0o755)
        except OSError:
            pass
    if not os.access(secilc, os.X_OK):
        return None

    with tempfile.TemporaryDirectory(dir=work_dir) as tmp:
        tmp_path = Path(tmp)
        cil_paths = []
        for key in policy_sources(cil_map, vendor_version, genfs_version):
            # Kept under the partition's path: the mapping files share one name.
            f = tmp_path / key
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_bytes(cil_map[key])
            cil_paths.append(str(f))

        out_policy = tmp_path / "precompiled_sepolicy"
        cmd = [
            str(secilc),
            "-m",
            "-M", "true",
            "-G",
            "-N",
            "-c", "30",
            "-o", str(out_policy),
            "-f", "/dev/null",
            *cil_paths,
        ]
        try:
            res = subprocess.run(cmd, capture_output=True, text=True)
            if res.returncode == 0 and out_policy.exists():
                return out_policy.read_bytes()
            print(f"secilc failed (code {res.returncode}): {res.stderr}")
        except Exception as e:
            print(f"Could not execute secilc: {e}")
        return None


def modify(source, dest, graphics_library=None, vulkan_driver=None, otatools=None, channeld=None,
           agent=None, hwcomposer=None, framework_overlay=None, settings_overlay=None, power_hal=None,
           codec2=None, audio=None, gnss_hal=None,
           sensors=None, camera=None, arm_translation=None, framework=None):
    """Copies target_files, removing the shell apps and the published kernel modules,
    switching to a user build and adding the Hyper-V mount table, the control channel
    daemon when `channeld` names one, the agent when `agent` names its APK, DSA's
    composer in place of Cuttlefish's when `hwcomposer` names one, the framework overlay
    when `framework_overlay` names its APK, the Settings overlay when `settings_overlay`
    names its APK, and DSA's power and health HALs in place of
    Cuttlefish's when `power_hal` names the folder that holds dsa-power and dsa-health, and
    DSA's codec HAL, whose video decoders decode on the host's GPU, when `codec2` names it,
    and DSA's GNSS HAL, which gives the guest the host's location, in place of Cuttlefish's
    when `gnss_hal` names it, and DSA's sensors HAL in place of Cuttlefish's when `sensors`
    names it, and DSA's camera HAL, which lends the host's cameras, in place of Cuttlefish's
    emulated cameras when `camera` names it, and the default ARM translation package's system
    image when `arm_translation` names the package's folder, and the patched framework's files
    in place of the build's own when `framework` maps their target_files names to their bytes."""
    fstab = (HERE / "fstab.dsa").read_bytes()
    default_translator = (Path(arm_translation) / ARM_TRANSLATION_IMAGE).read_bytes() if arm_translation else None
    graphics_payload = Path(graphics_library).read_bytes() if graphics_library else None
    vulkan_payload = Path(vulkan_driver).read_bytes() if vulkan_driver else None
    channeld_payload = Path(channeld).read_bytes() if channeld else None
    agent_payload = Path(agent).read_bytes() if agent else None
    hwcomposer_payload = Path(hwcomposer).read_bytes() if hwcomposer else None
    overlay_payload = Path(framework_overlay).read_bytes() if framework_overlay else None
    settings_overlay_payload = Path(settings_overlay).read_bytes() if settings_overlay else None
    has_hwcomposer = hwcomposer_payload is not None
    power_payloads = None
    if power_hal is not None:
        if channeld is None:
            raise ValueError("the power and health HALs reach the host through dsa-channeld; "
                             "give --channeld too")
        power_payloads = {
            name: (Path(power_hal) / Path(name).name).read_bytes() if source is None else source.read_bytes()
            for name, source in POWER_FILES.items()
        }
    has_power = power_payloads is not None
    codec2_payloads = None
    if codec2 is not None:
        codec2_payloads = {
            name: Path(codec2).read_bytes() if source is None else source.read_bytes()
            for name, source in CODEC2_FILES.items()
        }
    has_codec2 = codec2_payloads is not None
    audio_payload = Path(audio).read_bytes() if audio else None
    has_audio = audio_payload is not None
    gnss_payloads = None
    if gnss_hal is not None:
        if channeld is None:
            raise ValueError("the GNSS HAL reaches the host through dsa-channeld; give --channeld too")
        gnss_payloads = {
            name: Path(gnss_hal).read_bytes() if source is None else source.read_bytes()
            for name, source in GNSS_FILES.items()
        }
    has_gnss = gnss_payloads is not None
    sensors_payloads = None
    if sensors is not None:
        sensors_payloads = {
            name: Path(sensors).read_bytes() if source is None else source.read_bytes()
            for name, source in SENSORS_FILES.items()
        }
    has_sensors = sensors_payloads is not None
    camera_payloads = None
    if camera is not None:
        camera_payloads = {
            name: Path(camera).read_bytes() if source is None else source.read_bytes()
            for name, source in CAMERA_FILES.items()
        }
    has_camera = camera_payloads is not None
    build_tag = image_build_tag([
        graphics_payload, vulkan_payload, channeld_payload, agent_payload, hwcomposer_payload,
        overlay_payload, settings_overlay_payload, *(power_payloads or {}).values(), *(codec2_payloads or {}).values(),
        audio_payload, *(gnss_payloads or {}).values(), *(sensors_payloads or {}).values(),
        *(camera_payloads or {}).values(), *(framework or {}).values(),
    ])
    replaced_framework = set()
    replaced_graphics_payload = False
    replaced_vulkan_payload = False
    kept_modules = re.compile(r"^(VENDOR_BOOT/RAMDISK|VENDOR_DLKM|SYSTEM_DLKM)/lib/modules/.*\.ko$")

    fragment_text = SEPOLICY_FRAGMENT.read_text() if SEPOLICY_FRAGMENT.exists() else ""
    cil_map = {}
    new_plat_bytes = None
    new_sha256 = None
    recompiled_policy = None

    if agent_payload is not None:
        with zipfile.ZipFile(source) as z:
            if SYSTEM_EXT_FS_CONFIG not in z.namelist():
                raise ValueError(f"target_files has no {SYSTEM_EXT_FS_CONFIG}, so there is no system_ext "
                                 "partition to install the agent in")

    with zipfile.ZipFile(source) as zin, zipfile.ZipFile(dest, "w", zipfile.ZIP_STORED, allowZip64=True) as zout:
        names = set(zin.namelist())
        for name in names:
            if name.endswith(".cil"):
                cil_map[name] = zin.read(name)

        if "SYSTEM/etc/selinux/plat_sepolicy.cil" in cil_map:
            plat_orig = cil_map["SYSTEM/etc/selinux/plat_sepolicy.cil"].decode()
            plat_patched = patch_plat_sepolicy(plat_orig, fragment_text)
            # The memory controls the vendor scripts write, and the reclaim property.
            plat_patched += "\n" + MEMORY_POLICY.read_text()
            # adbd listening on vsock, and the property that starts it.
            plat_patched += "\n" + ADB_POLICY.read_text()
            # The kernel thread whose devtmpfs nodes nothing uses.
            plat_patched += "\n" + KERNEL_POLICY.read_text()
            # init mounting a user's ARM translation package over /system.
            plat_patched += "\n" + ARM_TRANSLATION_POLICY.read_text()
            # init laying a user's Google apps over /product and /system_ext.
            plat_patched += "\n" + GAPPS_POLICY.read_text()
            if framework is not None:
                # SurfaceFlinger telling the apps about a GPU reset.
                plat_patched += "\n" + GPU_RESET_POLICY.read_text()
            if channeld_payload is not None:
                # Its rules name the domain they declare, so they go in whole.
                plat_patched += "\n" + CHANNELD_POLICY.read_text()
                # The daemon's file sharing jobs, which name its program's label.
                plat_patched += "\n" + FILES_POLICY.read_text()
            if has_power:
                # The power and health HALs reach the daemon.
                plat_patched += "\n" + POWER_POLICY.read_text()
            if has_codec2:
                # The codec HAL reaches the host's video port.
                plat_patched += "\n" + CODEC2_POLICY.read_text()
            if has_audio:
                # The audio module's socket to the host.
                plat_patched += "\n" + AUDIO_POLICY.read_text()
            if has_gnss:
                # The GNSS HAL reaches the daemon.
                plat_patched += "\n" + GNSS_POLICY.read_text()
            if has_sensors:
                # The sensors HAL reaches the host's sensor port.
                plat_patched += "\n" + SENSORS_POLICY.read_text()
            if has_camera:
                # The camera HAL reaches the host's camera port.
                plat_patched += "\n" + CAMERA_POLICY.read_text()
            if agent_payload is not None:
                # What the agent needs to put the guest into Doze and set its battery.
                plat_patched += "\n" + AGENT_POLICY.read_text()
            # DSA's rules for platform_app hold for Android 17's per-SDK app domains too.
            base = len(narrow_vsock_neverallows(plat_orig).rstrip())
            plat_patched = plat_patched[:base] + with_versioned_app_domains(plat_patched[base:], plat_orig)
            new_plat_bytes = plat_patched.encode()
            cil_map["SYSTEM/etc/selinux/plat_sepolicy.cil"] = new_plat_bytes

            mapping_bytes = b""
            vers = None
            if "VENDOR/etc/selinux/plat_sepolicy_vers.txt" in names:
                vers = zin.read("VENDOR/etc/selinux/plat_sepolicy_vers.txt").decode().strip()
                mapping_key = f"SYSTEM/etc/selinux/mapping/{vers}.cil"
                if mapping_key in cil_map:
                    mapping_bytes = cil_map[mapping_key]
            if not mapping_bytes:
                for k in cil_map:
                    if k.startswith("SYSTEM/etc/selinux/mapping/") and k.endswith(".cil"):
                        mapping_bytes = cil_map[k]
                        break

            new_sha256 = compute_plat_sepolicy_sha256(new_plat_bytes, mapping_bytes)
            # init reads the genfs version from the vendor and uses this one when it is absent.
            genfs_version = "202404"
            if "VENDOR/etc/selinux/genfs_labels_version.txt" in names:
                genfs_version = zin.read("VENDOR/etc/selinux/genfs_labels_version.txt").decode().strip()
            if vers:
                recompiled_policy = recompile_precompiled_sepolicy(otatools, cil_map, vers, genfs_version)

        # The partition images are labelled from META/file_contexts.bin, so the labels added
        # to vendor_file_contexts reach the image only once that is rebuilt from them.
        file_contexts = []
        for name in PARTITION_FILE_CONTEXTS:
            text = zin.read(name).decode() if name in names else ""
            if name == "VENDOR/etc/selinux/vendor_file_contexts":
                text = update_vendor_file_contexts(text, channeld_payload is not None, has_hwcomposer,
                                                   has_power, codec2=has_codec2, audio=has_audio, gnss=has_gnss, sensors=has_sensors, camera=has_camera)
            file_contexts.append(text if text.endswith("\n") or not text else text + "\n")
        compiled_file_contexts = compile_file_contexts(otatools, "".join(file_contexts))

        # Every other variant of a module whose APEX goes: apexd would activate it instead.
        removed_variants = set(replaced_apex_variants(
            names, zin.read, removed_apexes(has_hwcomposer, has_power, has_gnss, has_sensors, has_camera)))
        removed_variant_stems = {apex_file_stem(name) for name in removed_variants}
        for info in zin.infolist():
            name = info.filename
            if (removed(name, has_hwcomposer, has_power, has_gnss, has_sensors, has_camera)
                    or name in removed_variants):
                if name.endswith((".apk", ".apex", ".capex")):
                    print(f"removing {name}", flush=True)
                continue
            if (has_sensors and name.startswith("VENDOR/etc/vintf/manifest/")
                    and other_sensors_manifest(name, zin.read(name))):
                print(f"removing {name}, which declares another sensors HAL", flush=True)
                continue
            if kept_modules.match(name):
                continue
            if name.startswith("IMAGES/"):
                # Rebuilt after signing from the modified contents.
                continue
            if name.endswith("/etc/selinux/precompiled_sepolicy"):
                if recompiled_policy is not None:
                    data = recompiled_policy
                else:
                    continue  # Delete precompiled_sepolicy so the device compiles it at boot.
            elif name == "SYSTEM/etc/selinux/plat_sepolicy.cil" and new_plat_bytes is not None:
                data = new_plat_bytes
            elif new_sha256 is not None and name in {
                "SYSTEM/etc/selinux/plat_sepolicy_and_mapping.sha256",
                "VENDOR/etc/selinux/precompiled_sepolicy.plat_sepolicy_and_mapping.sha256",
                "ODM/etc/selinux/precompiled_sepolicy.plat_sepolicy_and_mapping.sha256",
            }:
                data = new_sha256.encode()
            elif name == "META/file_contexts.bin":
                data = compiled_file_contexts
            elif name == GFXSTREAM_LIBRARY and graphics_payload is not None:
                data = graphics_payload
                replaced_graphics_payload = True
            elif name == VULKAN_RANCHU_LIBRARY and vulkan_payload is not None:
                data = vulkan_payload
                replaced_vulkan_payload = True
            elif framework is not None and name in framework:
                data = framework[name]
                replaced_framework.add(name)
            else:
                data = zin.read(name)
                if name in PROP_FILES:
                    data = edit_props(data.decode(), codec2=has_codec2,
                                      vendor=name in VENDOR_PROP_FILES,
                                      build_tag=build_tag,
                                      product=name == PRODUCT_PROP_FILE,
                                      network=name == NETWORK_PROPS_FILE).encode()
                    if name == ARM_TRANSLATION_PRODUCT_PROPS:
                        data = add_arm_translation_import(add_gapps_import(data.decode())).encode()
                elif name in IDENTITY_PROP_FILES:
                    data = edit_identity(data.decode(), build_tag=build_tag).encode()
                elif name.rsplit("/", 1)[-1].startswith("fstab.") and kernel_disk_names(data.decode(errors="replace")):
                    data = without_kernel_disk_entries(data.decode()).encode()
                elif name == WIFI_RC:
                    data = edit_wifi_rc(data.decode()).encode()
                elif name.endswith(SETTINGS_APK):
                    data = edit_settings_apk(data, name)
                elif name.endswith(DOCUMENTSUI_APK):
                    data = edit_documentsui_apk(data, name)
                elif name in PERMISSION_APEXES:
                    data = edit_permission_apex(data, name, otatools, Path(dest).parent)
                elif name.endswith("lib/modules/modules.load") or name.endswith("lib/modules/modules.dep"):
                    data = b""
                elif name == "META/apexkeys.txt":
                    data = b"".join(
                        line + b"\n" for line in data.splitlines()
                        if not any(f'name="{a}.apex"' in line.decode()
                                   for a in removed_apexes(has_hwcomposer, has_power, has_gnss, has_sensors, has_camera)
                                   + sorted(removed_variant_stems))
                    )
                elif name == "META/apkcerts.txt":
                    data = b"".join(
                        line + b"\n" for line in data.splitlines()
                        if not any(f'name="{a}.apk"' in line.decode()
                                   for a in REMOVED_APPS + ["DsaAgent", "DsaFrameworkOverlay",
                                                    "DsaSettingsOverlay"])
                    )
                    if agent_payload is not None:
                        data += AGENT_APKCERTS.encode() + b"\n"
                    if overlay_payload is not None:
                        data += FRAMEWORK_OVERLAY_APKCERTS.encode() + b"\n"
                    if settings_overlay_payload is not None:
                        data += SETTINGS_OVERLAY_APKCERTS.encode() + b"\n"
                elif name == SYSTEM_FS_CONFIG:
                    data = add_filesystem_config(data.decode(),
                                                 TRACE_FS_CONFIG + ARM_TRANSLATION_FS_CONFIG
                                                 + (ARM_TRANSLATION_DEFAULT_FS_CONFIG
                                                    if default_translator is not None else [])
                                                 + GAPPS_FS_CONFIG).encode()
                elif name == SYSTEM_EXT_FS_CONFIG and agent_payload is not None:
                    data = add_filesystem_config(data.decode(), AGENT_FS_CONFIG).encode()
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
                    if name == "META/vendor_filesystem_config.txt":
                        text = update_vendor_filesystem_config(text, channeld_payload is not None, has_hwcomposer,
                                                               has_power, codec2=has_codec2, audio=has_audio, gnss=has_gnss, sensors=has_sensors, camera=has_camera)
                        if overlay_payload is not None:
                            text = add_filesystem_config(text, FRAMEWORK_OVERLAY_FS_CONFIG)
                        if settings_overlay_payload is not None:
                            text = add_filesystem_config(text, SETTINGS_OVERLAY_FS_CONFIG)
                    data = text.encode()
                elif name in ("VENDOR/etc/selinux/vendor_file_contexts", "META/vendor_file_contexts"):
                    data = update_vendor_file_contexts(data.decode(), channeld_payload is not None,
                                                       has_hwcomposer, has_power, codec2=has_codec2,
                                                       audio=has_audio, gnss=has_gnss, sensors=has_sensors, camera=has_camera).encode()
                elif name == VENDOR_SERVICE_CONTEXTS and (channeld_payload is not None or has_codec2
                                                          or has_camera):
                    data = update_vendor_service_contexts(
                        data.decode(), codec2=has_codec2, channeld=channeld_payload is not None,
                        camera=has_camera).encode()
                elif has_audio and AUDIO_POLICY_CONFIG.match(name):
                    data = without_primary_audio_module(data.decode()).encode()
                elif name == VENDOR_PROPERTY_CONTEXTS:
                    data = update_vendor_property_contexts(data.decode()).encode()
            info.compress_type = zipfile.ZIP_STORED
            zout.writestr(info, data)
        # What the published build compiled, for the apps the image keeps: compile_apps
        # compiles an app again once its dex no longer matches, and the checks hold the image
        # to it.
        prebuilt = {apk: entry for apk, entry in prebuilt_oat(names, zin.read).items()
                    if not removed(apk, has_hwcomposer, has_power, has_gnss, has_sensors, has_camera)}
        zout.writestr(PREBUILT_OAT_RECORD, json.dumps(prebuilt, indent=2, sort_keys=True) + "\n")
        for directory in FSTAB_DIRS:
            zout.writestr(directory + "fstab.dsa", fstab)
        zout.writestr(MEMORY_RC, (HERE / "dsa-memory.rc").read_bytes())
        zout.writestr(TRACE_RC, (HERE / "dsa-trace.rc").read_bytes())
        zout.writestr(CONSOLE_RC, (HERE / "dsa-console.rc").read_bytes())
        zout.writestr(ARM_TRANSLATION_RC, (HERE / "dsa-arm-translation.rc").read_bytes())
        for target, source_file in ARM_TRANSLATION_PROPS.items():
            zout.writestr(target, source_file.read_bytes())
        if default_translator is not None:
            zout.writestr(ARM_TRANSLATION_DEFAULT_IMAGE, default_translator)
            zout.writestr(ARM_TRANSLATION_DEFAULT_PROPS,
                          ARM_TRANSLATION_PROPS["SYSTEM/etc/dsa/arm-translation-on.prop"].read_bytes())
        zout.writestr(GAPPS_RC, (HERE / "dsa-gapps.rc").read_bytes())
        for target, source_file in GAPPS_PROPS.items():
            zout.writestr(target, source_file.read_bytes())
        if VENDOR_PROPERTY_CONTEXTS not in names:
            zout.writestr(VENDOR_PROPERTY_CONTEXTS, update_vendor_property_contexts(""))
        zout.writestr(UNAVAILABLE_FEATURES, (HERE / "unavailable-features.xml").read_bytes())
        if has_hwcomposer:
            zout.writestr(HWCOMPOSER_BINARY, hwcomposer_payload)
            zout.writestr(HWCOMPOSER_RC, (HWCOMPOSER_DIR / "dsa-hwcomposer.rc").read_bytes())
            zout.writestr(HWCOMPOSER_MANIFEST, (HWCOMPOSER_DIR / "dsa-hwcomposer.xml").read_bytes())
        if has_power:
            for name, payload in power_payloads.items():
                zout.writestr(name, payload)
        if channeld_payload is not None:
            zout.writestr(CHANNELD_BINARY, channeld_payload)
            zout.writestr(CHANNELD_RC, (HERE / "dsa-channeld.rc").read_bytes())
            zout.writestr(CHANNEL_MANIFEST, (HERE / "dsa-channel-manifest.xml").read_bytes())
        if has_codec2:
            for name, payload in codec2_payloads.items():
                zout.writestr(name, payload)
        if has_audio:
            zout.writestr(AUDIO_BINARY, audio_payload)
            zout.writestr(AUDIO_RC, (AUDIO_DIR / "dsa-audio.rc").read_bytes())
        if has_gnss:
            for name, payload in gnss_payloads.items():
                zout.writestr(name, payload)
        if has_sensors:
            for name, payload in sensors_payloads.items():
                zout.writestr(name, payload)
        if has_camera:
            for name, payload in camera_payloads.items():
                zout.writestr(name, payload)
        if (channeld_payload is not None or has_codec2 or has_camera) and VENDOR_SERVICE_CONTEXTS not in names:
            zout.writestr(VENDOR_SERVICE_CONTEXTS, update_vendor_service_contexts(
                "", codec2=has_codec2, channeld=channeld_payload is not None, camera=has_camera))
        if overlay_payload is not None:
            zout.writestr(FRAMEWORK_OVERLAY_APK, overlay_payload)
        if settings_overlay_payload is not None:
            zout.writestr(SETTINGS_OVERLAY_APK, settings_overlay_payload)
        if agent_payload is not None:
            zout.writestr(AGENT_APK, agent_payload)
            zout.writestr(AGENT_PERMISSIONS, AGENT_PERMISSIONS_SOURCE.read_bytes())
            zout.writestr(AGENT_DEFAULT_PERMISSIONS, AGENT_DEFAULT_PERMISSIONS_SOURCE.read_bytes())
        if vulkan_payload is not None and not replaced_vulkan_payload:
            zout.writestr(VULKAN_RANCHU_LIBRARY, vulkan_payload)
            replaced_vulkan_payload = True
        if "VENDOR/etc/selinux/vendor_file_contexts" not in names:
            zout.writestr(
                "VENDOR/etc/selinux/vendor_file_contexts",
                update_vendor_file_contexts("", channeld_payload is not None, has_hwcomposer, has_power,
                                            codec2=has_codec2, audio=has_audio, gnss=has_gnss, sensors=has_sensors, camera=has_camera).encode(),
            )
    if graphics_payload is not None and not replaced_graphics_payload:
        dest.unlink(missing_ok=True)
        raise ValueError(f"target_files does not contain {GFXSTREAM_LIBRARY}")
    if framework is not None and replaced_framework != set(framework):
        dest.unlink(missing_ok=True)
        raise ValueError("target_files does not contain "
                         + ", ".join(sorted(set(framework) - replaced_framework))
                         + ", which the patched framework replaces")


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


# The system apps users open, compiled to machine code when the image is made. The build
# compiles them only into system_other, which the guest does not have, and only with the
# verify filter. On first boot ART verifies them too and nothing more, so they run
# interpreted and through the JIT, and each screen of Settings took 100 to 350 ms to open.
# Settings' dex is edited above, so no published odex could match it anyway. Each app is
# compiled with speed-profile when it carries a profile (assets/dexopt/baseline.prof), and
# with speed otherwise; Settings always with speed. The odex goes next to the APK, where ART
# takes it as the factory build's (compilation reason prebuilt), so first boot has nothing
# left to compile for these apps. Apps the image lacks are skipped.
# PermissionController is not here: it lives inside the permission APEX, which is signed
# and read-only, and ART takes no odex beside an APK inside an APEX.
SETTINGS_APP = "SYSTEM_EXT/priv-app/Settings/Settings.apk"
COMPILED_APPS = [
    SETTINGS_APP,
    "SYSTEM/priv-app/DocumentsUI/DocumentsUI.apk",
    "SYSTEM/priv-app/PackageInstaller/PackageInstaller.apk",
    "SYSTEM/priv-app/IntentResolver/IntentResolver.apk",
    "SYSTEM/priv-app/SoundPicker/SoundPicker.apk",
    "SYSTEM/app/PrintSpooler/PrintSpooler.apk",
    "SYSTEM_EXT/priv-app/StorageManager/StorageManager.apk",
    "PRODUCT/app/webview/webview.apk",
    AGENT_APK,
]
# The apps whose dex make-image.py edits (edit_settings_apk, edit_documentsui_apk), compiled
# after the edits with the rest. dex2oat verifies every method as it compiles, and refuses an
# edited app with a method the verifier rejects outright: ART would throw a VerifyError
# instead of running it, and the app would close the moment that class loads.
EDITED_APPS = [SETTINGS_APP, "SYSTEM/priv-app/DocumentsUI/DocumentsUI.apk"]
# What differs between the guest architectures DSA makes images for: the kernel's file
# format and name, ART's instruction set and its variant, and the ELF machine number of a
# binary built for the guest. arm64 is 64-bit only: the guest runs no 32-bit ARM code.
ARCHES = {
    "x86_64": {"kernel": "bzImage", "isa": "x86_64", "isa_variant": "x86_64", "elf_machine": 62},
    "arm64": {"kernel": "Image", "isa": "arm64", "isa_variant": "generic", "elf_machine": 183},
}
DEFAULT_ARCH = "x86_64"
# The binaries DSA builds for the guest, where the image keeps them.
DSA_GUEST_BINARIES = [
    "VENDOR/bin/dsa-channeld",
    "VENDOR/bin/hw/dsa-hwcomposer",
    "VENDOR/bin/hw/dsa-codec2",
    "VENDOR/bin/hw/dsa-power",
    "VENDOR/bin/hw/dsa-health",
    "VENDOR/lib64/hw/vulkan.ranchu.so",
    "VENDOR/lib64/libOpenglSystemCommon.so",
]
COMPILED_ISA = ARCHES[DEFAULT_ARCH]["isa"]
COMPILED_ISA_VARIANT = ARCHES[DEFAULT_ARCH]["isa_variant"]
CAPEX_ORIGINAL = "original_apex"
EMBEDDED_PROFILE = "assets/dexopt/baseline.prof"
# Where an app's shared libraries are listed when the build left no odex to read them from:
# the agent declares none.
DEFAULT_CLASS_LOADER_CONTEXT = "PCL[]"
# Compiled against the same boot image as the framework's own odex files.
FRAMEWORK_ODEX = f"SYSTEM/framework/oat/{COMPILED_ISA}/services.odex"


def use_arch(arch):
    """Aims the app compilation and its checks at `arch`'s boot image, and names the device
    after it: dsa_x86_64 or dsa_arm64."""
    global COMPILED_ISA, COMPILED_ISA_VARIANT, FRAMEWORK_ODEX
    if arch not in ARCHES:
        raise ValueError(f"the guest architecture must be one of {', '.join(ARCHES)}, not {arch}")
    COMPILED_ISA = ARCHES[arch]["isa"]
    COMPILED_ISA_VARIANT = ARCHES[arch]["isa_variant"]
    FRAMEWORK_ODEX = f"SYSTEM/framework/oat/{COMPILED_ISA}/services.odex"
    IDENTITY["device"] = IDENTITY["name"] = identity_device(arch)


def identity_device(arch):
    """The device and product name of an image for `arch`."""
    return f"dsa_{arch}"


def image_spec(images, major, arch=DEFAULT_ARCH):
    """The inputs.json entry for Android `major` on `arch`. An entry without an arch is x86_64."""
    for spec in images:
        if spec["android_major"] == major and spec.get("arch", DEFAULT_ARCH) == arch:
            return spec
    raise ValueError(f"inputs.json has no image for Android {major} on {arch}")


def kernel_name(arch):
    """The file name the guest kernel has in an image: bzImage on x86_64, Image on arm64."""
    return ARCHES[arch]["kernel"]


def kernel_format(data):
    """The architecture a kernel file boots on, read from its header: x86_64 for a bzImage
    (its setup header says HdrS at 0x202), arm64 for an arm64 Image (ARM\\x64 at 0x38), and
    None for anything else."""
    if len(data) >= 0x206 and data[0x202:0x206] == b"HdrS":
        return "x86_64"
    if len(data) >= 0x3c and data[0x38:0x3c] == b"ARM\x64":
        return "arm64"
    return None


def elf_machine(data):
    """The e_machine of a little-endian 64-bit ELF file, or None for anything else."""
    if len(data) < 20 or data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        return None
    return int.from_bytes(data[18:20], "little")
PARTITION_DIRS = {"SYSTEM": "system", "SYSTEM_EXT": "system_ext", "PRODUCT": "product",
                  "VENDOR": "vendor", "ODM": "odm"}
PARTITION_FS_CONFIG = {
    "SYSTEM": "META/filesystem_config.txt",
    "SYSTEM_EXT": "META/system_ext_filesystem_config.txt",
    "PRODUCT": "META/product_filesystem_config.txt",
}
OAT_KEYS = ["bootclasspath", "bootclasspath-checksums", "classpath", "compilation-reason",
            "compiler-filter"]


def oat_key_values(odex):
    """The key-value store of an odex's oat header: the filter, the boot class path and its
    checksums, and the class loader context (classpath) it was compiled for."""
    start = odex.find(b"oat\n")
    if start < 0:
        raise ValueError("not an oat file")
    header = odex[start:start + (1 << 20)]
    values = {}
    for key in OAT_KEYS:
        at = header.find(b"\0" + key.encode() + b"\0")
        if at < 0:
            continue
        at += len(key) + 2
        values[key] = header[at:header.index(b"\0", at)].decode()
    return values


def vdex_checksums(vdex):
    """The dex checksums a vdex was made for: the CRC-32 of each classes*.dex in the APK."""
    if vdex[:4] != b"vdex":
        raise ValueError("not a vdex file")
    (sections,) = struct.unpack_from("<I", vdex, 8)
    for i in range(sections):
        kind, offset, size = struct.unpack_from("<III", vdex, 12 + 12 * i)
        if kind == 0:
            return list(struct.unpack_from(f"<{size // 4}I", vdex, offset))
    raise ValueError("the vdex has no checksum section")


def apk_dex_checksums(apk):
    """The CRC-32 of each classes*.dex in an APK, in the order ART reads them."""
    with zipfile.ZipFile(io.BytesIO(apk)) as z:
        dexes = {i.filename: i.CRC for i in z.infolist() if re.fullmatch(r"classes\d*\.dex", i.filename)}
    order = sorted(dexes, key=lambda n: int(n[len("classes"):-len(".dex")] or 1))
    return [dexes[n] for n in order]


def device_path(name):
    """/system_ext/priv-app/... for SYSTEM_EXT/priv-app/..."""
    partition, rest = name.split("/", 1)
    return f"/{PARTITION_DIRS[partition]}/{rest}"


def oat_files(apk):
    """The odex and vdex target_files members ART looks for beside an APK."""
    folder, file = apk.rsplit("/", 1)
    stem = file[:-len(".apk")]
    return (f"{folder}/oat/{COMPILED_ISA}/{stem}.odex", f"{folder}/oat/{COMPILED_ISA}/{stem}.vdex")


def factory_odex(apk):
    """Where the build put the app's own (verify only) odex: in system_other."""
    odex, _ = oat_files(apk)
    return "SYSTEM_OTHER" + device_path(odex)


def oat_filesystem_config(apk):
    """The filesystem configuration member and the rules for an APK's oat folder and files."""
    partition = apk.split("/", 1)[0]
    folder = device_path(apk).rsplit("/", 1)[0].lstrip("/")
    odex, vdex = (device_path(n).lstrip("/") for n in oat_files(apk))
    return PARTITION_FS_CONFIG[partition], [
        f"{folder}/oat 0 0 755 capabilities=0x0",
        f"{folder}/oat/{COMPILED_ISA} 0 0 755 capabilities=0x0",
        f"{odex} 0 0 644 capabilities=0x0",
        f"{vdex} 0 0 644 capabilities=0x0",
    ]


def compiler_filter(apk, has_profile):
    """speed for Settings, speed-profile for an app that carries a profile, speed otherwise."""
    if apk == SETTINGS_APP:
        return "speed"
    return "speed-profile" if has_profile else "speed"


def class_loader_contexts(context, root):
    """The class loader context dex2oat opens (each library at its copy under root) and the
    one it stores (the device's paths). The checksums of the published odex are dropped:
    dex2oat records the ones of the files it opens."""
    stored = re.sub(r"\*\d+", "", context)
    host = re.sub(r"(?<=[\[:#])(/[^\]:#*]+)", lambda m: str(root) + m.group(1), stored)
    return host, stored


def boot_image_locations(bootclasspath, checksums):
    """The device's boot image, as dex2oat names it: the primary image and each extension,
    one per "i;<count>/<checksum>" in the framework odex's bootclasspath-checksums."""
    jars = bootclasspath.split(":")
    locations, first = [], 0
    for part in checksums.split(":"):
        m = re.fullmatch(r"i;(\d+)/[0-9a-f]+", part)
        if not m:
            break
        name = "boot.art" if first == 0 else f"boot-{Path(jars[first]).stem}.art"
        locations.append(f"/system/framework/{name}")
        first += int(m.group(1))
    if not locations:
        raise ValueError(f"no boot image in the checksums {checksums!r}")
    return locations


def apex_jar_sources(bootclasspath, names):
    """The APEX files (target_files members) that hold the boot class path's /apex jars."""
    sources = {}
    for jar in bootclasspath.split(":"):
        if not jar.startswith("/apex/"):
            continue
        apex = jar.split("/")[2]
        found = [f"{p}/apex/{apex}{ext}" for p in PARTITION_DIRS for ext in (".apex", ".capex")
                 if f"{p}/apex/{apex}{ext}" in names]
        if not found:
            raise ValueError(f"the boot class path needs {jar}, but no partition has the {apex} APEX")
        sources[apex] = found[0]
    return sources


def dex2oat_command(dex2oat, apk, dex_location, odex, root, bootclasspath, boot_images, contexts,
                    filter, profile=None, extra=(), app_image=None):
    """The dex2oat command line the build itself uses to compile a system app, aimed at the
    guest's boot image (COMPILED_ISA), with an app image (.art) at `app_image` when given."""
    host_context, stored_context = contexts
    jars = bootclasspath.split(":")
    cmd = [
        dex2oat, "--avoid-storing-invocation",
        # The build's own heap size: the heap must fit in the low 4 GiB, and 2 GiB did not.
        "--runtime-arg", "-Xms64m", "--runtime-arg", "-Xmx512m",
        "--runtime-arg", "-Xbootclasspath:" + ":".join(f"{root}{j}" for j in jars),
        "--runtime-arg", "-Xbootclasspath-locations:" + bootclasspath,
        f"--class-loader-context={host_context}",
        f"--stored-class-loader-context={stored_context}",
        "--boot-image=" + ":".join(f"{root}{i}" for i in boot_images),
        f"--dex-file={apk}", f"--dex-location={dex_location}",
        f"--oat-file={odex}", f"--oat-location={dex_location.rsplit('/', 1)[0]}/oat/{COMPILED_ISA}/{Path(odex).name}",
        f"--android-root={root}/empty",
        f"--instruction-set={COMPILED_ISA}", f"--instruction-set-variant={COMPILED_ISA_VARIANT}",
        "--instruction-set-features=default",
        "--no-generate-debug-info", "--generate-build-id", "--force-determinism",
        "--no-inline-from=core-oj.jar",
        f"--compiler-filter={filter}", "--compilation-reason=prebuilt",
        *extra,
    ]
    if profile is not None:
        cmd += [f"--profile-file={profile}", "--resolve-startup-const-strings=true"]
    if app_image is not None:
        cmd += [f"--app-image-file={app_image}", "--image-format=lz4"]
    return cmd


def compiled_app_problems(z):
    """What is wrong with the compiled system apps in a target_files: an odex or vdex
    missing, made for another dex than the APK's, with a filter other than speed or
    speed-profile, against another boot image than the framework's, or for other shared
    libraries than the build's own."""
    names = set(z.namelist())
    problems = []
    framework = oat_key_values(z.read(FRAMEWORK_ODEX)) if FRAMEWORK_ODEX in names else {}
    for apk in COMPILED_APPS:
        if apk not in names:
            if apk == SETTINGS_APP:
                problems.append(f"{apk} is missing")
            continue
        odex, vdex = oat_files(apk)
        if odex not in names or vdex not in names:
            problems.append(f"{apk} is not compiled ({odex} or its vdex is missing), so it runs "
                            "interpreted and every screen opens slowly")
            continue
        made_for = vdex_checksums(z.read(vdex))
        dex = apk_dex_checksums(z.read(apk))
        if made_for != dex:
            problems.append(f"{vdex} was made for dex checksums {made_for}, but the APK's are {dex}, "
                            "so ART throws the odex away")
        keys = oat_key_values(z.read(odex))
        if keys.get("compiler-filter") not in ("speed", "speed-profile"):
            problems.append(f"{odex} is compiled with {keys.get('compiler-filter')}, not speed")
        if apk == SETTINGS_APP and keys.get("compiler-filter") != "speed":
            problems.append(f"{odex} is compiled with {keys.get('compiler-filter')}, not speed")
        if framework and keys.get("bootclasspath-checksums") != framework.get("bootclasspath-checksums"):
            problems.append(f"{odex} was compiled against boot image {keys.get('bootclasspath-checksums')}, "
                            f"but the guest's is {framework.get('bootclasspath-checksums')}")
        published = factory_odex(apk)
        expected = (oat_key_values(z.read(published)).get("classpath") if published in names
                    else DEFAULT_CLASS_LOADER_CONTEXT)
        if keys.get("classpath") != expected:
            problems.append(f"{odex} was compiled for shared libraries {keys.get('classpath')}, but "
                            f"the app loads {expected}")
    return problems


# The apps the published build compiled in their own partition (not in system_other): each
# APK's odex, vdex and app image (.art) beside it, the filter and the shared libraries it was
# compiled with. make-image.py records them from the published target_files, before any
# edit, in the target_files it makes, so the image can be checked against them: signing
# leaves an APK's dex as it is, so their odex files stay valid, but an APK whose dex DSA
# edits is compiled again, with the same filter, against the same boot image.
PREBUILT_OAT_RECORD = "META/dsa_prebuilt_oat.json"
PREBUILT_OAT_PARTITIONS = "|".join(PARTITION_DIRS)


def prebuilt_oat(names, read):
    """{apk: {"files": [odex, vdex, art...], "filter": f, "classpath": c}} for each APK in a
    target_files whose member names are `names` that has its odex in its own partition, for
    the guest's instruction set. `read` reads a member."""
    names = set(names)
    found = {}
    for odex in sorted(names):
        m = re.fullmatch(rf"((?:{PREBUILT_OAT_PARTITIONS})/.+)/oat/{COMPILED_ISA}/([^/]+)\.odex", odex)
        if not m or f"{m.group(1)}/{m.group(2)}.apk" not in names:
            continue
        keys = oat_key_values(read(odex))
        found[f"{m.group(1)}/{m.group(2)}.apk"] = {
            "files": [odex] + [odex[:-len(".odex")] + ext for ext in (".vdex", ".art")
                               if odex[:-len(".odex")] + ext in names],
            "filter": keys.get("compiler-filter"),
            "classpath": keys.get("classpath", DEFAULT_CLASS_LOADER_CONTEXT),
        }
    return found


def prebuilt_oat_problems(z):
    """What is wrong with the apps the published build compiled (PREBUILT_OAT_RECORD) in a
    target_files: an APK or one of its compiled files gone, a vdex made for another dex than
    the APK's, an odex compiled with another filter than the published build's, or against
    another boot image than the framework's."""
    names = set(z.namelist())
    if PREBUILT_OAT_RECORD not in names:
        return [f"{PREBUILT_OAT_RECORD} is missing, so nothing says which apps the published build "
                "shipped compiled"]
    framework = oat_key_values(z.read(FRAMEWORK_ODEX)) if FRAMEWORK_ODEX in names else {}
    problems = []
    for apk, entry in sorted(json.loads(z.read(PREBUILT_OAT_RECORD)).items()):
        if apk not in names:
            problems.append(f"{apk} is gone, though the published build ships it compiled")
            continue
        missing = [member for member in entry["files"] if member not in names]
        if missing:
            problems.append(f"{apk} lost {', '.join(missing)}, which the published build ships, so it "
                            "runs interpreted until Android compiles it again")
            continue
        odex = entry["files"][0]
        vdex = odex[:-len(".odex")] + ".vdex"
        made_for, dex = vdex_checksums(z.read(vdex)), apk_dex_checksums(z.read(apk))
        if made_for != dex:
            problems.append(f"{vdex} was made for dex checksums {made_for}, but the APK's are {dex}, "
                            "so ART throws the odex away")
        keys = oat_key_values(z.read(odex))
        # An app compiled with speed-profile that carries no profile of its own is compiled
        # again with speed, which compiles all of what speed-profile compiles.
        if keys.get("compiler-filter") not in {entry["filter"]} | (
                {"speed"} if entry["filter"] == "speed-profile" else set()):
            problems.append(f"{odex} is compiled with {keys.get('compiler-filter')}, but the published "
                            f"build compiled it with {entry['filter']}")
        if framework and keys.get("bootclasspath-checksums") != framework.get("bootclasspath-checksums"):
            problems.append(f"{odex} was compiled against boot image {keys.get('bootclasspath-checksums')}, "
                            f"but the guest's is {framework.get('bootclasspath-checksums')}")
    return problems


def compile_jobs(names, read):
    """What compile_apps compiles in a signed target_files: [(apk, filter, class loader
    context, whether it has an app image)]. COMPILED_APPS come first, their filter None (speed,
    or speed-profile when the APK carries a profile, see compiler_filter). Then each app the
    published build compiled (PREBUILT_OAT_RECORD) whose odex no longer matches its APK's dex,
    with the filter, the shared libraries and the app image it was compiled with."""
    names = set(names)
    jobs = []
    for apk in COMPILED_APPS:
        if apk not in names:
            continue
        published = factory_odex(apk)
        context = (oat_key_values(read(published)).get("classpath", DEFAULT_CLASS_LOADER_CONTEXT)
                   if published in names else DEFAULT_CLASS_LOADER_CONTEXT)
        jobs.append((apk, None, context, False))
    record = json.loads(read(PREBUILT_OAT_RECORD)) if PREBUILT_OAT_RECORD in names else {}
    for apk, entry in sorted(record.items()):
        if apk in COMPILED_APPS or apk not in names:
            continue
        vdex = entry["files"][0][:-len(".odex")] + ".vdex"
        if vdex in names and vdex_checksums(read(vdex)) == apk_dex_checksums(read(apk)):
            continue
        app_image = any(member.endswith(".art") for member in entry["files"])
        jobs.append((apk, entry["filter"], entry["classpath"], app_image))
    return jobs


# The key apexer signs a repacked APEX payload with. Signing gives every APEX the image's
# payload key and container key afterwards, and packs the payload once more as it signs the
# apps inside, so this key never reaches the image.
APEX_REPACK_KEY = "external/avb/test/data/testkey_rsa4096.pem"
APEX_REPACK_META = ["apex_manifest.pb", "apex_build_info.pb", "apex_manifest.json"]


def apex_tool_path(otatools):
    """The folders apexer and apex_compression_tool look for the tools they run in."""
    return f"{Path(otatools).resolve() / 'bin'}:{os.environ.get('PATH', '')}"


def apex_repack_command(otatools, meta, payload, image):
    """apexer's command line that packs the folder `payload` into an APEX payload image with
    the manifest and the build information (file contexts, filesystem, ownership) the APEX
    in `meta` was built with: the command sign_target_files_apks runs when it packs an APEX
    again after signing the apps in it."""
    otatools = Path(otatools)
    cmd = [otatools / "bin/apexer", "--force", "--payload_only", "--do_not_check_keyname",
           "--apexer_tool_path", apex_tool_path(otatools),
           "--manifest", meta / "apex_manifest.pb", "--build_info", meta / "apex_build_info.pb",
           "--key", otatools / APEX_REPACK_KEY]
    if (meta / "apex_manifest.json").exists():
        cmd += ["--manifest_json", meta / "apex_manifest.json"]
    return cmd + [payload, image]


def apex_payload(data, name, otatools, folder, env):
    """Unpacks an APEX, or a compressed APEX (`name` ends .capex), into `folder`: the APEX
    itself (folder/original.apex) and its payload's files (folder/payload)."""
    otatools = Path(otatools)
    shutil.rmtree(folder, ignore_errors=True)
    folder.mkdir(parents=True)
    apex = folder / "original.apex"
    if name.endswith(".capex"):
        with zipfile.ZipFile(io.BytesIO(data)) as capex:
            apex.write_bytes(capex.read(CAPEX_ORIGINAL))
    else:
        apex.write_bytes(data)
    run(otatools / "bin/deapexer", "--debugfs_path", otatools / "bin/debugfs_static",
        "--fsckerofs_path", otatools / "bin/fsck.erofs", "extract", apex, folder / "payload", env=env)
    return apex, folder / "payload"


def edit_apex_apks(data, name, edit, otatools, work):
    """An APEX (or a compressed APEX), `data`, with each APK in its payload replaced by
    edit(apk, its name), packed again as sign_target_files_apks packs one: the payload image
    by apexer with the APEX's own manifest and build information, put back in the APEX, and
    compressed again when it was. Signing then signs the apps in it, the payload and the
    container with the image's keys, as for every APEX. Returns `data` when no APK changed."""
    otatools = Path(otatools).resolve()
    env = dict(os.environ, PATH=apex_tool_path(otatools))
    folder = Path(work).resolve() / f"apex-{Path(name).stem}"
    apex, payload = apex_payload(data, name, otatools, folder, env)
    changed = False
    for apk in sorted(payload.rglob("*.apk")):
        before = apk.read_bytes()
        after = edit(before, f"{name}:{apk.relative_to(payload).as_posix()}")
        if after != before:
            apk.write_bytes(after)
            changed = True
    if not changed:
        shutil.rmtree(folder, ignore_errors=True)
        return data
    meta = folder / "meta"
    meta.mkdir()
    with zipfile.ZipFile(apex) as z:
        for member in APEX_REPACK_META:
            if member in z.namelist():
                (meta / member).write_bytes(z.read(member))
    # apexer writes the manifest into the payload itself, as signing's repack does.
    for leftover in ("apex_manifest.pb", "apex_manifest.json"):
        (payload / leftover).unlink(missing_ok=True)
    shutil.rmtree(payload / "lost+found", ignore_errors=True)
    image = folder / "apex_payload.img"
    run(*apex_repack_command(otatools, meta, payload, image), env=env)
    repacked = folder / "repacked.apex"
    with zipfile.ZipFile(apex) as zin, zipfile.ZipFile(repacked, "w") as zout:
        for info in zin.infolist():
            if info.filename != "apex_payload.img":
                zout.writestr(info, zin.read(info))
        zout.write(image, "apex_payload.img", compress_type=zipfile.ZIP_STORED)
    if name.endswith(".capex"):
        compressed = folder / "repacked.capex"
        run(otatools / "bin/apex_compression_tool", "compress", "--apex_compression_tool_path",
            apex_tool_path(otatools), "--input", repacked, "--output", compressed, env=env)
        repacked = compressed
    result = repacked.read_bytes()
    shutil.rmtree(folder, ignore_errors=True)
    return result


def edit_permission_apex(data, name, otatools, work):
    """The permission APEX with PermissionController's lists edited (see
    PERMISSION_CONTROLLER_DIMENSIONS). Without otatools nothing can pack it again, and it is
    left as it is."""
    if otatools is None:
        print(f"not editing {name}: there is no otatools to pack it again", flush=True)
        return data

    def edit(apk, apk_name):
        if not apk_name.endswith("/" + PERMISSION_CONTROLLER_APK):
            return apk
        return edit_permission_controller_apk(apk, apk_name)

    return edit_apex_apks(data, name, edit, otatools, work)


def apk_edits_left(apk, code=None, controllers=None, dimensions=None):
    """What an APK still has of what edit_apk takes out or changes, given the same edits: the
    methods, controllers and compiled XML files edit_apk would change. An empty list means
    the APK has every edit (or nothing they apply to)."""
    left = []
    with zipfile.ZipFile(io.BytesIO(apk)) as z:
        files = set(z.namelist())
        for dex in (sorted(f for f in files if re.fullmatch(r"classes\d*\.dex", f)) if code else []):
            content = z.read(dex)
            if content[:4] != b"dex\n":
                continue
            try:
                left += [f"{dex}: {method}" for method in sorted(set(edit_dex(content, code)[1]))]
            except ValueError as error:
                left.append(f"{dex}: {error}")
        for xml, removed in (controllers or {}).items():
            if xml in files:
                _, found = remove_xml_elements(z.read(xml), "controller", set(removed))
                left += [f"{xml}: {controller}" for controller in found]
        for xml, edits in (dimensions or {}).items():
            if xml in files and set_xml_dimensions(z.read(xml), edits)[1]:
                left.append(f"{xml}: " + ", ".join(f"{element} {attribute:#x} not {dp} dp"
                                                   for element, attribute, dp in edits))
    return left


def permission_controller_problems(signed, otatools, work, env):
    """What is wrong with PermissionController in a signed target_files: its lists without
    the room below their last row, looked for in the permission APEX the image ships."""
    with zipfile.ZipFile(signed) as z:
        found = [n for n in PERMISSION_APEXES if n in z.namelist()]
        if not found:
            return []
        data = z.read(found[0])
    folder = Path(work).resolve() / "apex-check"
    _, payload = apex_payload(data, found[0], otatools, folder, env)
    apks = sorted(payload.rglob(PERMISSION_CONTROLLER_APK))
    problems = [] if apks else [f"{found[0]} has no {PERMISSION_CONTROLLER_APK}"]
    for apk in apks:
        left = apk_edits_left(apk.read_bytes(), dimensions=PERMISSION_CONTROLLER_DIMENSIONS)
        if left:
            problems.append(f"{found[0]}:{apk.relative_to(payload).as_posix()} still has {'; '.join(left)}, "
                            "so the last row of its lists sits on the window's edge")
    shutil.rmtree(folder, ignore_errors=True)
    return problems


def compile_apps(signed, tools_zip, otatools, work, env):
    """Compiles COMPILED_APPS in the signed target_files and adds their odex and vdex files
    and filesystem rules to it. Returns the bytes each app's compiled files take."""
    tools = work / "dexpreopt-tools"
    with zipfile.ZipFile(tools_zip) as z:
        z.extractall(tools)
    for f in tools.iterdir():
        f.chmod(0o755)
    dex2oat = tools / "dex2oatd64"
    profman = tools / "profman"
    extra = (tools / "uffd_gc_flag.txt").read_text().split() if (tools / "uffd_gc_flag.txt").exists() else []
    root = (work / "dexpreopt-root").resolve()
    out = work / "dexpreopt-out"
    out.mkdir(parents=True, exist_ok=True)
    (root / "empty").mkdir(parents=True, exist_ok=True)

    with zipfile.ZipFile(signed) as z:
        names = set(z.namelist())
        framework = oat_key_values(z.read(FRAMEWORK_ODEX))
        bootclasspath = framework["bootclasspath"]
        boot_images = boot_image_locations(bootclasspath, framework["bootclasspath-checksums"])
        for partition, folder in PARTITION_DIRS.items():
            extract_tree(z, f"{partition}/framework/", root / folder / "framework")
        for apex, member in apex_jar_sources(bootclasspath, names).items():
            packed = work / "apex" / Path(member).name
            packed.parent.mkdir(parents=True, exist_ok=True)
            packed.write_bytes(z.read(member))
            if packed.suffix == ".capex":
                # A compressed APEX is a zip that holds the original one.
                decompressed = packed.with_suffix(".apex")
                with zipfile.ZipFile(packed) as capex:
                    decompressed.write_bytes(capex.read(CAPEX_ORIGINAL))
                packed = decompressed
            run(otatools / "bin/deapexer", "--debugfs_path", otatools / "bin/debugfs_static",
                "--fsckerofs_path", otatools / "bin/fsck.erofs", "extract", packed, root / "apex" / apex,
                env=env)
        for image in boot_images:
            if not (root / image.lstrip("/")).parent.joinpath(COMPILED_ISA, Path(image).name).exists():
                raise ValueError(f"the image has no {COMPILED_ISA} boot image at {image}")

        compiled, sizes, rules = {}, {}, {}
        for apk in COMPILED_APPS:
            if apk not in names:
                print(f"not compiling {apk}: the image does not have it", flush=True)
        for apk, filter, context, app_image in compile_jobs(names, z.read):
            data = z.read(apk)
            host_apk = out / apk
            host_apk.parent.mkdir(parents=True, exist_ok=True)
            host_apk.write_bytes(data)
            location = device_path(apk)
            profile = None
            with zipfile.ZipFile(io.BytesIO(data)) as a:
                if filter in (None, "speed-profile") and EMBEDDED_PROFILE in a.namelist():
                    embedded = host_apk.with_suffix(".baseline.prof")
                    embedded.write_bytes(a.read(EMBEDDED_PROFILE))
                    profile = host_apk.with_suffix(".prof")
                    try:
                        run(profman, f"--profile-file={embedded}", f"--reference-profile-file={profile}",
                            f"--apk={host_apk}", f"--dex-location={location}",
                            "--copy-and-update-profile-key", env=env)
                    except subprocess.CalledProcessError:
                        print(f"{apk}: profman could not read its profile, so it is compiled with speed",
                              flush=True)
                        profile = None
            if filter is None:
                filter = compiler_filter(apk, profile is not None)
            elif filter == "speed-profile" and profile is None:
                print(f"{apk}: the published build compiled it with speed-profile and the APK carries no "
                      "profile, so it is compiled with speed", flush=True)
                filter = "speed"
            odex_member, vdex_member = oat_files(apk)
            art_member = odex_member[:-len(".odex")] + ".art"
            odex = out / odex_member
            odex.parent.mkdir(parents=True, exist_ok=True)
            edited = apk in EDITED_APPS or apk not in COMPILED_APPS
            run(*dex2oat_command(dex2oat, host_apk, location, odex, root, bootclasspath, boot_images,
                                 class_loader_contexts(context, root), filter,
                                 profile=profile if filter == "speed-profile" else None,
                                 extra=extra + (["--abort-on-hard-verifier-error"] if edited else []),
                                 app_image=odex.with_suffix(".art") if app_image else None),
                env=dict(env, ANDROID_LOG_TAGS="*:e"))
            members = [odex_member, vdex_member] + ([art_member] if app_image else [])
            for member in members:
                compiled[member] = (out / member).read_bytes()
            sizes[apk] = sum(len(compiled[member]) for member in members)
            kinds = ", ".join(Path(member).suffix[1:] for member in members)
            print(f"compiled {apk} with {filter}: {sizes[apk] >> 10} KiB of {kinds}", flush=True)
            config, app_rules = oat_filesystem_config(apk)
            if app_image:
                app_rules.append(f"{device_path(art_member).lstrip('/')} 0 0 644 capabilities=0x0")
            rules.setdefault(config, []).extend(app_rules)

        updated = signed.with_name(signed.stem + "-compiled.zip")
        with zipfile.ZipFile(updated, "w", allowZip64=True) as zout:
            for info in z.infolist():
                if info.filename in compiled:
                    continue
                data = z.read(info)
                if info.filename in rules:
                    data = add_filesystem_config(data.decode(), rules[info.filename]).encode()
                zout.writestr(info, data)
            for member, data in compiled.items():
                info = zipfile.ZipInfo(member, date_time=(2008, 1, 1, 0, 0, 0))
                info.external_attr = 0o100644 << 16
                zout.writestr(info, data)
    updated.replace(signed)
    with zipfile.ZipFile(signed) as z:
        problems = compiled_app_problems(z) + prebuilt_oat_problems(z)
    if problems:
        raise ValueError("; ".join(problems))
    print(f"compiled system apps: {sum(sizes.values()) >> 20} MiB in all", flush=True)
    shutil.rmtree(root, ignore_errors=True)
    shutil.rmtree(out, ignore_errors=True)
    return sizes


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
    cmd = [sgdisk, "--clear", f"--disk-guid={SYSTEM_DISK_GUID}"]
    number = 1
    for name, size in SYSTEM_SMALL_PARTITIONS:
        cmd += [f"--new={number}:0:+{size // 1024}K", f"--change-name={number}:{name}"]
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
    return command_line_partitions(partitions)


def data_disk(otatools, work, out):
    """Emits an empty dynamic data disk template with the DATA_PARTITIONS."""
    raw = work / "data.raw"
    make_raw_disk(raw, DISK_SIZE)
    sgdisk = shutil.which("sgdisk") or str(otatools / "bin/sgdisk")
    cmd = [sgdisk, "--clear"]
    for number, (name, size) in enumerate(DATA_PARTITIONS, start=1):
        end = f"+{size // 1024}K" if size else "0"
        cmd += [f"--new={number}:0:{end}", f"--change-name={number}:{name}"]
        if name == "misc":
            cmd += [f"--partition-guid={number}:{MISC_UUID}"]
    run(*cmd, str(raw))
    materialize_backup_gpt(raw)
    out.unlink(missing_ok=True)
    run(shutil.which("qemu-img") or "qemu-img", "convert", "-f", "raw", "-O", "vhdx", "-o", "subformat=dynamic", "-S", "4k", raw, out)
    raw.unlink()


# The serial console of a Hyper-V VM: a 16550 UART (ttyS0) on x86_64, a PL011 (ttyAMA0) on arm64.
SERIAL_CONSOLE = {"x86_64": ["console=ttyS0", "8250.nr_uarts=1"], "arm64": ["console=ttyAMA0"]}


def kernel_cmdline(partition_cmdline, hwcomposer=False, arch=DEFAULT_ARCH, camera=False):
    """Returns the kernel command line shared by every DSA guest image of `arch`. With
    `hwcomposer`, the image has DSA's composer in place of Cuttlefish's; with `camera`, DSA's
    camera HAL in place of Cuttlefish's emulated cameras, whose APEX is then not selected."""
    if hwcomposer:
        composer = [f"androidboot.dsa.{key}={value}" for key, value in DISPLAY_BOOTCONFIG.items()]
        apexes = {module: apex for module, apex in VENDOR_APEX_BOOTCONFIG.items()
                  if module != "com.android.hardware.graphics.composer"}
    else:
        composer = [
            # Cuttlefish's composer needs a virtio-gpu card or a goldfish display, and
            # Hyper-V has neither. Without DSA's composer it runs with no display device
            # and composes nothing.
            "androidboot.hardware.hwcomposer.mode=noop",
            "androidboot.hardware.hwcomposer.display_finder_mode=noop",
        ]
        apexes = VENDOR_APEX_BOOTCONFIG
    if camera:
        apexes = {module: apex for module, apex in apexes.items() if module != CAMERA_APEX_MODULE}
    return " ".join([
        # No audit=1, as on Android devices: SELinux denials reach logd's auditd without it.
        # With it, the kernel also audited every iptables change and BPF program load with
        # their syscall records, and overflowed its queue for auditd before logd started.
        SERIAL_CONSOLE[arch][0], "printk.devkmsg=on", "panic=-1", *SERIAL_CONSOLE[arch][1:],
        "firmware_class.path=/vendor/etc/", "loop.max_part=7", "init=/init", partition_cmdline,
        "androidboot.boot_devices=vmbus", "androidboot.fstab_suffix=dsa",
        "androidboot.slot_suffix=_a", "androidboot.force_normal_boot=1",
        "androidboot.hardware=cutf_cvm", "androidboot.verifiedbootstate=orange",
        "androidboot.vbmeta.device_state=unlocked",
        "androidboot.hardware.gltransport=hvsocket",
        "androidboot.hardware.egl=angle",
        "androidboot.hardware.vulkan=ranchu",
        "androidboot.hardware.gralloc=minigbm",
        *composer,
        "androidboot.cpuvulkan.version=0",
        # The Cuttlefish lights HAL listens on this vsock port for a host client; without
        # one it binds port 0, which only privileged processes may, and aborts. No host
        # client connects; DSA's own ports are 5000-5001, 5555 (adb) and 5566-5567.
        "androidboot.vsock_lights_port=6900",
        # setup_wifi makes wlan0's MAC address from this number and exits without it.
        f"androidboot.wifi_mac_prefix={WIFI_MAC_PREFIX}",
        *(f"androidboot.vendor.apex.{module}={apex}"
          for module, apex in apexes.items()),
    ])


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--major", type=int, required=True)
    parser.add_argument("--keys", type=Path, required=True)
    parser.add_argument("--inputs", type=Path, default=REPO / ".scratch/aosp-inputs")
    parser.add_argument("--out", type=Path, default=REPO / "dsa/target/image")
    parser.add_argument("--arch", choices=sorted(ARCHES), default=DEFAULT_ARCH,
                        help="the guest architecture: x86_64 (the default) or arm64, which is 64-bit only")
    parser.add_argument("--kernel", type=Path, help="the guest kernel the image boots with (bzImage on "
                        "x86_64, Image on arm64); without it the host's own kernel is used")
    parser.add_argument("--graphics-library", type=Path,
                        help="a libOpenglSystemCommon.so built for the selected guest")
    parser.add_argument("--vulkan-driver", type=Path,
                        help="a vulkan.ranchu.so built for the selected guest")
    parser.add_argument("--channeld", type=Path,
                        help="the guest's control channel daemon (dsa-channeld) built for the guest")
    parser.add_argument("--hwcomposer", type=Path,
                        help="DSA's composer HAL (dsa-hwcomposer) built for the guest; it replaces "
                             "Cuttlefish's, and sends the host each task's frames")
    parser.add_argument("--codec2", type=Path,
                        help="DSA's codec HAL (dsa-codec2) built for the guest; its video decoders "
                             "decode on the host's GPU")
    parser.add_argument("--audio", type=Path,
                        help="DSA's primary audio module (dsa-audio) built for the guest; it plays the "
                             "guest's sound on the host's default output device")
    parser.add_argument("--sensors", type=Path,
                        help="DSA's sensors HAL (dsa-sensors) built for the guest; it replaces "
                             "Cuttlefish's, and shows the host's motion and light sensors")
    parser.add_argument("--camera", type=Path,
                        help="DSA's camera HAL (dsa-camera) built for the guest; it replaces "
                             "Cuttlefish's emulated cameras with the host computer's")
    parser.add_argument("--power-hal", type=Path,
                        help="a folder holding DSA's power and health HALs (dsa-power, dsa-health) built "
                             "for the guest; they replace Cuttlefish's, take the guest's power hints to "
                             "the host and give the guest the host's battery")
    parser.add_argument("--gnss-hal", type=Path,
                        help="DSA's GNSS HAL (dsa-gnss) built for the guest; it replaces Cuttlefish's "
                             "and gives the guest the host computer's location")
    parser.add_argument("--agent", type=Path,
                        help="the guest agent's APK (DsaAgent), unsigned or signed with any key; "
                        "the image signs it with the platform key")
    parser.add_argument("--aapt2", type=Path,
                        help="aapt2 from the Android SDK's build-tools, which builds the framework overlay; "
                             "found under ANDROID_HOME when not given, and without it the image has no overlay")
    parser.add_argument("--graphics-dir", type=Path,
                        help="a directory containing guest graphics libraries (vulkan.ranchu.so, libOpenglSystemCommon.so)")
    parser.add_argument("--arm-translation", type=Path,
                        help="an ARM translation package folder (arm-translation.json and system.img) that "
                             "an x86_64 image carries as its default translator, and releases beside it")
    parser.add_argument("--framework", type=Path,
                        help="a folder from the Framework build (framework.json and the files it names): "
                             "SurfaceFlinger, libhwui and libEGL that recover from a GPU device loss, built "
                             "from the image's own build for its architecture")
    args = parser.parse_args()
    if args.arm_translation:
        checked_arm_translation(args.arm_translation, args.arch)

    spec = image_spec(json.loads((HERE / "inputs.json").read_text())["images"], args.major, args.arch)
    use_arch(args.arch)
    framework_payloads = framework_description = None
    if args.framework:
        framework_payloads, framework_description = checked_framework(
            args.framework, spec["build_id"], spec["android_major"], args.arch)
    inputs = args.inputs / f"{spec['build_id']}-{args.arch}"
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

    graphics_library = args.graphics_library
    vulkan_driver = args.vulkan_driver
    if args.graphics_dir:
        if not graphics_library and (args.graphics_dir / "libOpenglSystemCommon.so").exists():
            graphics_library = args.graphics_dir / "libOpenglSystemCommon.so"
        if not vulkan_driver and (args.graphics_dir / "vulkan.ranchu.so").exists():
            vulkan_driver = args.graphics_dir / "vulkan.ranchu.so"
    elif graphics_library and not vulkan_driver:
        sibling = graphics_library.parent / "vulkan.ranchu.so"
        if sibling.exists():
            vulkan_driver = sibling

    framework_overlay = None
    settings_overlay = None
    aapt2 = args.aapt2 or find_aapt2()
    if aapt2:
        framework_res = work / "framework-res.apk"
        with zipfile.ZipFile(target_files) as z:
            framework_res.write_bytes(z.read("SYSTEM/framework/framework-res.apk"))
            home_padding = SETTINGS_APP in z.namelist() and settings_has_home_padding(z.read(SETTINGS_APP))
        framework_overlay = build_framework_overlay(aapt2, framework_res, work)
        settings_overlay = build_settings_overlay(aapt2, framework_res, work, home_padding)
    else:
        print("no aapt2, so the image has neither the framework overlay nor the Settings overlay", flush=True)

    modified = work / "modified.zip"
    modify(target_files, modified, graphics_library=graphics_library, vulkan_driver=vulkan_driver,
           otatools=otatools, channeld=args.channeld, agent=args.agent, hwcomposer=args.hwcomposer,
           framework_overlay=framework_overlay, settings_overlay=settings_overlay, power_hal=args.power_hal,
           codec2=args.codec2, audio=args.audio, gnss_hal=args.gnss_hal,
           sensors=args.sensors, camera=args.camera, arm_translation=args.arm_translation,
           framework=framework_payloads)

    signed = work / "signed.zip"
    # It runs where AOSP's certificates sit at the paths apkcerts.txt names, so every path
    # passed to it is absolute.
    aosp_root = aosp_test_certificates(otatools, work / "aosp-certificates", modified)
    run(otatools.resolve() / "bin/sign_target_files_apks", "-p", otatools.resolve(),
        *key_arguments(modified, str(Path(keys).resolve())), modified.resolve(), signed.resolve(),
        env=env, cwd=aosp_root)
    modified.unlink()
    # Signing packs the permission APEX once more as it signs the apps in it; what it packed
    # must still carry the edit.
    problems = permission_controller_problems(signed, otatools, work, env)
    if problems:
        raise ValueError("; ".join(problems))
    compile_apps(signed, inputs / spec["files"]["dexpreopt_tools"]["name"], otatools, work, env)
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
    kernel = kernel_name(args.arch)
    for name in {a["kernel"] for a in ARCHES.values()}:
        (out / name).unlink(missing_ok=True)
    if args.kernel:
        with open(args.kernel, "rb") as f:
            found = kernel_format(f.read(0x1000))
        if found != args.arch:
            raise SystemExit(f"{args.kernel} is not a kernel for {args.arch} (its header says {found or 'neither'})")
        shutil.copy(args.kernel, out / kernel)
        files.append(kernel)

    cmdline = kernel_cmdline(partition_cmdline, hwcomposer=args.hwcomposer is not None, arch=args.arch,
                             camera=args.camera is not None)
    manifest = {
        "version": spec["version"],
        "android_major": spec["android_major"],
        "arch": args.arch,
        # Orders images of one Android version: a new AOSP build, or a new revision of
        # DSA's image of the same build (a fix to how the image is made), is newer.
        "build": int(spec["build_id"]) * 100 + spec.get("revision", 1),
        "source": {"branch": spec["branch"], "target": spec["target"], "arch": args.arch, "build_id": spec["build_id"], "revision": spec.get("revision", 1), "security_patch": spec["security_patch"]},
        "kernel_cmdline": cmdline,
        # What the host needs to compose a fingerprint from a device identity the user chose.
        "identity": image_identity(zipfile.ZipFile(signed).read("SYSTEM/build.prop").decode()),
        "files": {name: {"size": (out / name).stat().st_size, "sha256": sha256(out / name)} for name in files},
    }
    (out / ARM_TRANSLATION_ZIP).unlink(missing_ok=True)
    if args.arm_translation:
        manifest["arm_translation"] = default_arm_translation(args.arm_translation, args.arch, out)
    if framework_description is not None:
        manifest["framework"] = framework_entry(framework_description)
    (out / "image.json").write_text(json.dumps(manifest, indent=2) + "\n")
    shutil.copy(signed, out / "signed-target_files.zip")
    shutil.rmtree(work)
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    sys.exit(main())
