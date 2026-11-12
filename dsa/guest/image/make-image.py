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
   dex no longer matches its odex (PREBUILT_OAT_RECORD), and services.jar, whose window
   manager it edits so a freeform window gets its status bar inset (SERVICES_CODE);
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
image.json describes them under "framework". When the framework build has DSA's KeyMint
(KEYMINT_FILES), whose root key of key blobs comes from the host, the vendor partition carries it
in place of the KeyMint APEX, which the image then does not select.
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
# (it marks the device set up), Telecom (system_server binds it at boot, and calling apps place
# their own calls through it), CarrierConfig, the browser, the camera and the clock. The browser
# stays although web links go to the host's browser by default (the agent holds the browser
# role then): an app may start it by name, the user can choose to keep links in Android, which
# gives the role back to it, and a link that cannot reach the host opens in it.
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
    # TeleService, the com.android.phone process: persistent, so the activity manager starts it
    # at boot and again whenever it ends, and it held about 8.5 MiB. Without the telephony
    # feature (unavailable-features.xml) it sets nothing up (PhoneGlobals returns at once), so
    # it publishes none of the phone, subscription or carrier config services and system_server
    # already runs without them, as an Android TV build, which has no TeleService, does.
    # Telecom stays for apps that place their own calls. The telephony provider, which runs
    # under the same name, stays and starts only when an app asks it something.
    "TeleService",
    # The opportunistic network service (com.android.ons) chooses between the SIMs of a
    # modem, which this VM does not have. It goes with TeleService: it shares the phone's
    # user ID and process, is persistent and starts at boot, and the network permissions it
    # uses are declared only by TeleService. Left alone it crashes on its first network
    # callback, the activity manager restarts it at once, and after a few crashes the rescue
    # party reboots Android within a minute of every boot.
    "ONS",
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
    # A text-to-speech engine of the guest's own, when a build has one: the agent's engine
    # speaks with the host's voices and is the default, and an engine here would hold its
    # voices in the guest's memory.
    "PicoTts",
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
] + [
    # Cuttlefish's example HALs for devices a desktop VM does not have. Each ran as a process
    # of its own from boot (about 1 MiB of PSS, plus its page tables, kernel stacks and binder
    # threads in the guest's kernel) and answered with fixed values or nothing. Each is
    # optional to the framework, which runs without it as a device without that hardware does:
    # - authsecret and rebootescrow: secrets for a secure element and for resuming an update
    #   after a reboot; the user's credential and the host's image updates do without them.
    # - cas: conditional access for broadcast television tuners.
    # - contexthub: a phone's sensor hub; its feature goes too (unavailable-features.xml).
    # - input.processor: a model that classifies touch input; input then goes unclassified,
    #   as on every device without one.
    # - ir (com.google.cf.ir): an infrared blaster; its feature goes too, because the
    #   consumer IR service refuses to start with the feature but no HAL.
    # - memtrack: per-process GPU memory for dumpsys; the example reported none anyway.
    # - net.nlinterceptor and tetheroffload: a modem's netlink filter and tethering offloaded
    #   to a modem.
    # - neuralnetworks: three sample NNAPI drivers on the CPU; the NNAPI runtime falls back to
    #   its own CPU path, which they only imitated.
    # - secure_element: the HAL behind the SecureElement app, which is removed above.
    # - usb: a phone's USB-C port roles; USB devices the host lends need no port HAL.
    # - vibrator: a phone's vibration motor; Android then reports no vibrator.
    "com.android.hardware.authsecret",
    "com.android.hardware.rebootescrow",
    "com.android.hardware.cas",
    "com.android.hardware.contexthub",
    "com.android.hardware.input.processor",
    "com.google.cf.ir",
    "com.android.hardware.memtrack",
    "com.android.hardware.net.nlinterceptor",
    "com.android.hardware.tetheroffload",
    "com.android.hardware.neuralnetworks",
    "com.android.hardware.secure_element",
    "com.android.hardware.usb",
    "com.android.hardware.vibrator",
]

# The virtual face and fingerprint HALs, installed as plain vendor files. They fake a sensor
# no PC has, so Settings offered face and fingerprint unlock that nothing could complete,
# and system_server started both HALs for it. With them go the features that make
# system_server start the biometric services (unavailable-features.xml as well, in case
# another permissions file declares them).
REMOVED_VENDOR_FILES = [
    "VENDOR/bin/hw/android.hardware.biometrics.face-service.default",
    "VENDOR/bin/hw/android.hardware.biometrics.fingerprint-service.default",
    "VENDOR/etc/init/face-default.rc",
    "VENDOR/etc/init/fingerprint-default.rc",
    "VENDOR/etc/vintf/manifest/face-default.xml",
    "VENDOR/etc/vintf/manifest/fingerprint-default.xml",
    "VENDOR/etc/permissions/android.hardware.biometrics.face.xml",
    "VENDOR/etc/permissions/android.hardware.fingerprint.xml",
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

# The host window's title bar is Android's status bar. A task's window draws its caption over
# the task's top band, and the agent gives the task status bar and caption bar inset sources
# as high as that band. An app that draws edge to edge then lays out below the band as it lays
# out below a phone's status bar, and draws its own status bar background under the caption;
# an app that does not gets the status bar background Android draws for it there.
#
# Android hands a window in a floating windowing mode (freeform, as every DSA task is, or
# picture-in-picture) only its caption bar and IME insets: InsetsPolicy's
# enforceInsetsPolicyForTarget keeps WindowInsets.Type.captionBar() (and the IME) and drops
# the rest. The status bar source the agent gives the task never reached its app, so an app
# that pads its toolbar by its status bar inset, as most edge-to-edge apps do (FLO's player
# screen, for one), got 0 and drew its toolbar under the caption, its buttons under the
# window's close button. The call that names the kept types names systemBars() instead
# (status bars, navigation bars and the caption bar), so the app receives the status bar
# source too, and with it the inset a phone gives it. The guest has no navigation bar and no
# system UI of its own, so the only system bar sources are the ones the agent gives tasks.
# Only the first call after isFloating changes: Android 16 and 17 have no other caption bar
# call before it, and a later Android that tests a source's type against captionBar() before
# it keeps that test.
#
# services.jar is compiled ahead of time (services.odex, .vdex and .art beside it), for its
# dex: the edited jar is compiled again with the published build's filter, class loader
# context and profile (SERVICES_PROFILE), as compile_apps compiles an edited app.
SERVICES_JAR = "SYSTEM/framework/services.jar"
SERVICES_CODE = {
    "Lcom/android/server/wm/InsetsPolicy;->enforceInsetsPolicyForTarget": ("replace call", {
        "after": "Landroid/app/WindowConfiguration;->isFloating",
        "old": "Landroid/view/WindowInsets$Type;->captionBar",
        "new": "Landroid/view/WindowInsets$Type;->systemBars",
    }),
}
# The jars whose dex make-image.py edits, compiled again after the edit like EDITED_APPS.
EDITED_JARS = [SERVICES_JAR]
# The profile services.jar was compiled with: frameworks/base's services/art-profile and
# art-profile-extra, one after the other (the build's art-profile-combined), at the commit the
# published build was made from. inputs.json names them for each build (services_profile and
# services_profile_extra), and fetch.sh downloads them beside the build's other inputs.
# profman turns them into the binary profile dex2oat reads; should it fail, services.jar is
# compiled with speed, which compiles all of what speed-profile compiles.
SERVICES_PROFILE_INPUT = "services_profile"

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
# ART keeps less free space in each process's heap: the heap is 75% full rather than 60%
# after a collection, as on a 1 GiB phone, with at most 4 MiB free, half a 1 GiB phone's 8 and
# twice a 512 MiB Android Go phone's 2. Cuttlefish's values are for a 4 GiB phone that has
# nothing else to do with its memory. The free space matters in the processes with the largest
# heaps, which are the ones that stay: system_server (its heap holds the state of every
# service), Google Play services and the app on screen each grow by up to that much between
# two collections, and none of them is in the background, where ART would trim it. The cost
# is a collection twice as often in those, which ART does concurrently with pauses well under
# a millisecond, on a desktop's processor. The limits on how far a heap may grow stay as they
# are, so no app has less memory to work with.
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
    "dalvik.vm.heapmaxfree": "4m",
}

# The density of the freeform display every task window lives on, by default: a 100% monitor's.
# The host replaces it on the kernel command line at each boot with the density of the user's
# main monitor (196 at 150%: Android's desktop dp is 1/117.23 inch and a host pixel at 100%
# 1/96 inch, so the density is 131 times the scale, rounded to an even number), the density a
# task window there asks its task to draw at. The
# hwcomposer reports it (ro.boot.dsa.display_dpi) and SurfaceFlinger takes it from
# ro.sf.lcd_density alone, which dsa-display.rc sets from androidboot.lcd_density. Without
# ro.sf.lcd_density SurfaceFlinger falls back to 213 dpi ("ro.sf.lcd_density must be defined as
# a build property"): every app then started at 213 and the host's density a moment later, a
# density change, which relaunches every activity that does not handle one itself. hello-vulkan
# was destroyed before its first window that way and crashed in its own clean-up. No build.prop
# may set it: init reads them before any script, and the first value of a read-only property
# stays, so a build.prop value would hold every boot at this default.
DISPLAY_DPI = 130

# The OpenGL ES version Android reports to apps and to app stores: 3.2 (0x30002), which ANGLE
# provides on the host GPU's Vulkan for every app. PackageManager reports it as reqGlEsVersion,
# and Google Play hides every app whose manifest asks for a version above it (<uses-feature
# android:glEsVersion>). Cuttlefish leaves it to its launcher, which passes
# androidboot.opengles.version for init_graphics.vendor.rc to copy; DSA does not run that
# launcher, so without this the property stayed empty, reqGlEsVersion was 0, and Play called
# YouTube incompatible and left YouTube and Chrome out of its search.
OPENGLES_VERSION = 0x30002

GRAPHICS_PROPS = {
    "ro.hardware.egl": "angle",
    "ro.hardware.vulkan": "ranchu",
    "ro.hardware.gralloc": "minigbm",
    "ro.cpuvulkan.version": "0",
    "ro.opengles.version": str(OPENGLES_VERSION),
}

# Properties no build.prop may set, whatever the input build's files say: the boot sets them.
BOOT_SET_PROPS = {"ro.sf.lcd_density"}

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
        or name in REMOVED_VENDOR_FILES
        or any(f"/{app}/" in name for app in REMOVED_APPS)
        or any(
            name in (f"VENDOR/apex/{apex}.apex", f"VENDOR/apex/{apex}.capex")
            for apex in removed_apexes(hwcomposer, power, gnss, sensors, camera)
        )
        or (gnss and cuttlefish_gnss_file(name))
        or (sensors and cuttlefish_sensors_file(name))
    )


def cuttlefish_gnss_file(name):
    """Whether a target_files member is part of a GNSS HAL installed as plain files (its
    binary, init script or VINTF fragment), other than DSA's own."""
    if name in GNSS_FILES:
        return False
    return any(name.startswith(folder) and "gnss" in name[len(folder):].lower()
               for folder in CUTTLEFISH_GNSS_FOLDERS)


def cuttlefish_sensors_file(name):
    """Whether a target_files member is part of a sensors HAL installed as plain files (its
    service, init script, sub-HAL library or sub-HAL list), other than DSA's own. Android 17's
    Cuttlefish has its multi-HAL that way, not in an APEX. Left in the image, init starts it,
    its Cuttlefish sub-HAL finds no sensors control socket to connect to, and it aborts every
    five seconds, filling the log and the tombstones."""
    if name in SENSORS_FILES:
        return False
    if name.startswith(CUTTLEFISH_SENSORS_CONFIG) and name != CUTTLEFISH_SENSORS_CONFIG:
        return True
    return any(name.startswith(folder) and "sensors" in name[len(folder):].lower()
               for folder in CUTTLEFISH_SENSORS_FOLDERS)


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
    # It serves the remotely provisioned component, the secure clock and the shared secret too,
    # which the same fragments declare.
    "VENDOR/bin/hw/dsa-keymint": ("aidl", "android.hardware.security.keymint", "IKeyMintDevice", "default"),
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


XML_HEADER = '<?xml version="1.0" encoding="utf-8"?>\n'
ANDROID_XMLNS = 'xmlns:android="http://schemas.android.com/apk/res/android"'
APP_XMLNS = 'xmlns:app="http://schemas.android.com/apk/res-auto"'


def overlay_manifest(package, target, resource_map=False, priority=100, required_property=None):
    """The manifest of a static overlay of `target` with no code; with `resource_map`, its
    resource map (SYSTEM_OVERLAY_MAP) names what it replaces, and only that. With
    `required_property` (a name and a value), the package manager applies the overlay only
    while that read-only property has that value when it scans the packages at boot."""
    mapping = f'\n        android:resourcesMap="@xml/{SYSTEM_OVERLAY_MAP}"' if resource_map else ""
    if required_property:
        name, value = required_property
        mapping += (f'\n        android:requiredSystemPropertyName="{name}"'
                    f'\n        android:requiredSystemPropertyValue="{value}"')
    return (
        XML_HEADER
        + f'<manifest {ANDROID_XMLNS}\n'
        f'    package="{package}">\n'
        # Android 14 and later refuse a package that targets an old SDK, and the default is 1.
        '    <uses-sdk android:minSdkVersion="34" android:targetSdkVersion="35" />\n'
        '    <application android:hasCode="false" />\n'
        f'    <overlay android:targetPackage="{target}" android:isStatic="true" android:priority="{priority}"{mapping} />\n'
        '</manifest>\n'
    )


def overlay_apkcerts(apk):
    """The apkcerts.txt line that has signing give an overlay in /vendor/overlay the platform key."""
    return (f'name="{apk}" certificate="build/make/target/product/security/platform.x509.pem" '
            'private_key="build/make/target/product/security/platform.pk8" partition="vendor"')


def overlay_fs_config(apk):
    """The vendor filesystem config lines of an overlay in /vendor/overlay."""
    return ["vendor/overlay 0 0 755 capabilities=0x0", f"vendor/overlay/{apk} 0 0 644 capabilities=0x0"]


def resources_xml(lines):
    """A values file of the given lines."""
    return XML_HEADER + "<resources>\n" + "".join(f"    {line}\n" for line in lines) + "</resources>\n"


def overlay_map(mapping):
    """The resource map of an overlay: each target resource ("type/name") and the overlay's
    resource that replaces it."""
    items = "".join(f'    <item target="{target}" value="@{value}" />\n' for target, value in mapping.items())
    return XML_HEADER + f"<overlay>\n{items}</overlay>\n"


def write_overlay_files(dest, files):
    """Writes each of `files` ({path under dest: text}) into `dest`."""
    for path, text in files.items():
        (dest / path).parent.mkdir(parents=True, exist_ok=True)
        (dest / path).write_text(text)


def write_overlay_record(dest, record):
    """Writes what an overlay was built against (SYSTEM_OVERLAY_RECORD), when `record` says."""
    if record is not None:
        write_overlay_files(dest, {SYSTEM_OVERLAY_RECORD: json.dumps(record, indent=2, sort_keys=True) + "\n"})


def build_overlay(aapt2, framework_res, sources, apk):
    """Compiles and links the overlay whose sources are in `sources` against `framework_res`,
    with its record as an asset when it has one. The APK is unsigned; signing gives it the
    platform key."""
    compiled = sources / "compiled.zip"
    run(aapt2, "compile", "--dir", sources / "res", "-o", compiled)
    assets = ["-A", sources / "assets"] if (sources / "assets").is_dir() else []
    run(aapt2, "link", "-o", apk, "--manifest", sources / "AndroidManifest.xml", "-I", framework_res,
        *assets, compiled)
    return apk


def design_style(name, parent, size=None, color=None, extra=()):
    """A text style of the design: Roboto at its bold weight, with `parent`'s other items
    (no parent when `parent` is empty), and the size and colour given."""
    items = [f'<item name="android:fontFamily">{DESIGN_FONT}</item>',
             '<item name="android:textStyle">bold</item>', *extra]
    if size is not None:
        items.append(f'<item name="android:textSize">{size}</item>')
    if color is not None:
        items.append(f'<item name="android:textColor">{color}</item>')
    body = "".join(f"\n        {item}" for item in items)
    return f'<style name="{name}" parent="{parent}">{body}\n    </style>'


def dialog_button_bar():
    """The framework's dialog button bar, with its buttons side by side, sharing its width.

    The ids are the framework's, which AlertController finds the buttons by. ButtonBarLayout
    stacks the buttons when they do not fit, as it did; wrap_content keeps a stacked button
    as wide as its text."""
    def button(number, role):
        return (f'        <Button\n'
                f'            android:id="@android:id/button{number}"\n'
                f'            style="?android:attr/buttonBar{role}ButtonStyle"\n'
                '            android:layout_width="wrap_content"\n'
                '            android:layout_height="wrap_content"\n'
                '            android:layout_weight="1" />\n')
    return (
        XML_HEADER
        + f'<ScrollView {ANDROID_XMLNS}\n'
        '    android:id="@*android:id/buttonPanel"\n'
        '    android:layout_width="match_parent"\n'
        '    android:layout_height="wrap_content"\n'
        '    android:scrollbarAlwaysDrawVerticalTrack="true"\n'
        '    android:scrollIndicators="top|bottom"\n'
        '    android:fillViewport="true"\n'
        '    style="?android:attr/buttonBarStyle">\n'
        '    <com.android.internal.widget.ButtonBarLayout\n'
        '        android:layout_width="match_parent"\n'
        '        android:layout_height="wrap_content"\n'
        '        android:layoutDirection="locale"\n'
        '        android:orientation="horizontal"\n'
        '        android:paddingStart="16dp"\n'
        '        android:paddingEnd="16dp"\n'
        '        android:paddingTop="4dp"\n'
        '        android:paddingBottom="12dp"\n'
        '        android:gravity="bottom">\n'
        + button(3, "Neutral")
        + '        <Space\n'
        '            android:id="@*android:id/spacer"\n'
        '            android:layout_width="0dp"\n'
        '            android:layout_height="0dp"\n'
        '            android:visibility="invisible" />\n'
        + button(2, "Negative")
        + button(1, "Positive")
        + '    </com.android.internal.widget.ButtonBarLayout>\n'
        '</ScrollView>\n'
    )


def toast_layout():
    """The toast an app draws itself: the framework's layout on the rounded toast frame."""
    return (
        XML_HEADER
        + f'<LinearLayout {ANDROID_XMLNS}\n'
        '    android:layout_width="wrap_content"\n'
        '    android:layout_height="wrap_content"\n'
        '    android:orientation="horizontal"\n'
        '    android:gravity="center_vertical"\n'
        '    android:maxWidth="@*android:dimen/toast_width"\n'
        f'    android:background="@*android:drawable/{TOAST_FRAME}"\n'
        '    android:elevation="@*android:dimen/toast_elevation"\n'
        '    android:layout_marginEnd="16dp"\n'
        '    android:layout_marginStart="16dp"\n'
        '    android:paddingStart="20dp"\n'
        '    android:paddingEnd="20dp">\n'
        '    <TextView\n'
        '        android:id="@android:id/message"\n'
        '        android:layout_width="wrap_content"\n'
        '        android:layout_height="wrap_content"\n'
        '        android:ellipsize="end"\n'
        '        android:maxLines="2"\n'
        '        android:paddingTop="12dp"\n'
        '        android:paddingBottom="12dp"\n'
        '        android:textAppearance="@*android:style/TextAppearance.Toast" />\n'
        '</LinearLayout>\n'
    )


def toast_frame():
    """The toast's frame: a pill on the theme's background. The framework's own frame takes
    the theme's colorSurface, which is private to the framework, so an overlay cannot name it;
    every DeviceDefault theme gives colorBackground the same colour."""
    return (
        XML_HEADER
        + f'<shape {ANDROID_XMLNS}\n'
        '    android:shape="rectangle">\n'
        '    <solid android:color="?android:attr/colorBackground" />\n'
        f'    <corners android:radius="{DESIGN_CORNER_DP}dp" />\n'
        '</shape>\n'
    )


def framework_overlay_sources(dest, record=None):
    """Writes the framework overlay's manifest, values and layouts into `dest`, with what it
    was built against when `record` says."""
    (dest / "res/values").mkdir(parents=True, exist_ok=True)
    (dest / "AndroidManifest.xml").write_text(overlay_manifest(FRAMEWORK_OVERLAY_PACKAGE, "android"))
    integers = "".join(f'    <integer name="{name}">{value}</integer>\n'
                       for name, value in FRAMEWORK_OVERLAY_INTEGERS.items())
    booleans = "".join(f'    <bool name="{name}">{str(value).lower()}</bool>\n'
                       for name, value in FRAMEWORK_OVERLAY_BOOLEANS.items())
    strings = "".join(f'    <string name="{name}" translatable="false">{value}</string>\n'
                      for name, value in FRAMEWORK_OVERLAY_STRINGS.items())
    (dest / "res/values/dsa.xml").write_text(
        f'<?xml version="1.0" encoding="utf-8"?>\n<resources>\n{integers}{booleans}{strings}</resources>\n')
    write_overlay_files(dest, {
        "res/values/dsa_design.xml": resources_xml(
            [f'<color name="{name}">{value}</color>' for name, value in DESIGN_COLORS.items()]
            + [f'<dimen name="{name}">{value}</dimen>' for name, value in DESIGN_FRAMEWORK_DIMENS.items()]
            + [design_style(name, f"@*android:style/{parent}") for name, parent in DESIGN_FRAMEWORK_TITLES.items()]),
        f"res/layout/{DIALOG_BUTTON_BAR_LAYOUT}.xml": dialog_button_bar(),
        f"res/layout/{TOAST_LAYOUT}.xml": toast_layout(),
        f"res/drawable/{TOAST_FRAME}.xml": toast_frame(),
        **status_bar_overlay_files(),
    })
    write_overlay_record(dest, record)


def framework_overlaid():
    """Every framework resource the framework overlay replaces, by name: it has no resource
    map, so the framework matches its resources to its own by name."""
    return ([f"integer/{name}" for name in FRAMEWORK_OVERLAY_INTEGERS]
            + [f"bool/{name}" for name in FRAMEWORK_OVERLAY_BOOLEANS]
            + [f"string/{name}" for name in FRAMEWORK_OVERLAY_STRINGS]
            + [f"dimen/{name}" for name in STATUS_BAR_DIMENS]
            + FRAMEWORK_DESIGN_OVERLAID)


def build_framework_overlay(aapt2, framework_res, work, record=None):
    """Builds the framework overlay APK (unsigned; signing gives it the platform key)."""
    sources = work / "framework-overlay"
    shutil.rmtree(sources, ignore_errors=True)
    framework_overlay_sources(sources, record)
    return build_overlay(aapt2, framework_res, sources, sources / "DsaFrameworkOverlay.apk")


def settings_overlay_sources(dest, home_padding=False, record=None):
    """Writes the Settings overlay's manifest, its resource map, its back button drawables,
    its preference list layout and the design's resources (cards, large titles, the home
    page) into `dest`, with `home_padding` the home list's bottom padding
    (SETTINGS_HOME_PADDING_DIMEN), for a Settings that has it, and what it was built against
    when `record` says."""
    write_overlay_files(dest, {
        **settings_overlay_files(home_padding),
        f"res/xml/{SETTINGS_OVERLAY_MAP}.xml": overlay_map(settings_overlay_map(home_padding)),
    })
    write_overlay_record(dest, record)


def settings_overlay_map(home_padding=False):
    """What the Settings overlay replaces ("type/name" in Settings) and with which of its own
    resources. The ids and attributes map onto themselves: nothing reads their values, and
    the map makes the framework rewrite the overlay layouts' references to them into
    Settings' ids."""
    mapping = {name: name for name in SETTINGS_OVERLAID}
    if home_padding:
        mapping[f"dimen/{SETTINGS_HOME_PADDING_DIMEN}"] = f"dimen/{SETTINGS_HOME_PADDING_DIMEN}"
    mapping.update({f"drawable/{name}": f"drawable/{name}" for name in SETTINGS_CARDS})
    mapping["dimen/settingslib_preference_corner_radius"] = "dimen/settingslib_preference_corner_radius"
    mapping[f"style/{SETTINGS_TITLE_THEME}"] = "style/DsaAppBarTheme"
    mapping[f"style/{SETTINGS_TITLE_THEME_PARENT}"] = f"style/{SETTINGS_TITLE_THEME_PARENT}"
    mapping["attr/expanded"] = "attr/expanded"
    mapping.update({f"style/{name}": f"style/{ours}" for name, (ours, _) in SETTINGS_TITLE_STYLES.items()})
    mapping[f"layout/{SETTINGS_HOME_LAYOUT}"] = f"layout/{SETTINGS_HOME_LAYOUT}"
    mapping.update({f"id/{name}": f"id/{name}" for name in SETTINGS_HOME_IDS})
    mapping.update({f"attr/{name}": f"attr/{name}" for name in SETTINGS_HOME_ATTRS})
    return mapping


def settings_overlay_files(home_padding=False):
    """The Settings overlay's manifest and resource files, by their path in its sources.

    The drawables and values SettingsLib keeps in -v36 folders are here in the same folders:
    an overlay's value is used only where its configuration matches at least as well as the
    one it replaces."""
    files = {
        "AndroidManifest.xml": overlay_manifest(SETTINGS_OVERLAY_PACKAGE, SETTINGS_OVERLAY_TARGET,
                                                resource_map=True),
        f"res/layout/{SETTINGS_LIST_LAYOUT}.xml": settings_preference_list(),
        "res/values/dsa.xml": resources_xml(
            [f'<attr name="{SETTINGS_LIST_STYLE_ATTR}" format="reference" />', *SETTINGS_HOME_ATTRS.values(),
             '<attr name="expanded" format="boolean" />']
            + ([f'<dimen name="{SETTINGS_HOME_PADDING_DIMEN}">{SETTINGS_LIST_BOTTOM_MARGIN_DP}dp</dimen>']
               if home_padding else [])),
        "res/values-v36/dsa_design.xml": resources_xml(
            [f'<dimen name="settingslib_preference_corner_radius">{DESIGN_CORNER_DP}dp</dimen>']
            + [design_style(ours, "", f"{size}dp") for ours, size in SETTINGS_TITLE_STYLES.values()]),
        f"res/layout/{SETTINGS_HOME_LAYOUT}.xml": settings_home_layout(large_title=False),
        f"res/layout-h{DESIGN_TALL_WINDOW_DP}dp/{SETTINGS_HOME_LAYOUT}.xml": settings_home_layout(large_title=True),
        f"res/drawable/{SETTINGS_SEARCH_ICON}.xml": search_icon(),
    }
    for folder, expanded in SETTINGS_TITLE_EXPANDS.items():
        files[f"res/{folder}/dsa_title.xml"] = resources_xml([
            f'<style name="DsaAppBarTheme" parent="@style/{SETTINGS_TITLE_THEME_PARENT}">'
            f'<item name="expanded">{str(expanded).lower()}</item></style>'])
    # The stand-in for Material's theme: only for watches, so Settings' own is always used.
    files["res/values-watch/dsa_stand_ins.xml"] = resources_xml(
        [f'<style name="{SETTINGS_TITLE_THEME_PARENT}" parent="" />'])
    for folder, theme in (("drawable-v36", "light"), ("drawable-night-v36", "dark")):
        files[f"res/{folder}/{SETTINGS_BACK_ICON}.xml"] = settings_back_icon(theme)
        for name, (top, bottom) in SETTINGS_CARDS.items():
            files[f"res/{folder}/{name}.xml"] = settings_card(top, bottom, theme)
    for folder, theme in (("drawable", "light"), ("drawable-night", "dark")):
        files[f"res/{folder}/{SETTINGS_SEARCH_ICON}_background.xml"] = search_background(theme)
    return files


def settings_card(top, bottom, theme):
    """One of SettingsLib's group backgrounds: the row's surface, inset from the list's sides
    as SettingsLib insets it, with the group's outer corners rounded (`top`, `bottom`) and
    square corners where the next row of the same group joins it. The 32 dp padding is
    SettingsLib's own, which its adapter gives the row."""
    top_dp, bottom_dp = (DESIGN_CORNER_DP if top else 0), (DESIGN_CORNER_DP if bottom else 0)
    return (
        XML_HEADER
        + f'<ripple {ANDROID_XMLNS}\n'
        '    android:color="?android:attr/colorControlHighlight"\n'
        '    android:paddingStart="32dp"\n'
        '    android:paddingEnd="32dp">\n'
        '    <item\n'
        '        android:start="?android:attr/listPreferredItemPaddingStart"\n'
        '        android:end="?android:attr/listPreferredItemPaddingEnd">\n'
        '        <shape android:shape="rectangle">\n'
        f'            <solid android:color="@*android:color/system_surface_bright_{theme}" />\n'
        '            <corners\n'
        f'                android:topLeftRadius="{top_dp}dp"\n'
        f'                android:topRightRadius="{top_dp}dp"\n'
        f'                android:bottomLeftRadius="{bottom_dp}dp"\n'
        f'                android:bottomRightRadius="{bottom_dp}dp" />\n'
        '        </shape>\n'
        '    </item>\n'
        '</ripple>\n'
    )


def search_icon():
    """A magnifying glass, drawn for DSA: a ring and its handle."""
    return (
        XML_HEADER
        + f'<vector {ANDROID_XMLNS}\n'
        '    android:width="24dp"\n'
        '    android:height="24dp"\n'
        '    android:viewportWidth="24"\n'
        '    android:viewportHeight="24"\n'
        '    android:tint="?android:attr/textColorSecondary">\n'
        '    <path\n'
        '        android:strokeColor="@android:color/white"\n'
        '        android:strokeWidth="2"\n'
        '        android:strokeLineCap="round"\n'
        '        android:pathData="M10,4A6,6 0,1 1,10,16A6,6 0,1 1,10,4Z M14.5,14.5L20,20" />\n'
        '</vector>\n'
    )


def search_background(theme):
    """The home page's search bar: a pill on the cards' surface."""
    return (
        XML_HEADER
        + f'<shape {ANDROID_XMLNS}\n'
        '    android:shape="rectangle">\n'
        f'    <solid android:color="@*android:color/system_surface_bright_{theme}" />\n'
        f'    <corners android:radius="{DESIGN_CORNER_DP}dp" />\n'
        '</shape>\n'
    )


def settings_home_layout(large_title):
    """Settings' home page: Settings' own layout of its lists, under an app bar with the
    search bar, and with `large_title` the large title above the search bar.

    The title scrolls away with the list (scroll|snap) and leaves the search bar as the
    toolbar. The home page's code sets the search bar's click and the container's padding by
    their ids, which are Settings' own; the text is the framework's, in every language."""
    title = (
        '        <TextView\n'
        '            android:layout_width="match_parent"\n'
        f'            android:layout_height="{DESIGN_TITLE_AREA_DP}dp"\n'
        '            android:gravity="center"\n'
        '            android:paddingStart="?android:attr/listPreferredItemPaddingStart"\n'
        '            android:paddingEnd="?android:attr/listPreferredItemPaddingEnd"\n'
        '            android:maxLines="2"\n'
        '            android:ellipsize="end"\n'
        '            android:text="@*android:string/global_action_settings"\n'
        f'            android:textSize="{DESIGN_LARGE_TITLE_DP}dp"\n'
        f'            android:fontFamily="{DESIGN_FONT}"\n'
        '            android:textStyle="bold"\n'
        '            android:textColor="?android:attr/textColorPrimary"\n'
        '            android:accessibilityHeading="true"\n'
        '            app:layout_scrollFlags="scroll|snap" />\n'
    ) if large_title else ""
    return (
        XML_HEADER
        + '<androidx.coordinatorlayout.widget.CoordinatorLayout\n'
        f'    {ANDROID_XMLNS}\n'
        f'    {APP_XMLNS}\n'
        '    android:id="@+id/settings_homepage_container"\n'
        '    android:layout_width="match_parent"\n'
        '    android:layout_height="match_parent">\n'
        '    <androidx.core.widget.NestedScrollView\n'
        '        android:id="@+id/main_content_scrollable_container"\n'
        '        android:layout_width="match_parent"\n'
        '        android:layout_height="match_parent"\n'
        '        app:layout_behavior="com.android.settings.widget.HomepageAppBarScrollingViewBehavior">\n'
        '        <LinearLayout\n'
        '            android:id="@+id/homepage_container"\n'
        '            android:layout_width="match_parent"\n'
        '            android:layout_height="wrap_content"\n'
        '            android:layout_marginTop="-8dp"\n'
        '            android:orientation="vertical">\n'
        '            <FrameLayout\n'
        '                android:id="@+id/suggestion_content"\n'
        '                android:layout_width="match_parent"\n'
        '                android:layout_height="wrap_content"\n'
        '                android:layout_marginStart="?android:attr/listPreferredItemPaddingStart"\n'
        '                android:layout_marginEnd="?android:attr/listPreferredItemPaddingEnd" />\n'
        '            <FrameLayout\n'
        '                android:id="@+id/contextual_cards_content"\n'
        '                android:layout_width="match_parent"\n'
        '                android:layout_height="wrap_content"\n'
        '                android:layout_marginStart="4dp"\n'
        '                android:layout_marginEnd="4dp" />\n'
        '            <FrameLayout\n'
        '                android:id="@+id/main_content"\n'
        '                android:layout_width="match_parent"\n'
        '                android:layout_height="wrap_content"\n'
        '                android:animateLayoutChanges="true" />\n'
        '        </LinearLayout>\n'
        '    </androidx.core.widget.NestedScrollView>\n'
        '    <com.google.android.material.appbar.AppBarLayout\n'
        '        android:id="@+id/app_bar"\n'
        '        android:layout_width="match_parent"\n'
        '        android:layout_height="wrap_content"\n'
        '        android:background="?android:attr/colorBackground"\n'
        '        android:touchscreenBlocksFocus="false"\n'
        '        android:keyboardNavigationCluster="false">\n'
        + title
        + '        <LinearLayout\n'
        '            android:id="@+id/app_bar_container"\n'
        '            android:layout_width="match_parent"\n'
        '            android:layout_height="wrap_content"\n'
        '            android:orientation="horizontal"\n'
        '            android:paddingTop="8dp"\n'
        '            android:paddingBottom="8dp"\n'
        '            android:paddingStart="?android:attr/listPreferredItemPaddingStart"\n'
        '            android:paddingEnd="?android:attr/listPreferredItemPaddingEnd"\n'
        '            android:background="?android:attr/colorBackground">\n'
        '            <LinearLayout\n'
        '                android:id="@+id/search_action_bar"\n'
        '                android:layout_width="match_parent"\n'
        '                android:layout_height="wrap_content"\n'
        '                android:minHeight="56dp"\n'
        '                android:gravity="center_vertical"\n'
        '                android:orientation="horizontal"\n'
        '                android:paddingStart="20dp"\n'
        '                android:paddingEnd="20dp"\n'
        '                android:clickable="true"\n'
        '                android:focusable="true"\n'
        '                android:focusedByDefault="true"\n'
        '                android:clipToOutline="true"\n'
        f'                android:background="@drawable/{SETTINGS_SEARCH_ICON}_background"\n'
        '                android:foreground="?android:attr/selectableItemBackground">\n'
        '                <ImageView\n'
        '                    android:layout_width="24dp"\n'
        '                    android:layout_height="24dp"\n'
        '                    android:importantForAccessibility="no"\n'
        f'                    android:src="@drawable/{SETTINGS_SEARCH_ICON}" />\n'
        '                <TextView\n'
        '                    android:id="@+id/search_bar_title"\n'
        '                    android:layout_width="wrap_content"\n'
        '                    android:layout_height="wrap_content"\n'
        '                    android:layout_marginStart="12dp"\n'
        '                    android:paddingEnd="8dp"\n'
        '                    android:text="@android:string/search_go"\n'
        '                    android:textSize="16sp"\n'
        '                    android:textColor="?android:attr/textColorSecondary" />\n'
        '            </LinearLayout>\n'
        '        </LinearLayout>\n'
        '    </com.google.android.material.appbar.AppBarLayout>\n'
        '</androidx.coordinatorlayout.widget.CoordinatorLayout>\n'
    )


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


def build_settings_overlay(aapt2, framework_res, work, home_padding=False, record=None):
    """Builds the Settings overlay APK (unsigned; signing gives it the platform key)."""
    sources = work / "settings-overlay"
    shutil.rmtree(sources, ignore_errors=True)
    settings_overlay_sources(sources, home_padding, record)
    return build_overlay(aapt2, framework_res, sources, sources / "DsaSettingsOverlay.apk")


def design_overlay_resources(apk):
    """The resource files of one of DESIGN_OVERLAYS, by their path in its sources, and its
    resource map: each target resource and the overlay's resource that replaces it."""
    if apk == "DsaDocumentsUiOverlay.apk":
        return ({"res/values/dsa.xml": resources_xml(
            [f'<bool name="{key.split("/", 1)[1]}">{value}</bool>' for key, value in DOCUMENTSUI_DESIGN.items()])},
            {key: key for key in DOCUMENTSUI_DESIGN})
    if apk == "DsaPermissionControllerOverlay.apk":
        # The dialog's title view takes its size and place from the style, which it names as
        # its style: the style says them all, with no parent of PermissionController's.
        place = ['<item name="android:layout_width">match_parent</item>',
                 '<item name="android:layout_height">wrap_content</item>',
                 '<item name="android:gravity">center</item>']
        files = {f"res/{folder}/dsa.xml": resources_xml([design_style(
            "DsaPermissionTitle", "", "24sp", f"@*android:color/system_on_surface_{theme}", place)])
            for folder, theme in (("values-v36", "light"), ("values-night-v36", "dark"))}
        return files, {f"style/{PERMISSION_TITLE_STYLE}": "style/DsaPermissionTitle"}
    if apk == "DsaPackageInstallerOverlay.apk":
        styles = [design_style(ours, "@android:style/TextAppearance.DeviceDefault.Headline",
                               f"{size}sp" if size else None,
                               "?android:attr/textColorPrimary" if size else None)
                  for ours, size in PACKAGE_INSTALLER_TITLES.values()]
        return ({"res/values/dsa.xml": resources_xml(styles)},
                {f"style/{name}": f"style/{ours}" for name, (ours, _) in PACKAGE_INSTALLER_TITLES.items()})
    if apk == "DsaIntentResolverOverlay.apk":
        return ({"res/values/dsa.xml": resources_xml(
            [f'<dimen name="{key.split("/", 1)[1]}">{value}</dimen>' for key, value in INTENT_RESOLVER_DESIGN.items()])},
            {key: key for key in INTENT_RESOLVER_DESIGN})
    if apk == GAPPS_FRAMEWORK_OVERLAY_APK:
        lines = []
        for key, value in GAPPS_FRAMEWORK_OVERLAY.items():
            kind, name = key.split("/", 1)
            text = str(value).lower() if kind == "bool" else value
            extra = ' translatable="false"' if kind == "string" else ""
            lines.append(f'<{kind} name="{name}"{extra}>{text}</{kind}>')
        return {"res/values/dsa.xml": resources_xml(lines)}, {key: key for key in GAPPS_FRAMEWORK_OVERLAY}
    raise ValueError(f"{apk} is not one of the design's overlays")


def system_overlay(apk):
    """The entry of SYSTEM_OVERLAYS for the overlay file `apk`."""
    return next(overlay for overlay in SYSTEM_OVERLAYS if overlay["apk"] == apk)


def design_overlay_sources(dest, apk, record=None):
    """Writes the sources of one of DESIGN_OVERLAYS into `dest`: its manifest, resource map
    and resources, and what it was built against when `record` says."""
    overlay = system_overlay(apk)
    files, mapping = design_overlay_resources(apk)
    write_overlay_files(dest, {
        "AndroidManifest.xml": overlay_manifest(overlay["package"], overlay["target"], resource_map=True,
                                                priority=overlay.get("priority", 100),
                                                required_property=overlay.get("required_property")),
        **files,
        f"res/xml/{SYSTEM_OVERLAY_MAP}.xml": overlay_map(mapping),
    })
    write_overlay_record(dest, record)


def build_design_overlay(aapt2, framework_res, work, apk, record=None):
    """Builds one of DESIGN_OVERLAYS (unsigned; signing gives it the platform key)."""
    sources = work / f"overlay-{Path(apk).stem}"
    shutil.rmtree(sources, ignore_errors=True)
    design_overlay_sources(sources, apk, record)
    return build_overlay(aapt2, framework_res, sources, sources / apk)


def overlaid_resources(apk, home_padding=False):
    """The resources ("type/name") the overlay file `apk` replaces in the package it overlays."""
    if apk == "DsaFrameworkOverlay.apk":
        return framework_overlaid()
    if apk == "DsaSettingsOverlay.apk":
        return list(settings_overlay_map(home_padding))
    return list(design_overlay_resources(apk)[1])


# A compiled resource table (resources.arsc): a table chunk holding a string pool and one
# package chunk per package. A package holds its type names and its entry names (key
# strings), then per type a spec chunk with a flag word per entry (whether the entry is
# public among them) and a type chunk per configuration, each with an offset per entry.
# framework-res keeps its private attributes in a type of their own, ARSC_PRIVATE_ATTR, so
# the public ones keep their ids.
ARSC_TABLE = 0x0002
ARSC_PACKAGE = 0x0200
ARSC_TYPE = 0x0201
ARSC_TYPE_SPEC = 0x0202
ARSC_SPEC_PUBLIC = 0x40000000
ARSC_PRIVATE_ATTR = "^attr-private"
ARSC_TYPE_SPARSE = 0x01
ARSC_TYPE_OFFSET16 = 0x02
ARSC_ENTRY_COMPACT = 0x0008
ARSC_NO_ENTRY = 0xFFFFFFFF
ARSC_NO_ENTRY16 = 0xFFFF


def _arsc_entry_offsets(table, at, flags, count):
    """The index and offset of each entry a type chunk holds, from its offset array at `at`."""
    if flags & ARSC_TYPE_SPARSE:
        pairs = [struct.unpack_from("<HH", table, at + 4 * i) for i in range(count)]
        return [(index, 4 * offset) for index, offset in pairs]
    if flags & ARSC_TYPE_OFFSET16:
        offsets = struct.unpack_from(f"<{count}H", table, at)
        return [(index, 4 * offset) for index, offset in enumerate(offsets) if offset != ARSC_NO_ENTRY16]
    offsets = struct.unpack_from(f"<{count}I", table, at)
    return [(index, offset) for index, offset in enumerate(offsets) if offset != ARSC_NO_ENTRY]


ARSC_TYPE_DIMENSION = 0x05
ARSC_UNIT_DIP = 1
BASE_DENSITY = 160  # the density at which a dp is one pixel
ARSC_CONFIG_DENSITY = 14  # where the density sits in a type chunk's configuration


def complex_dimension(data):
    """The value and unit of a compiled dimension (Android's complex value format): a signed
    24-bit mantissa, a radix and a unit."""
    mantissa = struct.unpack("<i", struct.pack("<I", data & 0xFFFFFF00))[0] / 256
    return mantissa / (1 << (0, 7, 15, 23)[(data >> 4) & 3]), data & 0xF


def arsc_dimensions(table):
    """{(name, density): (value, unit)} of each dimension a compiled resource table defines,
    by the density of its configuration (0 where it has none)."""
    chunk_type, header_size, _ = struct.unpack_from("<HHI", table, 0)
    if chunk_type != ARSC_TABLE:
        raise ValueError("not a compiled resource table")
    found = {}
    offset = header_size
    while offset < len(table):
        chunk_type, header_size, size = struct.unpack_from("<HHI", table, offset)
        if chunk_type == ARSC_PACKAGE:
            type_strings, _, key_strings = struct.unpack_from("<III", table, offset + 8 + 4 + 256)
            types = xml_strings(table, offset + type_strings)
            keys = xml_strings(table, offset + key_strings)
            child = offset + header_size
            while child < offset + size:
                kind, child_header, child_size = struct.unpack_from("<HHI", table, child)
                if kind == ARSC_TYPE:
                    type_id, flags, _, count, entries_start = struct.unpack_from("<BBHII", table, child + 8)
                    density = struct.unpack_from("<H", table, child + 20 + ARSC_CONFIG_DENSITY)[0]
                    if types[type_id - 1] == "dimen":
                        for _, entry in _arsc_entry_offsets(table, child + child_header, flags, count):
                            at = child + entries_start + entry
                            first, entry_flags = struct.unpack_from("<HH", table, at)
                            if entry_flags & ARSC_ENTRY_COMPACT:
                                key, data_type, data = first, entry_flags >> 8, struct.unpack_from("<I", table, at + 4)[0]
                            else:
                                key = struct.unpack_from("<I", table, at + 4)[0]
                                data_type, data = struct.unpack_from("<BI", table, at + first + 3)
                            if data_type == ARSC_TYPE_DIMENSION:
                                found[(keys[key], density)] = complex_dimension(data)
                child += child_size
        offset += size
    return found


def status_bar_overlay_problems(table):
    """What is wrong with the status bar heights of the framework overlay's resource table:
    each STATUS_BAR_DIMENS value must be there for every density status_bar_heights names and
    come to the title bar's pixels there, as getDimensionPixelSize rounds them, and the value
    without a density must be the highest of them."""
    found = arsc_dimensions(table)
    heights = status_bar_heights()
    problems = []
    for name in STATUS_BAR_DIMENS:
        for density, dp in sorted(heights.items()) + [(0, max(heights.values()))]:
            value = found.get((name, density))
            wanted = int(dp * (density or BASE_DENSITY) / BASE_DENSITY + 0.5)
            if value is None:
                problems.append(f"{name} has no value for {density or 'any'} dpi")
            elif value[1] != ARSC_UNIT_DIP or int(value[0] * (density or BASE_DENSITY) / BASE_DENSITY + 0.5) != wanted:
                problems.append(f"{name} is {value[0]:g} (unit {value[1]}) at {density or 'any'} dpi, not "
                                f"the title bar's {wanted} px there")
    return problems


def arsc_resource_names(table):
    """The resources a compiled resource table defines in any configuration, as "type/name"."""
    return set(arsc_resources(table))


def arsc_resources(table):
    """The resources a compiled resource table defines in any configuration, as "type/name",
    each with whether it is public. A private attribute of framework-res is "attr/name", as
    a reference names it."""
    chunk_type, header_size, _ = struct.unpack_from("<HHI", table, 0)
    if chunk_type != ARSC_TABLE:
        raise ValueError("not a compiled resource table")
    resources = {}
    offset = header_size
    while offset < len(table):
        chunk_type, header_size, size = struct.unpack_from("<HHI", table, offset)
        if chunk_type == ARSC_PACKAGE:
            # After the chunk header: the package id, its name (128 UTF-16 units), then the
            # offsets of its type and key string pools from the package's start.
            type_strings, _, key_strings = struct.unpack_from("<III", table, offset + 8 + 4 + 256)
            types = xml_strings(table, offset + type_strings)
            keys = xml_strings(table, offset + key_strings)
            specs = {}
            child = offset + header_size
            while child < offset + size:
                kind, child_header, child_size = struct.unpack_from("<HHI", table, child)
                if kind == ARSC_TYPE_SPEC:
                    type_id, _, _, count = struct.unpack_from("<BBHI", table, child + 8)
                    specs[type_id] = struct.unpack_from(f"<{count}I", table, child + child_header)
                elif kind == ARSC_TYPE:
                    type_id, flags, _, count, entries_start = struct.unpack_from("<BBHII", table, child + 8)
                    kind_name = types[type_id - 1]
                    kind_name = "attr" if kind_name == ARSC_PRIVATE_ATTR else kind_name
                    spec = specs.get(type_id, ())
                    for index, entry in _arsc_entry_offsets(table, child + child_header, flags, count):
                        at = child + entries_start + entry
                        first, entry_flags = struct.unpack_from("<HH", table, at)
                        key = first if entry_flags & ARSC_ENTRY_COMPACT else struct.unpack_from("<I", table, at + 4)[0]
                        public = index < len(spec) and bool(spec[index] & ARSC_SPEC_PUBLIC)
                        name = f"{kind_name}/{keys[key]}"
                        resources[name] = resources.get(name, False) or public
                child += child_size
        offset += size
    return resources


# What an overlay's sources name of the framework: android: attributes on elements and in
# style items, and @android:, @*android: and ?android: references.
FRAMEWORK_ATTRIBUTE = re.compile(r'(?<![\w:])android:(\w+)\s*=')
FRAMEWORK_STYLE_ITEM = re.compile(r'<item\s+name="android:(\w+)"')
FRAMEWORK_REFERENCE = re.compile(r'([@?])(\*?)android:(?:(\w+)/)?([\w.]+)')


def framework_references(sources):
    """Every framework resource the overlay sources in `sources` name, by the text that names
    it: the resource ("type/name") and whether the overlay needs it public. aapt2 links an
    overlay to framework-res only through its public resources, except where a reference
    says @*android:, which may name a private one; an attribute has no such form."""
    references = {}
    for path in sorted(Path(sources).rglob("*.xml")):
        text = path.read_text()
        for match in [*FRAMEWORK_ATTRIBUTE.finditer(text), *FRAMEWORK_STYLE_ITEM.finditer(text)]:
            references[f"android:{match[1]}"] = (f"attr/{match[1]}", True)
        for match in FRAMEWORK_REFERENCE.finditer(text):
            sigil, private, kind, name = match.groups()
            kind = kind or ("attr" if sigil == "?" else None)
            if kind is not None:
                references[match[0]] = (f"{kind}/{name}", sigil == "?" or not private)
    return references


def framework_reference_problems(sources, framework_table):
    """What the overlay sources in `sources` name of the framework that aapt2 would not link
    against `framework_table`, the image's framework-res: a resource it lacks, or one it
    keeps private that the overlay names as public."""
    resources = arsc_resources(framework_table)
    problems = []
    for text, (name, needs_public) in sorted(framework_references(sources).items()):
        if name not in resources:
            problems.append(f"{text} names {name}, which this image's framework-res does not have")
        elif needs_public and not resources[name]:
            problems.append(f"{text} names {name}, which this image's framework-res keeps private, "
                            "and an overlay can name only its public resources there")
    return problems


def apk_resource_table(apk):
    """An APK's compiled resource table."""
    with zipfile.ZipFile(io.BytesIO(apk)) as z:
        return z.read("resources.arsc")


def table_digest(apk):
    """The SHA-256 of an APK's compiled resource table, or None for a file that is no APK
    with one."""
    try:
        return hashlib.sha256(apk_resource_table(apk)).hexdigest()
    except (zipfile.BadZipFile, KeyError):
        return None


def missing_overlaid(overlaid, table):
    """What of `overlaid` ("type/name") the package whose resource table is `table` lacks: an
    overlay written for another build of it, which would overlay nothing there."""
    return sorted(set(overlaid) - arsc_resource_names(table))


def overlay_record(target, framework_table, target_table):
    """What an overlay of `target` is built against: the SHA-256 of the framework-res and
    target resource tables it was checked with. Signing changes neither table."""
    return {"target": target,
            "framework_res": hashlib.sha256(framework_table).hexdigest(),
            "target_resources": hashlib.sha256(target_table).hexdigest()}


def system_overlay_record_of(apk):
    """The record (SYSTEM_OVERLAY_RECORD) an overlay APK carries, or None."""
    try:
        with zipfile.ZipFile(io.BytesIO(apk)) as z:
            return json.loads(z.read(SYSTEM_OVERLAY_RECORD))
    except (zipfile.BadZipFile, KeyError, ValueError):
        return None


def apk_named(names, suffix):
    """The target_files name of the APK that ends with `suffix`, or None."""
    return next((name for name in names if name == suffix or name.endswith(suffix)), None)


def system_overlay_targets(z, otatools, work, env):
    """The resource table of each package a system screen overlay overlays, by overlay file,
    read from the target_files `z`: from the APK, or for PermissionController from inside the
    permission APEX, which otatools unpacks."""
    names = z.namelist()
    tables = {}
    for overlay in SYSTEM_OVERLAYS:
        if overlay["target_apk"] is not None:
            name = apk_named(names, overlay["target_apk"])
            if name is None:
                raise ValueError(f"the image has no {overlay['target_apk'].lstrip('/')}, which "
                                 f"{overlay['apk']} overlays")
            tables[overlay["apk"]] = apk_resource_table(z.read(name))
            continue
        apex = next((n for n in PERMISSION_APEXES if n in names), None)
        if apex is None:
            raise ValueError(f"the image has no permission APEX, whose {PERMISSION_CONTROLLER_APK} "
                             f"{overlay['apk']} overlays")
        folder = Path(work).resolve() / "apex-overlay-target"
        _, payload = apex_payload(z.read(apex), apex, Path(otatools).resolve(), folder, env)
        apks = sorted(payload.rglob(PERMISSION_CONTROLLER_APK))
        if not apks:
            raise ValueError(f"{apex} has no {PERMISSION_CONTROLLER_APK}, which {overlay['apk']} overlays")
        tables[overlay["apk"]] = apk_resource_table(apks[0].read_bytes())
        shutil.rmtree(folder, ignore_errors=True)
    return tables


def system_overlay_sources(dest, apk, home_padding=False, record=None):
    """Writes the sources of the system screen overlay file `apk` into `dest`."""
    if apk == "DsaFrameworkOverlay.apk":
        framework_overlay_sources(dest, record)
    elif apk == "DsaSettingsOverlay.apk":
        settings_overlay_sources(dest, home_padding, record)
    else:
        design_overlay_sources(dest, apk, record)


def build_system_overlays(aapt2, framework_res, work, tables, home_padding=False):
    """Builds every system screen overlay against `framework_res`, each first checked against
    the resource table of the package it overlays (`tables`, by overlay file): an overlay
    that would replace a resource the package lacks was written for another build, and the
    image is not made. Returns the framework overlay, the Settings overlay and the others
    ({file name: APK})."""
    framework_table = tables["DsaFrameworkOverlay.apk"]
    records = {}
    unlinkable = []
    for overlay in SYSTEM_OVERLAYS:
        apk = overlay["apk"]
        missing = missing_overlaid(overlaid_resources(apk, home_padding), tables[apk])
        if missing:
            raise ValueError(f"{apk} replaces {', '.join(missing)}, which {overlay['target']} in this "
                             "image does not have: the overlay was written for another build of it")
        records[apk] = overlay_record(overlay["target"], framework_table, tables[apk])
        # Every overlay's references are checked before any is linked, so one run names them
        # all, where aapt2 would stop at the first.
        sources = Path(work) / "overlay-references" / Path(apk).stem
        shutil.rmtree(sources, ignore_errors=True)
        system_overlay_sources(sources, apk, home_padding)
        unlinkable += [f"{apk}: {problem}" for problem in framework_reference_problems(sources, framework_table)]
        shutil.rmtree(sources, ignore_errors=True)
    if unlinkable:
        raise ValueError("the overlays name framework resources that cannot be linked against this image's "
                         "framework-res:\n  " + "\n  ".join(unlinkable))
    framework_overlay = build_framework_overlay(aapt2, framework_res, work, records["DsaFrameworkOverlay.apk"])
    settings_overlay = build_settings_overlay(aapt2, framework_res, work, home_padding,
                                              records["DsaSettingsOverlay.apk"])
    others = {apk: build_design_overlay(aapt2, framework_res, work, apk, records[apk]) for apk in DESIGN_OVERLAYS}
    return framework_overlay, settings_overlay, others


def system_overlay_problems(names, read):
    """What is wrong with the system screen overlays in a target_files whose names are `names`
    and whose files `read` reads: one missing, or one built against another framework-res or
    another build of the app it overlays. PermissionController is inside the permission APEX,
    which takes otatools to read; make-image.py checks its overlay against it."""
    problems = []
    framework = table_digest(read(FRAMEWORK_RES)) if FRAMEWORK_RES in names else None
    for overlay in SYSTEM_OVERLAYS:
        path = f"VENDOR/overlay/{overlay['apk']}"
        if path not in names:
            problems.append(f"{path} is missing, so {overlay['without']}")
            continue
        record = system_overlay_record_of(read(path))
        if not isinstance(record, dict) or record.get("target") != overlay["target"]:
            problems.append(f"{path} does not say which framework-res and {overlay['target']} it was built "
                            "against, so nothing shows it matches this image")
            continue
        if framework is not None and record.get("framework_res") != framework:
            problems.append(f"{path} was built against another framework-res than this image's, so its "
                            "references into the framework may name other resources")
        target = apk_named(names, overlay["target_apk"]) if overlay["target_apk"] else None
        if target is not None and record.get("target_resources") != table_digest(read(target)):
            problems.append(f"{path} was built for another build of {overlay['target']} than {target}, so "
                            "what it replaces may not be there")
        if path == FRAMEWORK_OVERLAY_APK:
            with zipfile.ZipFile(io.BytesIO(read(path))) as apk:
                table = apk.read("resources.arsc") if "resources.arsc" in apk.namelist() else None
            try:
                found = status_bar_overlay_problems(table) if table is not None else ["it has no resource table"]
            except (ValueError, IndexError, struct.error):
                found = ["its resource table cannot be read"]
            if found:
                problems.append(f"{path}: {'; '.join(found)}, so an app that pads by the status bar's height "
                                "does not line up with the window's title bar")
    return problems


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


XML_TYPE_ATTRIBUTE = 0x02


def set_xml_theme_attributes(axml, edits):
    """Compiled XML with, for each (element, attribute name) in `edits`, that attribute of
    every `element` set to the theme's value of the same attribute (?attr/name). The attribute
    is found by its name, in any namespace, as an app's own attributes (app:expanded) have ids
    that differ from build to build; its id comes from the file's resource map. An element
    without the attribute is left as it is. Returns the new file and the number of attributes
    it changed."""
    file_type, file_header, _ = struct.unpack_from("<HHI", axml, 0)
    if file_type != XML_FILE:
        raise ValueError("not a compiled Android XML file")
    out = bytearray(axml)
    strings, resource_ids = [], []
    changed = 0
    offset = file_header
    while offset < len(axml):
        chunk_type, header_size, size = struct.unpack_from("<HHI", axml, offset)
        if chunk_type == XML_STRING_POOL:
            strings = xml_strings(axml, offset)
        elif chunk_type == XML_RESOURCE_MAP:
            entries = (size - header_size) // 4
            resource_ids = list(struct.unpack_from(f"<{entries}I", axml, offset + header_size))
        elif chunk_type == XML_START_ELEMENT:
            _, name, attribute_start, attribute_size, count = struct.unpack_from("<IIHHH", axml, offset + header_size)
            for element, attribute in edits:
                if strings[name] != element:
                    continue
                for i in range(count):
                    at = offset + header_size + attribute_start + i * attribute_size
                    _, attribute_name = struct.unpack_from("<II", axml, at)
                    if strings[attribute_name] != attribute:
                        continue
                    if attribute_name >= len(resource_ids):
                        raise ValueError(f"the compiled XML gives {attribute} of {element} no resource id")
                    reference = resource_ids[attribute_name]
                    if struct.unpack_from("<BI", out, at + 15) == (XML_TYPE_ATTRIBUTE, reference):
                        continue
                    struct.pack_into("<IHBBI", out, at + 8, XML_NO_INDEX, 8, 0, XML_TYPE_ATTRIBUTE, reference)
                    changed += 1
        offset += size
    return bytes(out), changed


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


def _method_index(refs, name):
    """The index of the method "Lclass;->name" a dex file refers to exactly once."""
    found = [i for i, ref in enumerate(refs) if ref == name]
    if len(found) != 1:
        raise ValueError(f"the dex file refers to {name} {len(found)} times, not once")
    return found[0]


def edit_dex(dex, edits):
    """Applies {"Lclass;->method": edit, or a list of edits} to a dex file. An edit is
    ("return false", None) or ("return true", None), which makes a method that returns a
    boolean return that at once; ("skip call", "Lclass;->method"), which turns each call to
    that method, whose result is not used, into no-ops; or ("replace code", {"registers": n,
    "old": [...], "new": [...]}), which replaces a method whose code is exactly `old` with
    `new` (Dalvik assembly, see assemble_dex) in n registers, padded with no-ops to the same
    size; or ("replace call", {"after": a, "old": o, "new": n}), which makes the first call to
    the static method o after the first call to a a call to n, which takes and returns the same
    types (only the method the instruction names changes). Returns the new file and the
    methods it changed. A method already edited, or not in this file, is not changed; one
    whose code is neither `old` nor `new` is refused."""
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
            elif kind == "replace call":
                old_index, new_index = (_method_index(refs, target[k]) for k in ("old", "new"))
                method_shorty = dex_ids(dex)[1]
                if method_shorty[old_index] != method_shorty[new_index]:
                    raise ValueError(f"{target['new']} takes or returns other types than {target['old']}")
                calls = [(at, refs[struct.unpack_from("<H", dex, at + 2)[0]])
                         for at, op in dex_instructions(dex, code_off) if op in DEX_INVOKES]
                after = next((i for i, (_, ref) in enumerate(calls) if ref == target["after"]), None)
                found = next(((at, ref) for at, ref in calls[after + 1:]
                              if ref in (target["old"], target["new"])), None) if after is not None else None
                if found is None:
                    raise ValueError(f"{method} calls neither {target['old']} nor {target['new']} after "
                                     f"{target['after']}: it was built from other sources, and the edit "
                                     "must be written for them")
                at, ref = found
                if ref == target["new"]:
                    continue
                if out[at] != 0x71:
                    raise ValueError(f"{method} calls {target['old']} other than as a static method")
                struct.pack_into("<H", out, at + 2, new_index)
                changed.append(method)
            else:
                raise ValueError(f"unknown dex edit {kind}")
    if changed:
        out[12:32] = hashlib.sha1(bytes(out[32:])).digest()
        out[8:12] = struct.pack("<I", zlib.adler32(bytes(out[12:])))
    return bytes(out), changed


def edit_apk(data, name, code=None, controllers=None, dimensions=None, theme_attributes=None):
    """An APK with `code` edited in its dex files (see edit_dex), the preference elements
    whose controller `controllers` names removed from its compiled XML ({xml: [controller]}),
    the dimensions `dimensions` names set in its compiled XML ({xml: [(element,
    attribute, dp)]}, see set_xml_dimensions), and the attributes `theme_attributes` names
    taken from the theme ({xml: [(element, attribute name)]}, see set_xml_theme_attributes).
    Prints each change; returns the APK as it was when nothing changed."""
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
        for xml, edits in (theme_attributes or {}).items():
            if xml not in files:
                continue
            edited, changed = set_xml_theme_attributes(replacements.get(xml) or apk.read(xml), edits)
            if changed:
                for element, attribute in edits:
                    print(f"setting {element} attribute {attribute} to ?attr/{attribute} in {name}:{xml}",
                          flush=True)
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
    with room below the last row of the lists that set their own padding, and with its pages'
    large titles opening expanded or folded as the Settings overlay's theme says
    (SETTINGS_TITLE_EXPANDED)."""
    return edit_apk(data, name, code={**SETTINGS_CODE_WITHOUT_BLUETOOTH, **SETTINGS_CODE_LIST_MARGIN},
                    controllers=SETTINGS_CONTROLLERS_WITHOUT_BLUETOOTH, dimensions=SETTINGS_LIST_DIMENSIONS,
                    theme_attributes=SETTINGS_TITLE_EXPANDED)


ZIP_ALIGNMENT_EXTRA = 0xD935  # zipalign's alignment extra field: a 16-bit alignment, then padding


def aligned_zip(data, alignment=4):
    """The zip `data` with the data of each stored member starting at a multiple of
    `alignment` bytes, as zipalign leaves it. ART maps a jar's stored dex files straight from
    the file only when they are aligned, and otherwise copies them into memory: 20 MiB of
    system_server's for services.jar. Members keep their order, names, dates and compression."""
    out = io.BytesIO()
    with zipfile.ZipFile(io.BytesIO(data)) as source, zipfile.ZipFile(out, "w") as aligned:
        for info in source.infolist():
            member = zipfile.ZipInfo(info.filename, info.date_time)
            member.compress_type = info.compress_type
            member.external_attr = info.external_attr
            member.create_system = info.create_system
            if info.compress_type == zipfile.ZIP_STORED:
                # The local header is 30 bytes, then the name and the extra field.
                start = out.tell() + 30 + len(info.filename.encode("utf-8")) + 6
                padding = -start % alignment
                member.extra = struct.pack("<HHH", ZIP_ALIGNMENT_EXTRA, 2 + padding, alignment) + bytes(padding)
            aligned.writestr(member, source.read(info))
    return out.getvalue()


def zip_misaligned(data, alignment=4):
    """The stored dex files (classes*.dex) of the zip `data` whose data does not start at a
    multiple of `alignment` bytes."""
    found = []
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        for info in z.infolist():
            if info.compress_type != zipfile.ZIP_STORED or not re.fullmatch(r"classes\d*\.dex", info.filename):
                continue
            name_length, extra_length = struct.unpack_from("<HH", data, info.header_offset + 26)
            if (info.header_offset + 30 + name_length + extra_length) % alignment:
                found.append(info.filename)
    return found


def edit_services_jar(data, name):
    """services.jar with InsetsPolicy giving a freeform window its status bar inset
    (SERVICES_CODE), its stored dex files aligned again. Returns the jar as it was when it
    already has the edit."""
    edited = edit_apk(data, name, code=SERVICES_CODE)
    return data if edited is data else aligned_zip(edited)


def edit_documentsui_apk(data, name):
    """DocumentsUI.apk (Files) with room below its list's last row in windows that have no bar
    below the list."""
    return edit_apk(data, name, code=DOCUMENTSUI_CODE)


def edit_permission_controller_apk(data, name):
    """PermissionController.apk with room below the last row of its preference lists, and
    those lists grouped in cards with Settings' corners."""
    return edit_apk(data, name, dimensions=permission_controller_edits())


def permission_controller_edits():
    """The compiled XML edits PermissionController.apk gets: its lists' room and its cards."""
    return {**PERMISSION_CONTROLLER_DIMENSIONS, **PERMISSION_CONTROLLER_CARDS}


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
        if (key in props_to_apply or key in DROPPED_PROPS or key in BOOT_SET_PROPS
                or (vendor and key in VENDOR_FORBIDDEN_PROPS)):
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
# the guest through its next lease. While the host answers DNS, the daemon's stub takes every
# query sent to those servers and the host's own resolver answers it (dns_problems), so the
# host's hosts file, VPN and DNS filters apply; the DHCP servers are the fallback while it does
# not. Cuttlefish's rename_eth0 would hand eth0 to the radio as rmnet0, and DSA has no
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
# Where a sensors HAL installed as plain files keeps its service, init script and sub-HAL
# libraries. The interface libraries (VENDOR/lib64/android.hardware.sensors*.so) are not in
# these folders and stay: DSA's HAL links them.
CUTTLEFISH_SENSORS_FOLDERS = [
    "VENDOR/bin/hw/",
    "VENDOR/etc/init/",
    "VENDOR/lib/hw/",
    "VENDOR/lib64/hw/",
]
# The multi-HAL's list of sub-HALs to load.
CUTTLEFISH_SENSORS_CONFIG = "VENDOR/etc/sensors/"
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
    "display_dpi": DISPLAY_DPI,
}

# Turns on the zram swap fstab.dsa declares, and tunes the kernel for it.
MEMORY_RC = "VENDOR/etc/init/dsa-memory.rc"
# Sets the density SurfaceFlinger starts at from the boot's androidboot.lcd_density.
DISPLAY_RC = "VENDOR/etc/init/dsa-display.rc"
# Main thread timing for the input trace (dsa-trace.rc). A system script, so init itself runs it.
TRACE_RC = "SYSTEM/etc/init/dsa-trace.rc"
# Quiets the serial console once boot completes, so it carries only the kernel's early boot and
# a panic. A system script, because only init itself may write the kernel's printk levels.
CONSOLE_RC = "SYSTEM/etc/init/dsa-console.rc"
SU_GUARD_RC = "SYSTEM/etc/init/dsa-su-guard.rc"
# Makes the folder for zram's backing file (fstab.dsa) and removes the last boot's file. A
# system script, because only init itself may make a folder at the top of /data.
ZRAM_BACKING_RC = "SYSTEM/etc/init/dsa-zram-backing.rc"
SYSTEM_FS_CONFIG = "META/filesystem_config.txt"
TRACE_FS_CONFIG = [
    "system/etc/init/dsa-trace.rc 0 0 644 capabilities=0x0",
    "system/etc/init/dsa-console.rc 0 0 644 capabilities=0x0",
    "system/etc/init/dsa-zram-backing.rc 0 0 644 capabilities=0x0",
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
# Where an x86_64 image makes a translation package reachable, without laying anything over
# /system/bin, /system/lib64 or /system/etc: an overlay there makes the services in the folder
# run through overlayfs, which looks each file up with init's credentials, and init may not
# read most services' programs, so their exec fails (bpfloader's first, which reboots Android).
# The init script binds the package's folders onto these empty folders and its linker
# configuration onto the empty file. /system_ext/lib64 is on the system namespace's linker
# search path, where the zygote finds the bridge by name, and the system_ext partition has no
# libraries of its own there; the runners go under /system/bin/, where the linker gives a
# program the system section of its configuration. The links lead the translators that carry
# them to their CPU description and their own folder in the mount, and the runner names older
# packages' rules give to the bound runners. arm64 images take none of this: their own linker
# reads /system/etc/ld.config.arm64.txt, which would then be empty.
ARM_TRANSLATION_MOUNT = "/mnt/dsa-arm-translation"
ARM_TRANSLATION_FOLDERS = {
    "SYSTEM_EXT/lib64": "system/lib64",
    "SYSTEM/lib64/arm64": "system/lib64/arm64",
    "SYSTEM/bin/arm64": "system/bin/arm64",
    "SYSTEM/bin/dsa-arm-translation": "system/bin",
}
ARM_TRANSLATION_LINKER_CONFIG = "SYSTEM/etc/ld.config.arm64.txt"
# The library Berberis (Digitalis) runs its translated code in. It opens it by this absolute
# path, never by name, so that it can load it at an address it reserved first; with the
# package's system/lib64 bound onto /system_ext/lib64 alone, the path held nothing and every
# ARM app aborted as it started ("exec_region_elf_backed.cc:48: CHECK failed: 0 != 0"). The
# image carries an empty file there, and the init script binds the package's onto it. A link
# into the mount would not do: the linker checks the path it really opened, which would be
# under /mnt, against the system namespace, which may not load from there.
ARM_TRANSLATION_EXEC_REGION = "SYSTEM/lib64/libberberis_exec_region.so"
# The empty files the init script binds a package's files onto, each with what is lost
# without it.
ARM_TRANSLATION_BOUND_FILES = {
    ARM_TRANSLATION_LINKER_CONFIG: "the translator's ARM64 linker has no namespaces",
    ARM_TRANSLATION_EXEC_REGION: "the translator cannot load the library its code runs in, and "
                                 "every ARM app aborts as it starts",
}
# The runners Berberis (Digitalis) and Google's translator name in their binfmt_misc rules,
# led to where the package's system/bin is bound. Packages made now name the bound place
# themselves (make-package.py, and the host for Google's); these keep a package made before,
# such as a release's default translator, starting its ARM64 programs.
ARM_TRANSLATION_RUNNERS = ["berberis_program_runner_binfmt_misc_arm64",
                           "ndk_translation_program_runner_binfmt_misc_arm64"]
ARM_TRANSLATION_LINKS = {
    "SYSTEM/etc/cpuinfo.arm64.txt": f"{ARM_TRANSLATION_MOUNT}/system/etc/cpuinfo.arm64.txt",
    "SYSTEM/etc/berberis": f"{ARM_TRANSLATION_MOUNT}/system/etc/berberis",
    **{f"SYSTEM/bin/{runner}": f"dsa-arm-translation/{runner}" for runner in ARM_TRANSLATION_RUNNERS},
}
ARM_TRANSLATION_REACH_FS_CONFIG = {
    "META/filesystem_config.txt": [
        "system/lib64/arm64 0 0 755 capabilities=0x0",
        "system/bin/arm64 0 0 755 capabilities=0x0",
        "system/bin/dsa-arm-translation 0 0 755 capabilities=0x0",
        "system/etc/ld.config.arm64.txt 0 0 644 capabilities=0x0",
        "system/lib64/libberberis_exec_region.so 0 0 644 capabilities=0x0",
        "system/etc/cpuinfo.arm64.txt 0 0 644 capabilities=0x0",
        "system/etc/berberis 0 0 644 capabilities=0x0",
        *[f"system/bin/{runner} 0 0 755 capabilities=0x0" for runner in ARM_TRANSLATION_RUNNERS],
    ],
    "META/system_ext_filesystem_config.txt": [
        "system_ext/lib64 0 0 755 capabilities=0x0",
    ],
}


def arm_translation_reach_names():
    """Every target_files name the x86_64 image adds to reach a translation package."""
    return ([folder + "/" for folder in ARM_TRANSLATION_FOLDERS] + list(ARM_TRANSLATION_BOUND_FILES)
            + list(ARM_TRANSLATION_LINKS))


def arm_translation_reach_conflicts(names):
    """What a build's own target files already have where the image binds a translation
    package or puts its links: a file there would be hidden, or would hide the package's."""
    conflicts = []
    for folder in ARM_TRANSLATION_FOLDERS:
        inside = sorted(name for name in names if name.startswith(folder + "/") and name != folder + "/")
        if inside or folder in names:
            conflicts.append(f"{folder} already has {inside[:3] or [folder]}, which binding the ARM "
                             "translation package there would hide")
    for name in [*ARM_TRANSLATION_BOUND_FILES, *ARM_TRANSLATION_LINKS]:
        if name in names or any(other.startswith(name + "/") for other in names):
            conflicts.append(f"{name} is already in the build, where the ARM translation package "
                             "goes")
    return conflicts


def check_arm_translation_reach(manifest, names, read, info=None):
    """The problems with where an image makes a translation package reachable, given the names
    in its target files, a function that reads one, and one that gives a name's zip entry. An
    x86_64 image has each empty folder, the empty linker configuration and the links into the
    mount; an arm64 image has none of them, above all no linker configuration for arm64, which
    its own linker would read."""
    arch = manifest.get("arch") or DEFAULT_ARCH
    added = arm_translation_reach_names()
    if arch != "x86_64":
        return [f"the {arch} image has {name}, which only an x86_64 image's translator needs"
                for name in added if name in names or name.rstrip("/") in names]
    problems = []
    for folder in ARM_TRANSLATION_FOLDERS:
        inside = [name for name in names if name.startswith(folder + "/") and name != folder + "/"]
        if folder + "/" not in names and not inside:
            problems.append(f"{folder} is missing, so the ARM translation package has nowhere to go")
        elif inside:
            problems.append(f"{folder} holds {sorted(inside)[:3]}, which the ARM translation package "
                            "hides")
    for name, lost in ARM_TRANSLATION_BOUND_FILES.items():
        if name not in names:
            problems.append(f"{name} is missing, so {lost}")
        elif read(name):
            problems.append(f"{name} is not empty: it is the build's, not the place the package's "
                            "goes")
    for name, target in ARM_TRANSLATION_LINKS.items():
        if name not in names:
            problems.append(f"{name} is missing, so a translator never finds its own {target}")
            continue
        entry = info(name) if info is not None else None
        if entry is not None and not stat.S_ISLNK(entry.external_attr >> 16):
            problems.append(f"{name} is not a link to {target}")
        elif read(name).decode(errors="replace") != target:
            problems.append(f"{name} does not lead to {target}")
    return problems


# The folders nothing may be laid over or bound onto: the partitions' roots and the folders the
# system's own programs and libraries are in. A service started from one through overlayfs is
# looked up with init's credentials, which may not read most services' programs, so it fails.
PROGRAM_FOLDERS = {"/system", "/system/bin", "/system/xbin", "/system/lib64", "/system/lib",
                   "/system/etc", "/product", "/product/bin", "/product/lib64", "/system_ext",
                   "/system_ext/bin", "/vendor", "/vendor/bin", "/vendor/lib64", "/odm"}


def mounts_onto(rc):
    """Each mount command of an init script: its type and where it mounts."""
    found = []
    for line in rc.splitlines():
        words = line.split()
        if len(words) >= 4 and words[0] == "mount":
            found.append((words[1], words[3]))
    return found


def program_folder_problems(rc_name, rc):
    """The mounts of the init script `rc_name` that lay something over a folder of the
    system's programs (PROGRAM_FOLDERS), and, for the translation script, any overlay."""
    problems = []
    for kind, point in mounts_onto(rc):
        if point.rstrip("/") in PROGRAM_FOLDERS:
            problems.append(f"{rc_name} mounts {kind} onto {point}, so the services there run through "
                            "it and fail to start")
        elif rc_name == "dsa-arm-translation.rc" and kind == "overlay":
            problems.append(f"{rc_name} lays an overlay over {point}; the package is bound onto "
                            "folders of its own")
    return problems


def directory_entry(name):
    """A zip entry for the empty folder `name` (ending in /)."""
    info = zipfile.ZipInfo(name)
    info.external_attr = (0o40755 << 16) | 0x10
    return info


def link_entry(name):
    """A zip entry for a symbolic link at `name`, whose data is its target."""
    info = zipfile.ZipInfo(name)
    info.external_attr = 0o120777 << 16
    return info


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

# DSA's KeyMint, from the same Framework build: the nonsecure KeyMint service whose root key of
# key blobs comes from the DSA host (dsa-channeld --keymint-key leaves it in
# /dev/dsa_keymint/root_key each boot), with the libssl it needs and the VINTF fragments and
# feature file the KeyMint APEX carries. The image installs them as vendor files and selects no
# KeyMint APEX (bootconfig "none", which apexd takes as "activate none of this module's
# APEXes"), so one KeyMint serves each instance. init starts it as the APEX's service was
# started, under the same name, and in the same domain: its label is the one the APEX gave its
# binary, hal_keymint_default_exec, and hal_keymint_default serves the KeyMint HAL. A framework
# build from before the KeyMint has none of these files, and the image then keeps the APEX.
KEYMINT_FILES = list(framework_lock.KEYMINT_FILES)
KEYMINT_ELF_FILES = list(framework_lock.KEYMINT_OUTPUTS)
KEYMINT_BINARY = framework_lock.KEYMINT_BINARY
KEYMINT_RC = "VENDOR/etc/init/dsa-keymint.rc"
KEYMINT_RC_SOURCE = HERE / "dsa-keymint.rc"
KEYMINT_CONTEXT = "/vendor/bin/hw/dsa-keymint u:object_r:hal_keymint_default_exec:s0"
KEYMINT_APEX_MODULE = "com.android.hardware.keymint"
KEYMINT_APEX_SELECTION = f"androidboot.vendor.apex.{KEYMINT_APEX_MODULE}=none"
# The instances the KeyMint service registers (kmr_hal's ALL_HALS), each of which a fragment must
# declare.
KEYMINT_INSTANCES = [
    ("aidl", "android.hardware.security.keymint", "IKeyMintDevice", "default"),
    ("aidl", "android.hardware.security.keymint", "IRemotelyProvisionedComponent", "default"),
    ("aidl", "android.hardware.security.secureclock", "ISecureClock", "default"),
    ("aidl", "android.hardware.security.sharedsecret", "ISharedSecret", "default"),
]
# The libraries a vendor binary gets from the system partition (the LLNDK) rather than from
# /vendor/lib64, so need not be in the vendor partition.
LLNDK_LIBRARIES = {
    "libc.so", "libm.so", "libdl.so", "liblog.so", "libbinder_ndk.so", "libvndksupport.so",
    "libsync.so", "libnativewindow.so", "libselinux.so", "libcgrouprc.so", "libandroid_net.so",
    "libmediandk.so", "libvulkan.so", "libEGL.so", "libGLESv1_CM.so", "libGLESv2.so",
    "libGLESv3.so", "libstatssocket.so",
}


def framework_has_keymint(names):
    """Whether a framework build's files (their target_files names) carry DSA's KeyMint."""
    return KEYMINT_BINARY in names


def elf_needed(data):
    """The DT_NEEDED entries of a little-endian 64-bit ELF file, or None for anything else."""
    if elf_machine(data[:64]) is None or len(data) < 64:
        return None
    shoff = int.from_bytes(data[0x28:0x30], "little")
    shentsize = int.from_bytes(data[0x3A:0x3C], "little")
    shnum = int.from_bytes(data[0x3C:0x3E], "little")
    sections = []
    for i in range(shnum):
        at = shoff + i * shentsize
        if at + 64 > len(data):
            return None
        kind = int.from_bytes(data[at + 4:at + 8], "little")
        offset = int.from_bytes(data[at + 24:at + 32], "little")
        size = int.from_bytes(data[at + 32:at + 40], "little")
        link = int.from_bytes(data[at + 40:at + 44], "little")
        sections.append((kind, offset, size, link))
    needed = []
    for kind, offset, size, link in sections:
        if kind != 6 or link >= len(sections):  # SHT_DYNAMIC, with its string table
            continue
        strings = sections[link][1]
        for at in range(offset, offset + size, 16):
            tag = int.from_bytes(data[at:at + 8], "little", signed=True)
            if tag == 0:
                break
            if tag == 1:  # DT_NEEDED
                start = strings + int.from_bytes(data[at + 8:at + 16], "little")
                needed.append(data[start:data.index(b"\0", start)].decode(errors="replace"))
    return needed


def keymint_library_problems(binary, vendor_names):
    """The libraries DSA's KeyMint binary needs that neither the vendor partition (by the
    target_files names in `vendor_names`) nor the LLNDK gives it: without one, the linker stops
    it, and Android has no KeyMint."""
    needed = elf_needed(binary)
    if needed is None:
        return ["the KeyMint binary is not an ELF file whose libraries can be read"]
    return [f"the KeyMint binary needs {lib}, which the vendor partition does not carry"
            for lib in needed if lib not in LLNDK_LIBRARIES and f"VENDOR/lib64/{lib}" not in vendor_names]


def selected_vendor_apexes(cmdline):
    """The vendor APEX each module's bootconfig selection names, by module."""
    selected = {}
    for part in cmdline.split():
        if part.startswith("androidboot.vendor.apex.") and "=" in part:
            module, value = part[len("androidboot.vendor.apex."):].split("=", 1)
            selected[module] = value
    return selected


def keymint_problems(manifest, entry, names, read):
    """The problems with DSA's KeyMint in an image whose framework `entry` (image.json's) names
    it: its files are the framework build's, init starts it in the APEX's domain, and no KeyMint
    APEX is active beside it."""
    problems = []
    files = entry.get("files") or {}
    arch = manifest.get("arch") or DEFAULT_ARCH
    machine = ARCHES.get(arch, {}).get("elf_machine")
    for name in KEYMINT_FILES:
        if name not in names:
            problems.append(f"{name} is missing from the image")
            continue
        data = read(name)
        if hashlib.sha256(data).hexdigest() != files.get(name):
            problems.append(f"{name} is not the framework build's file that image.json names, so the "
                            "image's KeyMint is not the one that takes its root key from the host")
        if name in KEYMINT_ELF_FILES and elf_machine(data[:64]) != machine:
            problems.append(f"{name} is built for ELF machine {elf_machine(data[:64])}, not {arch}")
    declared = set()
    for name in KEYMINT_FILES:
        if name.startswith("VENDOR/etc/vintf/") and name in names:
            declared |= vintf_instances(read(name))
    for instance in KEYMINT_INSTANCES:
        if instance not in declared:
            problems.append(f"the KeyMint's fragments do not declare {instance[1]}.{instance[2]}/{instance[3]}, "
                            "so servicemanager refuses it")
    if KEYMINT_RC not in names:
        problems.append(f"{KEYMINT_RC} is missing, so init never starts the KeyMint")
    elif b"/vendor/bin/hw/dsa-keymint" not in read(KEYMINT_RC):
        problems.append(f"{KEYMINT_RC} does not start /vendor/bin/hw/dsa-keymint")
    contexts = "VENDOR/etc/selinux/vendor_file_contexts"
    if contexts in names and KEYMINT_CONTEXT not in read(contexts).decode(errors="replace").splitlines():
        problems.append("the KeyMint binary is not labelled hal_keymint_default_exec, so init does not "
                        "start it in the KeyMint HAL's domain")
    if "META/file_contexts.bin" in names and b"/vendor/bin/hw/dsa-keymint" not in read("META/file_contexts.bin"):
        problems.append("META/file_contexts.bin does not label dsa-keymint, so the vendor image gives it "
                        "another label and init refuses to start it")
    if KEYMINT_BINARY in names:
        problems += keymint_library_problems(read(KEYMINT_BINARY), names)
    cmdline = manifest.get("kernel_cmdline", "")
    selected = selected_vendor_apexes(cmdline)
    if selected.get(KEYMINT_APEX_MODULE) != "none":
        problems.append(f"the kernel command line does not have {KEYMINT_APEX_SELECTION}, so apexd "
                        "activates a KeyMint APEX beside DSA's KeyMint")
    for name in vendor_apex_members(names):
        if apex_module_name(read(name)) != KEYMINT_APEX_MODULE:
            continue
        if KEYMINT_APEX_MODULE not in selected or selected[KEYMINT_APEX_MODULE] == apex_file_stem(name):
            problems.append(f"{name} is active beside DSA's KeyMint")
    return problems


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
    wanted = framework_names(files)
    if sorted(files) != sorted(wanted):
        problems.append(f"the framework has {sorted(files)}, not {sorted(wanted)}")
    machine = ARCHES.get(arch, {}).get("elf_machine")
    for name in wanted:
        entry = files.get(name) or {}
        data = read(name)
        if data is None:
            problems.append(f"the framework's {name} is missing")
            continue
        if len(data) != entry.get("size") or hashlib.sha256(data).hexdigest() != entry.get("sha256"):
            problems.append(f"the framework's {name} is not the file framework.json describes")
        if name in FRAMEWORK_FILES + KEYMINT_ELF_FILES and elf_machine(data[:64]) != machine:
            problems.append(f"the framework's {name} is built for ELF machine {elf_machine(data[:64])}, "
                            f"not {arch} ({machine})")
    return problems


def framework_names(files):
    """The files a framework build must have, given the names its framework.json lists: the
    framework's three, and the KeyMint's when it has any of them (a build from before the KeyMint
    has none)."""
    if any(name in KEYMINT_FILES for name in files):
        return FRAMEWORK_FILES + KEYMINT_FILES
    return list(FRAMEWORK_FILES)


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
    return {name: read(name) for name in framework_names(description["files"])}, description


def framework_entry(description):
    """What image.json says of the framework an image carries."""
    return {
        "version": description.get("version", ""),
        "build_id": description["build_id"],
        "release": description.get("release", ""),
        "arch": description["arch"],
        "files": {name: description["files"][name]["sha256"]
                  for name in framework_names(description["files"])},
    }


def check_framework(manifest, names, read):
    """The problems with the patched framework in an image, given image.json, the names in its
    target files and a function that reads one: each file image.json names is in the image with
    that SHA-256, built from the image's own build for its architecture, SurfaceFlinger may
    tell the apps about a reset, and DSA's KeyMint, when the framework has it, is the one Android
    runs (keymint_problems)."""
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
    wanted = framework_names(files)
    if sorted(files) != sorted(wanted):
        problems.append(f"image.json's framework names {sorted(files)}, not {sorted(wanted)}")
    if framework_has_keymint(files):
        problems += keymint_problems(manifest, entry, names, read)
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


# The user's add-on disk: a read-only disk the host builds and attaches, whose one partition
# (dsa_addons) is an erofs file system holding the add-on images the user installed as plain
# files. The host puts androidboot.dsa.addon_disk=on in the boot's bootconfig only while it
# attaches the disk; the script then mounts it at /mnt/dsa-addons before post-fs-data, where
# the Google Play and ARM translation scripts mount each image from it.
ADDONS_RC = "SYSTEM/etc/init/dsa-addons.rc"
ADDONS_FS_CONFIG = ["system/etc/init/dsa-addons.rc 0 0 644 capabilities=0x0"]
ADDONS_TRIGGER = "on post-fs && property:ro.boot.dsa.addon_disk=on"
ADDONS_MOUNT = ("mount erofs /dev/block/by-name/dsa_addons /mnt/dsa-addons ro nodev nosuid noexec "
                "fscontext=u:object_r:dsa_addons_fs:s0,context=u:object_r:system_file:s0")
# Where each add-on's image is on the mounted disk, and the script that mounts it.
ADDON_IMAGES = {
    "dsa-gapps.rc": "/mnt/dsa-addons/gapps.img",
    "dsa-arm-translation.rc": "/mnt/dsa-addons/arm-translation.img",
}


def addon_disk_problems(names, read):
    """The problems with how an image mounts the user's add-on disk, given the names in its
    target files and a function that reads one: the script is there, mounts the disk read only
    and only when the host says it attached one, before post-fs-data, and the Google Play and ARM
    translation scripts mount each image from it."""
    if ADDONS_RC not in names:
        return [f"{ADDONS_RC} is missing, so the add-ons a user installs are never mounted"]
    problems = []
    rc = read(ADDONS_RC).decode(errors="replace")
    trigger = None
    mounts = []
    for line in rc.splitlines():
        if line.startswith("on "):
            trigger = line.strip()
        elif line.strip().startswith("mount "):
            mounts.append((trigger, line.strip()))
    if mounts != [(ADDONS_TRIGGER, ADDONS_MOUNT)]:
        problems.append("dsa-addons.rc does not mount the add-on disk alone, read only and labelled as "
                        f"system files, at post-fs once the host says it attached one ({ADDONS_MOUNT}), "
                        "so the add-on images are not there when post-fs-data mounts them")
    for rc_name, image in ADDON_IMAGES.items():
        path = f"SYSTEM/etc/init/{rc_name}"
        if path in names and f"mount erofs {image} " not in read(path).decode(errors="replace"):
            problems.append(f"{rc_name} never mounts {image}, so that add-on is not taken from the "
                            "add-on disk")
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
# What the "on" file sets so that a privileged permission no list names is logged and granted
# rather than stopping system_server at every boot. The product build.prop imports the file
# last (before only the ARM translation import, which sets no such property), and the product
# partition's properties are read after every other partition's, so this wins over an
# "enforce" set anywhere else.
GAPPS_PRIVAPP_LOG = "ro.control_privapp_permissions=log"
# Google Play services and the Play Store compiled again without an app image, which each of
# their processes would otherwise load into memory of its own (dsa-gapps.rc). The script runs
# as the shell; the agent asks for it through the first property, and init keeps the build's
# app image format in the second while it clears it for the compile (sepolicy/gapps.cil).
GAPPS_COMPILE_SCRIPT = "SYSTEM/etc/dsa/gapps-compile.sh"
GAPPS_COMPILE_PACKAGES = ["com.google.android.gms", "com.android.vending"]
GAPPS_COMPILE_PROPERTY = "vendor.dsa.gapps.compile"
GAPPS_IMAGE_FORMAT_PROPERTY = "vendor.dsa.gapps.appimageformat"
GAPPS_COMPILE_PROPERTY_CONTEXTS = {
    GAPPS_COMPILE_PROPERTY: f"{GAPPS_COMPILE_PROPERTY} u:object_r:dsa_gapps_compile_prop:s0 exact string",
    GAPPS_IMAGE_FORMAT_PROPERTY: f"{GAPPS_IMAGE_FORMAT_PROPERTY} u:object_r:dsa_gapps_compile_prop:s0 exact string",
}
GAPPS_FS_CONFIG = [
    "system/etc/init/dsa-gapps.rc 0 0 644 capabilities=0x0",
    "system/etc/dsa/gapps-on.prop 0 0 644 capabilities=0x0",
    "system/etc/dsa/gapps-off.prop 0 0 644 capabilities=0x0",
    "system/etc/dsa/gapps-compile.sh 0 0 644 capabilities=0x0",
]


# The folders of /product and /system_ext the init script lays the Google apps image over:
# never a partition's root, whose bin folder holds services that cannot start through an
# overlay. The image the host makes holds every one of them, and the system must too, so each
# overlay finds both layers; the image adds the ones the build leaves out, empty.
GAPPS_PARTITIONS = ["PRODUCT", "SYSTEM_EXT"]
GAPPS_FOLDERS = ["app", "priv-app", "framework", "etc/permissions", "etc/sysconfig",
                 "etc/default-permissions", "etc/preferred-apps"]
GAPPS_PARTITION_FS_CONFIG = {
    "PRODUCT": "META/product_filesystem_config.txt",
    "SYSTEM_EXT": "META/system_ext_filesystem_config.txt",
}


def gapps_folders():
    """The target_files folders (PRODUCT/app, ...) the Google apps are laid over."""
    return [f"{partition}/{folder}" for partition in GAPPS_PARTITIONS for folder in GAPPS_FOLDERS]


def missing_folders(names, folders):
    """The folders of `folders` that no name of a target_files archive is in or names."""
    return [folder for folder in folders
            if not any(name == folder + "/" or name.startswith(folder + "/") for name in names)]


def gapps_folder_problems(names):
    """The folders the Google apps are laid over that an image's target files lack: each one
    lacking fails its overlay, so the Google apps never start."""
    return [f"{folder} is missing, so the Google apps are never laid over /{folder.lower()}"
            for folder in missing_folders(names, gapps_folders())]


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
# The zram writeback (dsa-channeld.rc): the daemon sets the budget of the run, in 4 KiB pages,
# and then the property init acts on. init sets how long no app must be open before a
# writeback and how long a page must go unused before it is written (dsa-memory.rc), from the
# boot's androidboot values or the defaults, and the daemon reads them. The daemon keeps what it
# wrote today in the persistent property, so a restart of Android does not start a new day's
# budget.
WRITEBACK_PROPERTY_CONTEXTS = {
    "vendor.dsa.memory.writeback": "vendor.dsa.memory.writeback u:object_r:dsa_memory_prop:s0 exact bool",
    "vendor.dsa.memory.writeback_pages":
        "vendor.dsa.memory.writeback_pages u:object_r:dsa_memory_prop:s0 exact int",
    "vendor.dsa.memory.writeback_after":
        "vendor.dsa.memory.writeback_after u:object_r:dsa_memory_prop:s0 exact int",
    "vendor.dsa.memory.writeback_age":
        "vendor.dsa.memory.writeback_age u:object_r:dsa_memory_prop:s0 exact int",
    "persist.vendor.dsa.memory.writeback_day":
        "persist.vendor.dsa.memory.writeback_day u:object_r:dsa_memory_prop:s0 exact string",
}
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
# dsa-channeld sets this to 1 when the host's config turns root elevation on for this boot and
# to 0 when it is off; dsa-channeld.rc starts and stops the elevation helper to match (su.cil).
ROOT_ELEVATION_PROPERTY = "vendor.dsa.root_elevation"
ROOT_ELEVATION_PROPERTY_CONTEXT = f"{ROOT_ELEVATION_PROPERTY} u:object_r:dsa_root_elevation_prop:s0 exact bool"
# dsa-channeld sets this to "on" while the host answers the guest's DNS queries and to "off"
# when it stays unreachable; dsa-channeld.rc has dsa-channeld --dns-redirect send every DNS
# query to the daemon's stub, or stop doing so, to match (channeld.cil).
DNS_PROPERTY = "vendor.dsa.dns"
DNS_PROPERTY_CONTEXT = f"{DNS_PROPERTY} u:object_r:dsa_dns_prop:s0 exact enum on off"
DNS_REDIRECT_SERVICE = "service dsa_dns_redirect /vendor/bin/dsa-channeld --dns-redirect"
DNS_REDIRECT_TRIGGER = f"on property:{DNS_PROPERTY}=*"
# Root elevation's su client. It is a system file, where an app's PATH and the kernel's su
# guard look for su, labelled with a type of its own so the policy lets apps run that file
# alone (su.cil). The label goes in only with the daemon, whose policy declares the type.
PLAT_FILE_CONTEXTS = "SYSTEM/etc/selinux/plat_file_contexts"
SU_CLIENT_BINARY = "SYSTEM/bin/su"
SU_CLIENT_PATH = "/system/bin/su"
SU_CLIENT_CONTEXT = f"{SU_CLIENT_PATH} u:object_r:dsa_su_exec:s0"
# zram's backing file, /data/per_boot/zram_swap (fstab.dsa), and its folder, which
# dsa-zram-backing.rc makes. A type of its own (memory.cil), so the loop driver may read and
# write it and nothing else may open it. memory.cil is in every image, so the label is too.
ZRAM_BACKING_PATH = "/data/per_boot(/.*)?"
ZRAM_BACKING_CONTEXT = f"{ZRAM_BACKING_PATH} u:object_r:dsa_zram_backing_file:s0"
# The userdebug target_files build ships AOSP's own setuid su at /system/xbin/su (mode 04750,
# root:shell). Even when the kernel's su guard hides its inode in LSM hooks, generic VFS
# permission checks run DAC (generic_permission) before security_inode_permission, so an
# unprivileged app calling access(X_OK) or execve() on that 04750 file gets EACCES from DAC
# before the guard's hook can return ENOENT. It is dropped from the image.
AOSP_XBIN_SU_BINARY = "SYSTEM/xbin/su"
# The elevation helper: the daemon's program in a mode of its own, and the sockets init makes
# in /dev/socket when it starts it. su connects on the first, dsa-channeld on the second.
SU_HELPER_SERVICE = "service dsa_su /vendor/bin/dsa-channeld --su-relay"
SU_SOCKET = "/dev/socket/dsa_su"
SU_RELAY_SOCKET = "/dev/socket/dsa_su_relay"
SU_SOCKET_CONTEXTS = [
    (r"/dev/socket/dsa_su ", r"/dev/socket/dsa_su u:object_r:dsa_su_socket:s0"),
    (r"/dev/socket/dsa_su_relay ", r"/dev/socket/dsa_su_relay u:object_r:dsa_su_relay_socket:s0"),
]
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
# KeyMint's root key from the host: dsa-channeld --keymint-key, which dsa-channeld.rc defines,
# writes it to a tmpfs folder that a system script makes and starts it for on init (as init:
# a vendor script's mkdir runs as vendor_init). The folder's label lets only KeyMint's domain
# read the key (channeld.cil).
KEYMINT_KEY_RC = "SYSTEM/etc/init/dsa-keymint-key.rc"
KEYMINT_KEY_SERVICE = "service dsa_keymint_key /vendor/bin/dsa-channeld --keymint-key"
KEYMINT_KEY_DIR = "/dev/dsa_keymint"
KEYMINT_KEY_CONTEXT = f"{KEYMINT_KEY_DIR}(/.*)? u:object_r:dsa_keymint_key_file:s0"
# The root certificates a user or administrator added to the host, among Android's system
# certificates: a system script (as init, which alone may mount over a system folder) makes a
# tmpfs folder, runs dsa-channeld --cacerts-setup (dsa-channeld.rc) to copy Android's two
# certificate folders into it, and binds each copy over the original before the zygote starts.
# The daemon then adds the host's roots there. The folder's label lets the daemon write in it
# and every domain read it (cacerts.cil).
CACERTS_RC = "SYSTEM/etc/init/dsa-cacerts.rc"
CACERTS_SERVICE = "service dsa_cacerts_setup /vendor/bin/dsa-channeld --cacerts-setup"
CACERTS_DIR = "/dev/dsa_cacerts"
CACERTS_CONTEXT = f"{CACERTS_DIR}(/.*)? u:object_r:dsa_cacerts_file:s0"
CACERTS_SCRIPT = [
    "on post-fs-data",
    f"    mkdir {CACERTS_DIR} 0755 system system",
    "    exec_start dsa_cacerts_setup",
    f"    mount none {CACERTS_DIR}/conscrypt /apex/com.android.conscrypt/cacerts bind",
    f"    mount none {CACERTS_DIR}/system /system/etc/security/cacerts bind",
]
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
# The agent's system configuration: Android's backup manager uses a backup transport only when
# a sysconfig file lists it, so this one names the agent's, which keeps app data in the host's
# backup folder.
AGENT_SYSCONFIG = "SYSTEM_EXT/etc/sysconfig/sysconfig-dsa-agent.xml"
AGENT_SYSCONFIG_SOURCE = REPO / "dsa/guest/agent/sysconfig-dsa-agent.xml"
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
# The activity manager's cached process limit when the device configuration sets none: 8, not
# Android 17's 1024, so a phone's dozens of cached processes do not hold memory the host would
# get back. Android 17 reads the limit from the device configuration
# (activity_manager/max_cached_processes, which the agent sets to the same value) and else from
# this resource; the global setting activity_manager_constants no longer carries it. The
# resource holds from the first boot, before the agent runs.
FRAMEWORK_OVERLAY_INTEGERS = {"config_tapTimeoutMillis": 0, "config_customizedMaxCachedProcesses": 8}
# The user's aspect ratio override of an app, which Settings offers on the app's aspect ratio
# page and a task window's system menu opens. The window manager applies the override only
# while this flag is on, and Android leaves it off unless a device turns it on, as large
# screen devices do.
#
# The window manager keeps the boot animation up until a wallpaper has drawn
# (config_checkWallpaperAtBoot). The image has no wallpaper to draw: Android's default one is
# SystemUI's ImageWallpaper, and SystemUI is removed, so the window manager gave up only at
# its 30 s boot timeout ("BOOT TIMEOUT: forcing display enabled"). Every boot was 30 s
# longer, and the user's storage was unlocked, and widgets became available, 30 s later.
#
# The agent is the guest's network location provider, answered by the host's location, with or
# without Google Play: with the overlay flag off, the location manager binds the package the
# string names and no other, where Google Play's provider would otherwise win on its higher
# service version. The agent is the geocode provider only without Google Play: these values
# name it, and the Google apps overlay (GAPPS_FRAMEWORK_OVERLAY) names Google's geocoder instead
# on a boot with the Google apps image.
FRAMEWORK_OVERLAY_BOOLEANS = {
    "config_appCompatUserAppAspectRatioSettingsIsEnabled": True,
    "config_checkWallpaperAtBoot": False,
    "config_enableNetworkLocationOverlay": False,
    "config_enableGeocoderOverlay": False,
}
# The device configurator: the package the framework lets write DeviceConfig, Android's server
# flags (WRITE_DEVICE_CONFIG and WRITE_ALLOWLISTED_DEVICE_CONFIG are "configurator"
# permissions). On a phone with Google Play it is Google Play services, whose platform
# configurator copies the flags Google serves into DeviceConfig. The image is built from AOSP,
# which names no configurator ("Configurator: none" in `dumpsys package`), so Play services was
# never granted those permissions and retried: one boot on a PC fired its RETRY_UPDATE alarm 720
# times in about ten minutes, each a run of the Play services process. Without Google Play
# no package has this name, and the framework takes the configurator to be absent, as before.
GAPPS_CONFIGURATOR = "com.google.android.gms"
FRAMEWORK_OVERLAY_STRINGS = {
    "config_deviceConfiguratorPackageName": GAPPS_CONFIGURATOR,
    "config_networkLocationProviderPackageName": "io.github.thisisthepy.dsa.agent",
    "config_geocoderProviderPackageName": "io.github.thisisthepy.dsa.agent",
}
# Google apps: on a boot with the Google apps image (the host puts androidboot.dsa.gapps=on
# in the bootconfig, so ro.boot.dsa.gapps is on before the package manager scans the overlays),
# this static overlay, above the framework overlay, names Google's geocoder, and the agent is
# not the geocode provider: Google's answers with street addresses and turns addresses into
# positions, which the host's boundary data does not. Without the Google apps image the package
# manager leaves the overlay out, and the agent's geocoder (the framework overlay's values)
# stands. The network location provider stays the agent's either way.
GAPPS_FRAMEWORK_OVERLAY_APK = "DsaFrameworkGappsOverlay.apk"
GAPPS_GEOCODER_PACKAGE = "com.google.android.gms"
GAPPS_FRAMEWORK_OVERLAY = {
    "bool/config_enableGeocoderOverlay": False,
    "string/config_geocoderProviderPackageName": GAPPS_GEOCODER_PACKAGE,
}
# The host window's title bar is Android's status bar (see SERVICES_CODE): many apps take the
# status bar's height from the framework's resources (getIdentifier("status_bar_height",
# "dimen", "android"), or SystemBarUtils' status_bar_height_default) rather than from their
# window's insets, and pad their own status bar background and toolbar by it, as they would on
# a phone. These resources are the title bar's height, so such an app also stays clear of it.
#
# The host draws the title bar at the height a standard window's caption and top border have
# at the monitor's DPI (STATUS_BAR_TITLE_HEIGHTS, read from AdjustWindowRectExForDpi on the
# Windows host with the default theme), and Android draws at 131 times the monitor's scale,
# rounded and made even (status_bar_density, as the host's density_for_dpi). The title bar is
# then 34.0 to 38.2 dp high depending on the scale (35.4 dp at 200%), so each density Android draws at on a
# monitor scale gets its own value, which getDimensionPixelSize rounds to the title bar's
# pixels exactly. Android picks the nearest density for a task that draws at another one (an
# app that keeps its density on a monitor of another scale), which differs from the title bar
# by at most a few dp. The values without a density are the highest, at a scale of 100%, so
# an app never pads less than the title bar.
STATUS_BAR_TITLE_HEIGHTS = {96: 31, 120: 38, 144: 45, 168: 52, 192: 58, 216: 65, 240: 72, 264: 79,
                            288: 85, 336: 99, 384: 112, 432: 126, 480: 139}
STATUS_BAR_DIMENS = ["status_bar_height", "status_bar_height_default", "status_bar_height_portrait",
                     "status_bar_height_landscape"]


def status_bar_density(dpi):
    """The density Android draws at on a monitor of `dpi`: 131 x the scale, rounded to the
    nearest dpi and then down to an even number."""
    return ((dpi * 131 + 48) // 96) & ~1


def status_bar_heights():
    """{density: dp}: the status bar height for each density Android draws at on a monitor
    scale, the title bar's height there in dp."""
    return {status_bar_density(dpi): px * BASE_DENSITY / status_bar_density(dpi)
            for dpi, px in STATUS_BAR_TITLE_HEIGHTS.items()}


def status_bar_overlay_files():
    """The framework overlay's status bar height values: {path: XML}, one values folder per
    density and the default one."""
    heights = status_bar_heights()

    def values(dp):
        return resources_xml([f'<dimen name="{name}">{dp:.4f}dp</dimen>' for name in STATUS_BAR_DIMENS])

    files = {"res/values/dsa_status_bar.xml": values(max(heights.values()))}
    for density, dp in sorted(heights.items()):
        files[f"res/values-{density}dpi/dsa_status_bar.xml"] = values(dp)
    return files


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

# The design of the screens Android draws itself: Settings, Files (DocumentsUI), the
# permission screens and dialogs (PermissionController), the install dialogs
# (PackageInstaller), the share sheet (IntentResolver), and the framework's dialogs and toasts.
# A large title at the top of a screen folds into the toolbar as the list scrolls, and starts
# folded in a window under 600 dp tall, where it would take a third of the height. Lists are
# grouped in rounded cards with the page showing between the groups, titles are bold, the
# accent is blue, both themes follow the same rules, and dialogs have rounded corners with
# their buttons side by side along the bottom.
#
# Static overlays carry it, one per app, each built with aapt2 against the image's own
# framework-res and checked against the app it overlays: every resource it replaces must be
# there, and the overlay records the resource tables it was built against
# (SYSTEM_OVERLAY_RECORD), which check-image.py compares with the image's. Overlays reference
# the framework's resources and the resources they replace; their own, such as the search
# icon, resolve in the overlay itself. The fonts are the image's own Roboto, at its bold
# weight, and every drawing is DSA's own.
DESIGN_CORNER_DP = 26
DESIGN_TALL_WINDOW_DP = 600
DESIGN_TITLE_AREA_DP = 200
DESIGN_LARGE_TITLE_DP = 36
DESIGN_TOOLBAR_TITLE_DP = 20
DESIGN_FONT = "sans-serif"
SYSTEM_OVERLAY_RECORD = "assets/dsa-overlay.json"
SYSTEM_OVERLAY_MAP = "dsa_overlay"
FRAMEWORK_RES = "SYSTEM/framework/framework-res.apk"
# Each overlay: its file in /vendor/overlay, its package, the package it overlays, where that
# package's APK is in target_files (a path suffix, or the permission APEX), and what is wrong
# with an image that lacks it.
SYSTEM_OVERLAYS = [
    {"apk": "DsaFrameworkOverlay.apk", "package": FRAMEWORK_OVERLAY_PACKAGE, "target": "android",
     "target_apk": FRAMEWORK_RES,
     "without": "a press on a list row shows only after the 100 ms tap timeout, and the framework's "
                "dialogs, toasts and colours keep Android's own look"},
    {"apk": "DsaSettingsOverlay.apk", "package": SETTINGS_OVERLAY_PACKAGE, "target": SETTINGS_OVERLAY_TARGET,
     "target_apk": "/priv-app/Settings/Settings.apk",
     "without": "Settings' back button shows its hover and ripple 8 dp left of its circle, the last row "
                "of a Settings list sits on the window's bottom edge, and Settings has no large title "
                "or rounded cards"},
    {"apk": "DsaDocumentsUiOverlay.apk", "package": "io.github.thisisthepy.dsa.overlay.documentsui",
     "target": "com.android.documentsui", "target_apk": "/priv-app/DocumentsUI/DocumentsUI.apk",
     "without": "Files keeps its old look instead of the rounded one"},
    {"apk": "DsaPermissionControllerOverlay.apk", "package": "io.github.thisisthepy.dsa.overlay.permissioncontroller",
     "target": "com.android.permissioncontroller", "target_apk": None,
     "without": "the permission dialog's title is not bold"},
    {"apk": "DsaPackageInstallerOverlay.apk", "package": "io.github.thisisthepy.dsa.overlay.packageinstaller",
     "target": "com.android.packageinstaller", "target_apk": "/priv-app/PackageInstaller/PackageInstaller.apk",
     "without": "the install dialog's title is not bold"},
    {"apk": "DsaIntentResolverOverlay.apk", "package": "io.github.thisisthepy.dsa.overlay.intentresolver",
     "target": "com.android.intentresolver", "target_apk": "/priv-app/IntentResolver/IntentResolver.apk",
     "without": "the share sheet's headline keeps its small size"},
    {"apk": "DsaFrameworkGappsOverlay.apk", "package": "io.github.thisisthepy.dsa.overlay.framework.gapps",
     "target": "android", "target_apk": FRAMEWORK_RES,
     # Above the framework overlay, whose geocoder values it replaces.
     "priority": 101, "required_property": ("ro.boot.dsa.gapps", "on"),
     "without": "with Google Play installed, apps' address lookups still go to DSA's geocoder instead of "
                "Google's"},
]
SYSTEM_OVERLAY_FILES = [overlay["apk"] for overlay in SYSTEM_OVERLAYS]
# The overlays built from a resource map (design_overlay_resources): the design's, and the
# Google apps overlay. The framework and Settings overlays are built by their own functions,
# which other changes made first.
DESIGN_OVERLAYS = SYSTEM_OVERLAY_FILES[2:]

# The blue accent: one tonal ramp (tone 0 is white, 1000 black) for the framework's first
# accent palette, which older apps and the DeviceDefault theme read, and the colour roles
# Material 3 apps read, in both themes. With no theme service in the guest (no SystemUI),
# the framework's own values are the colours every app gets.
DESIGN_BLUE_TONES = {
    0: "#FFFFFF", 10: "#FAFBFF", 50: "#EDF2FF", 100: "#D8E3FF", 200: "#ADC6FF", 300: "#7FA8FF",
    400: "#4F8AFF", 500: "#2B6EF2", 600: "#1A5AD7", 700: "#0D46AE", 800: "#053486", 900: "#001B4A",
    1000: "#000000",
}
# The page is a light grey (or black) and the cards on it white (or a dark grey), so the
# cards stand out; dialogs and sheets sit a step above the page.
DESIGN_COLORS = {
    **{f"system_accent1_{tone}_{theme}": value for tone, value in DESIGN_BLUE_TONES.items()
       for theme in ("light", "dark")},
    "system_primary_light": "#0B64D8",
    "system_on_primary_light": "#FFFFFF",
    "system_primary_container_light": "#D8E3FF",
    "system_on_primary_container_light": "#001B4A",
    "system_inverse_primary_light": "#ADC6FF",
    "system_surface_tint_light": "#0B64D8",
    "system_palette_key_color_primary_light": "#2B6EF2",
    "system_primary_dim_light": "#0D56BD",
    "system_primary_dark": "#6FA3FF",
    "system_on_primary_dark": "#002E6E",
    "system_primary_container_dark": "#0D46AE",
    "system_on_primary_container_dark": "#D8E3FF",
    "system_inverse_primary_dark": "#0B64D8",
    "system_surface_tint_dark": "#6FA3FF",
    "system_palette_key_color_primary_dark": "#2B6EF2",
    "system_primary_dim_dark": "#5B94F5",
    "system_primary_fixed": "#D8E3FF",
    "system_primary_fixed_dim": "#ADC6FF",
    "system_on_primary_fixed": "#001B4A",
    "system_on_primary_fixed_variant": "#0D46AE",
    "system_background_light": "#F2F2F5",
    "system_surface_light": "#F2F2F5",
    "system_surface_dim_light": "#DADADF",
    "system_surface_bright_light": "#FFFFFF",
    "system_surface_container_lowest_light": "#FFFFFF",
    "system_surface_container_low_light": "#F7F7FA",
    "system_surface_container_light": "#F2F2F5",
    "system_surface_container_high_light": "#FAFAFC",
    "system_surface_container_highest_light": "#E6E6EB",
    "system_background_dark": "#000000",
    "system_surface_dark": "#000000",
    "system_surface_dim_dark": "#000000",
    "system_surface_bright_dark": "#1C1C1F",
    "system_surface_container_lowest_dark": "#000000",
    "system_surface_container_low_dark": "#0B0B0D",
    "system_surface_container_dark": "#000000",
    "system_surface_container_high_dark": "#242428",
    "system_surface_container_highest_dark": "#2C2C31",
}
# Rounded dialogs and sheets, with the cards' radius.
DESIGN_FRAMEWORK_DIMENS = {
    "config_dialogCornerRadius": f"{DESIGN_CORNER_DP}dp",
    "config_bottomDialogCornerRadius": f"{DESIGN_CORNER_DP}dp",
}
# Bold titles: dialog titles, window titles (the share sheet's among them) and toolbar
# titles of the apps that use the DeviceDefault theme. Each keeps its own parent.
DESIGN_FRAMEWORK_TITLES = {
    "TextAppearance.DeviceDefault.DialogWindowTitle": "TextAppearance.Material.DialogWindowTitle",
    "TextAppearance.DeviceDefault.WindowTitle": "TextAppearance.Material.WindowTitle",
    "TextAppearance.DeviceDefault.Widget.ActionBar.Title": "TextAppearance.Material.Widget.ActionBar.Title",
}
# A dialog's buttons, side by side across its bottom, each as wide as the others' share; a
# dialog whose buttons cannot fit still stacks them, as ButtonBarLayout does.
DIALOG_BUTTON_BAR_LAYOUT = "alert_dialog_button_bar_material"
# A toast of an app that draws its own (one that targets Android 10 or earlier): a pill on
# the theme's background, the colour the framework's own toast frame has in every DeviceDefault
# theme, rather than a square.
TOAST_LAYOUT = "transient_notification"
TOAST_FRAME = "toast_frame"
FRAMEWORK_DESIGN_OVERLAID = (
    [f"color/{name}" for name in DESIGN_COLORS]
    + [f"dimen/{name}" for name in DESIGN_FRAMEWORK_DIMENS]
    + [f"style/{name}" for name in DESIGN_FRAMEWORK_TITLES]
    + [f"layout/{DIALOG_BUTTON_BAR_LAYOUT}", f"layout/{TOAST_LAYOUT}", f"drawable/{TOAST_FRAME}"]
)

# Settings: SettingsLib's group backgrounds round the outer corners of a group by
# settingslib_preference_corner_radius and the inner ones by 4 dp, so every row is a card of
# its own. Here the inner corners are square, and a group reads as one card; the 2 dp
# SettingsLib keeps below each row shows the page through as the line between rows.
SETTINGS_CARDS = {
    "settingslib_round_background": (True, True),
    "settingslib_round_background_top": (True, False),
    "settingslib_round_background_center": (False, False),
    "settingslib_round_background_bottom": (False, True),
}
# The large title of a SettingsLib page opens expanded in a window 600 dp tall or more and
# folded below it. SettingsLib's app bar takes the theme overlay SETTINGS_TITLE_THEME (an
# empty style over Material's theme); the overlay's version of it says whether the app bar
# opens expanded, in two configurations, and the app bar's layout reads that from the theme
# (SETTINGS_TITLE_EXPANDED). The style keeps Material's theme as its parent: the overlay names
# that style through a stand-in kept only for watches, which never matches, so the reference
# resolves to Settings' own style.
SETTINGS_TITLE_THEME = "SettingsLibTheme.CollapsingToolbar.Expressive"
SETTINGS_TITLE_THEME_PARENT = "Theme.MaterialComponents.DayNight"
SETTINGS_TITLE_EXPANDS = {"values-v36": False, f"values-h{DESIGN_TALL_WINDOW_DP}dp-v36": True}
SETTINGS_TITLE_STYLES = {
    "SettingsLibCollapsingToolbarTitle.Expanded": ("DsaLargeTitle", DESIGN_LARGE_TITLE_DP),
    "SettingsLibCollapsingToolbarTitle.Collapsed": ("DsaToolbarTitle", DESIGN_TOOLBAR_TITLE_DP),
}
# Settings' home page has no title: its app bar holds only the search bar. The overlay's
# layout puts the large title above the search bar in windows 600 dp tall or more, where it
# scrolls away and leaves the search bar as the toolbar. The ids are Settings' own, which the
# home page's code finds its views by; the search bar is drawn here, so the layout includes
# nothing of Settings'.
SETTINGS_HOME_LAYOUT = "settings_homepage_container"
SETTINGS_HOME_IDS = [
    "settings_homepage_container", "main_content_scrollable_container", "homepage_container",
    "suggestion_content", "contextual_cards_content", "main_content", "app_bar", "app_bar_container",
    "search_action_bar", "search_bar_title",
]
# Material's attributes the home layout uses, as Settings defines them.
SETTINGS_HOME_ATTRS = {
    "layout_behavior": '<attr name="layout_behavior" format="string" />',
    "layout_scrollFlags": (
        '<attr name="layout_scrollFlags">'
        '<flag name="noScroll" value="0x0" /><flag name="scroll" value="0x1" />'
        '<flag name="exitUntilCollapsed" value="0x2" /><flag name="enterAlways" value="0x4" />'
        '<flag name="enterAlwaysCollapsed" value="0x8" /><flag name="snap" value="0x10" />'
        '<flag name="snapMargins" value="0x20" /></attr>'),
}
SETTINGS_SEARCH_ICON = "dsa_search"

# Files: DocumentsUI's Material 3 look (rounded containers and the colour roles), which the
# app offers to devices through an overlayable switch and otherwise keeps for a developer
# option. Its layouts are not overlayable, so it gets no large title.
DOCUMENTSUI_DESIGN = {"bool/force_material3": "true"}
# PermissionController lets an overlay replace only what it declares overlayable: the
# permission dialog's title is, its list backgrounds are not (PERMISSION_CONTROLLER_CARDS).
PERMISSION_TITLE_STYLE = "PermissionGrantTitleMessageExpressive"
# PackageInstaller's dialog titles, for its DeviceDefault and Material 3 themes.
PACKAGE_INSTALLER_TITLES = {
    "TextAppearance.PackageInstaller.Title": ("DsaInstallerTitle", None),
    "TextAppearance.PackageInstaller.Material.Title": ("DsaInstallerMaterialTitle", 24),
}
# The share sheet's headline; its title is the framework's window title (bold above) and its
# sheet's corners the framework's bottom sheet radius.
INTENT_RESOLVER_DESIGN = {"dimen/chooser_headline_text_size": "20sp"}

# SettingsLib opens every page's large title folded: the app bar of its collapsing toolbar
# says app:expanded="false" in the layout itself, in every window. An overlay cannot change
# that attribute: it belongs to the layout, not to a style or value an overlay could replace,
# and the layout includes other SettingsLib layouts, which an overlay can name only by
# replacing them too. The compiled layouts in Settings.apk take the value from the app bar's
# theme instead (?attr/expanded), which the Settings overlay sets by the window's height
# (SETTINGS_TITLE_EXPANDS).
APP_BAR_LAYOUT = "com.google.android.material.appbar.AppBarLayout"
SETTINGS_TITLE_EXPANDED = {
    "res/layout-v36/settingslib_expressive_collapsing_toolbar_content_layout.xml": [(APP_BAR_LAYOUT, "expanded")],
    "res/layout-v36/settingslib_expressive_support_toolbar_layout.xml": [(APP_BAR_LAYOUT, "expanded")],
}
# PermissionController's lists are SettingsLib's, but the app declares <overlayable> and its
# list backgrounds are not in it, so idmap2 refuses any overlay of them. Their compiled
# drawables in the APK get the corners Settings' cards have instead: outer corners of
# DESIGN_CORNER_DP, square inner ones.
ANDROID_RADIUS = 0x010101A8
ANDROID_TOP_LEFT_RADIUS = 0x010101A9
ANDROID_TOP_RIGHT_RADIUS = 0x010101AA
ANDROID_BOTTOM_LEFT_RADIUS = 0x010101AB
ANDROID_BOTTOM_RIGHT_RADIUS = 0x010101AC


def card_corner_edits(top, bottom, single):
    """The corner edits of one of SettingsLib's group backgrounds: a whole-card one names one
    radius, the others the four corners."""
    if single:
        return [("corners", ANDROID_RADIUS, DESIGN_CORNER_DP if top else 0)]
    top_dp, bottom_dp = (DESIGN_CORNER_DP if top else 0), (DESIGN_CORNER_DP if bottom else 0)
    return [("corners", ANDROID_TOP_LEFT_RADIUS, top_dp), ("corners", ANDROID_TOP_RIGHT_RADIUS, top_dp),
            ("corners", ANDROID_BOTTOM_LEFT_RADIUS, bottom_dp), ("corners", ANDROID_BOTTOM_RIGHT_RADIUS, bottom_dp)]


# The center and single backgrounds name one radius for all four corners; the top and bottom
# ones name each corner.
PERMISSION_CONTROLLER_CARDS = {
    f"res/drawable-v36/{name}.xml": card_corner_edits(top, bottom, single=name in (
        "settingslib_round_background", "settingslib_round_background_center"))
    for name, (top, bottom) in SETTINGS_CARDS.items()
}

SYSTEM_EXT_FS_CONFIG = "META/system_ext_filesystem_config.txt"
AGENT_FS_CONFIG = [
    "system_ext/priv-app/DsaAgent 0 0 755 capabilities=0x0",
    "system_ext/priv-app/DsaAgent/DsaAgent.apk 0 0 644 capabilities=0x0",
    "system_ext/etc/permissions/privapp-permissions-dsa-agent.xml 0 0 644 capabilities=0x0",
    "system_ext/etc/default-permissions 0 0 755 capabilities=0x0",
    "system_ext/etc/default-permissions/default-permissions-dsa-agent.xml 0 0 644 capabilities=0x0",
    "system_ext/etc/sysconfig 0 0 755 capabilities=0x0",
    "system_ext/etc/sysconfig/sysconfig-dsa-agent.xml 0 0 644 capabilities=0x0",
]


def update_vendor_file_contexts(fc_text, channeld=False, hwcomposer=False, power=False, codec2=False,
                                audio=False, gnss=False, sensors=False,
                                camera=False, keymint=False):
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
        # The elevation helper's sockets: init gives a socket file the label its path has here.
        entries += SU_SOCKET_CONTEXTS
        # KeyMint's root key, readable only by KeyMint.
        entries.append((f"{KEYMINT_KEY_DIR}(", KEYMINT_KEY_CONTEXT))
        # The host's root certificates, which the daemon writes and every domain reads.
        entries.append((f"{CACERTS_DIR}(", CACERTS_CONTEXT))
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
    if keymint:
        entries.append((r"/vendor/bin/hw/dsa-keymint", KEYMINT_CONTEXT))
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
    for name, context in WRITEBACK_PROPERTY_CONTEXTS.items():
        if not any(line.split()[:1] == [name] for line in lines):
            lines.append(context)
    if not any(line.split()[:1] == [BOOT_PROPERTY] for line in lines):
        lines.append(BOOT_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [ADB_PROPERTY] for line in lines):
        lines.append(ADB_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [SHARES_PROPERTY] for line in lines):
        lines.append(SHARES_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [FILES_PROPERTY] for line in lines):
        lines.append(FILES_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [ROOT_ELEVATION_PROPERTY] for line in lines):
        lines.append(ROOT_ELEVATION_PROPERTY_CONTEXT)
    if not any(line.split()[:1] == [DNS_PROPERTY] for line in lines):
        lines.append(DNS_PROPERTY_CONTEXT)
    for name, context in GAPPS_COMPILE_PROPERTY_CONTEXTS.items():
        if not any(line.split()[:1] == [name] for line in lines):
            lines.append(context)
    return "\n".join(lines) + "\n"


def with_zram_backing_context(fc_text):
    """Labels zram's backing folder (ZRAM_BACKING_CONTEXT) in plat_file_contexts. A line the
    build already has for the same path goes: a second specification for it would conflict."""
    lines = [line for line in fc_text.splitlines() if line.split()[:1] != [ZRAM_BACKING_PATH]]
    lines.append(ZRAM_BACKING_CONTEXT)
    return "\n".join(lines) + "\n"


def update_plat_file_contexts(fc_text):
    """Labels the su client dsa_su_exec in plat_file_contexts. A line the build already has
    for the same path goes: a second specification for it would conflict."""
    lines = [line for line in fc_text.splitlines() if line.split()[:1] != [SU_CLIENT_PATH]]
    lines.append(SU_CLIENT_CONTEXT)
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
                                    camera=False, keymint=False):
    """Adds filesystem configuration entries for the files DSA adds to vendor if not present."""
    lines = fs_config_text.splitlines()
    entries = [
        ("vendor/lib64/hw/vulkan.ranchu.so", "vendor/lib64/hw/vulkan.ranchu.so 0 0 644 capabilities=0x0"),
        ("vendor/lib64/libOpenglSystemCommon.so", "vendor/lib64/libOpenglSystemCommon.so 0 0 644 capabilities=0x0"),
        ("vendor/etc/init/dsa-memory.rc", "vendor/etc/init/dsa-memory.rc 0 0 644 capabilities=0x0"),
        ("vendor/etc/init/dsa-display.rc", "vendor/etc/init/dsa-display.rc 0 0 644 capabilities=0x0"),
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
    if keymint:
        for name in KEYMINT_FILES + [KEYMINT_RC]:
            path = "vendor/" + name.split("/", 1)[1]
            mode = "0 2000 755" if "/bin/" in path else "0 0 644"
            entries.append((path, f"{path} {mode} capabilities=0x0"))
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
ADDONS_POLICY = HERE / "sepolicy/addons.cil"
GPU_RESET_POLICY = HERE / "sepolicy/gpu-reset.cil"
SU_POLICY = HERE / "sepolicy/su.cil"
CACERTS_POLICY = HERE / "sepolicy/cacerts.cil"

# What root elevation needs of the policy, each with what goes wrong without it.
ROOT_ELEVATION_RULES = {
    "(allow untrusted_app_all dsa_su_exec (file (read open getattr map execute execute_no_trans)))":
        "apps cannot run the su client",
    "(allow untrusted_app_all dsa_su_socket (sock_file (write)))":
        "apps cannot reach the elevation helper's socket",
    "(allow untrusted_app_all dsa_su (unix_stream_socket (connectto)))":
        "apps cannot connect to the elevation helper",
    "(allow shell dsa_su_exec (file (read open getattr map execute execute_no_trans)))":
        "adb shell cannot run the su client",
    "(allow shell dsa_su_socket (sock_file (write)))":
        "adb shell cannot reach the elevation helper's socket",
    "(allow shell dsa_su (unix_stream_socket (connectto)))":
        "adb shell cannot connect to the elevation helper",
    "(typeattributeset mlstrustedsubject (dsa_su))":
        "the elevation helper is not mlstrustedsubject, so an app, which runs at an MLS level of its own, "
        "cannot connect to it",
    "(typeattributeset mlstrustedobject (dsa_su_socket))":
        "the elevation helper's socket is not mlstrustedobject, so an app cannot write it",
    "(allow dsa_channeld dsa_su (unix_stream_socket (connectto)))":
        "dsa-channeld cannot reach the elevation helper, so no su call reaches the host",
    "(allow dsa_channeld dsa_root_elevation_prop (property_service (set)))":
        "dsa-channeld cannot start the elevation helper",
    '(genfscon securityfs "/dsa_su" (u object_r dsa_su_control_file ((s0) (s0))))':
        "the su guard's control file is not labelled, so nothing may write it",
    "(allow init dsa_su_control_file (file (write open getattr)))":
        "init cannot hide su at boot, so every app sees it",
    "(allow dsa_su dsa_su_control_file (file (write open getattr)))":
        "the elevation helper cannot tell the su guard which apps may see su",
    "(typetransition dsa_su shell_exec process dsa_su_root)":
        "an approved command does not run in the root domain",
}


def root_elevation_policy_problems(plat_sepolicy_text):
    """Returns what is wrong with root elevation's policy in a platform policy that has the
    daemon: a rule of ROOT_ELEVATION_RULES it lacks, or a domain other than the one it
    belongs to that may enter the root domain, write the su guard or start the helper."""
    problems = [f"the SELinux policy lacks {rule}: {why}"
                for rule, why in ROOT_ELEVATION_RULES.items() if rule not in plat_sepolicy_text]
    # The approved command's domain is permissive, so the helper alone may lead into it.
    entries = re.findall(r"\(typetransition\s+(\S+)\s+\S+\s+process\s+dsa_su_root\s*\)", plat_sepolicy_text)
    entries += re.findall(r"\(allow\s+(\S+)\s+dsa_su_root\s+\(\s*process\s+\([^)]*\b(?:dyn)?transition\b",
                          plat_sepolicy_text)
    for source in sorted(set(entries) - {"dsa_su"}):
        problems.append(f"{source} may enter dsa_su_root, the approved command's root domain, which only the "
                        "elevation helper may")
    writers = re.findall(r"\(allow\s+(\S+)\s+dsa_su_control_file\s+\(\s*file\s+\([^)]*\b(?:write|append)\b",
                         plat_sepolicy_text)
    for subject in sorted(set(writers) - {"init", "dsa_su"}):
        problems.append(f"{subject} may write the su guard, which decides which apps see su")
    setters = re.findall(r"\(allow\s+(\S+)\s+dsa_root_elevation_prop\s+\(\s*property_service\s+\([^)]*\bset\b",
                         plat_sepolicy_text)
    for subject in sorted(set(setters) - {"dsa_channeld"}):
        problems.append(f"{subject} may start the elevation helper, which only dsa-channeld may")
    return problems


def root_elevation_problems(names, read):
    """Returns what an image with the daemon lacks for root elevation: its policy
    (root_elevation_policy_problems), the su client's label in the file contexts the system
    image is built from, the property that starts the helper, and the init script that hides su
    at boot and runs the helper with its sockets."""
    problems = []
    if SU_CLIENT_BINARY not in names:
        problems.append(f"{SU_CLIENT_BINARY} is missing, so apps that may elevate find no su client")
    if AOSP_XBIN_SU_BINARY in names:
        problems.append(f"{AOSP_XBIN_SU_BINARY} is in the image, so /system/xbin/su is not an absent file")
    if "SYSTEM/etc/selinux/plat_sepolicy.cil" in names:
        problems += root_elevation_policy_problems(read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode())
    if "META/file_contexts.bin" in names and b"dsa_su_exec" not in read("META/file_contexts.bin"):
        problems.append(f"META/file_contexts.bin does not label {SU_CLIENT_PATH} dsa_su_exec, so the system "
                        "image leaves the su client a file apps may not run")
    if PLAT_FILE_CONTEXTS in names and SU_CLIENT_CONTEXT not in read(PLAT_FILE_CONTEXTS).decode().splitlines():
        problems.append(f"{PLAT_FILE_CONTEXTS} does not label {SU_CLIENT_PATH} dsa_su_exec, so a restorecon "
                        "gives the su client another label")
    if VENDOR_PROPERTY_CONTEXTS in names and ROOT_ELEVATION_PROPERTY_CONTEXT not in read(
            VENDOR_PROPERTY_CONTEXTS).decode().splitlines():
        problems.append(f"vendor_property_contexts does not give {ROOT_ELEVATION_PROPERTY} its type, so "
                        "dsa-channeld cannot start the elevation helper")
    rc = read(CHANNELD_RC).decode() if CHANNELD_RC in names else ""
    for line, why in [
        (SU_HELPER_SERVICE, "init never starts the elevation helper"),
        ("seclabel u:r:dsa_su:s0", "the elevation helper does not run in its own domain"),
        ("socket dsa_su stream 0666 root root", "su has no socket to reach the helper on"),
        ("socket dsa_su_relay seqpacket 0660 root system", "dsa-channeld has no socket to reach the helper on"),
    ]:
        if line not in rc.splitlines() and f"    {line}" not in rc.splitlines():
            problems.append(f"{CHANNELD_RC} lacks \"{line}\", so {why}")
    guard = read(SU_GUARD_RC).decode() if SU_GUARD_RC in names else ""
    for line in ['write /sys/kernel/security/dsa_su/control "path /system/bin/su"',
                 "write /sys/kernel/security/dsa_su/control enable"]:
        if f"    {line}" not in guard.splitlines():
            problems.append(f"{SU_GUARD_RC} lacks \"{line}\", so su is not hidden at boot")
    if SYSTEM_FS_CONFIG in names:
        fs_config = read(SYSTEM_FS_CONFIG).decode().splitlines()
        xbin_rules = [line for line in fs_config if line.split()[:1] == ["system/xbin"]]
        if xbin_rules != ["system/xbin 0 2000 755 capabilities=0x0"]:
            problems.append(f"{SYSTEM_FS_CONFIG} does not make system/xbin mode 755, so looking up "
                            "/system/xbin/su returns EACCES instead of ENOENT")
        if any(line.split()[:1] == ["system/xbin/su"] for line in fs_config):
            problems.append(f"{SYSTEM_FS_CONFIG} still lists system/xbin/su")
    contexts = (read("VENDOR/etc/selinux/vendor_file_contexts").decode().splitlines()
                if "VENDOR/etc/selinux/vendor_file_contexts" in names else [])
    for _, context in SU_SOCKET_CONTEXTS:
        if context not in contexts:
            problems.append(f"vendor_file_contexts lacks \"{context}\", so init makes the socket with a label "
                            "its clients may not write")
    return problems


def dns_problems(names, read):
    """Returns what an image with the daemon lacks for the guest's DNS to be answered by the
    host's resolver: the redirect service that sends DNS queries to the daemon's stub, as root
    in its own domain and started whenever the daemon switches it, and the type of the property
    that switches it. A trigger of the vendor script on a service's state (init.svc.*) is named
    too: vendor_init may not read those properties, so init drops the whole action at boot."""
    problems = []
    rc = read(CHANNELD_RC).decode().splitlines() if CHANNELD_RC in names else []
    for line, why in [
        (DNS_REDIRECT_SERVICE, "the guest's DNS queries never reach the host's resolver"),
        ("    user root", "the packet filter cannot be changed"),
        ("    seclabel u:r:dsa_dns_redirect:s0", "the redirect runs outside its own domain"),
        (DNS_REDIRECT_TRIGGER, "nothing starts the redirect when the daemon switches it"),
    ]:
        if line not in rc:
            problems.append(f"{CHANNELD_RC} lacks \"{line.strip()}\", so {why}")
    for line in rc:
        if line.startswith("on ") and "property:init.svc." in line:
            problems.append(f"{CHANNELD_RC} has \"{line}\", which init drops as an unexported property "
                            "trigger, so nothing it lists ever runs")
    if VENDOR_PROPERTY_CONTEXTS in names and DNS_PROPERTY_CONTEXT not in read(
            VENDOR_PROPERTY_CONTEXTS).decode().splitlines():
        problems.append(f"vendor_property_contexts does not give {DNS_PROPERTY} its type, so the daemon "
                        "cannot switch the guest's DNS to the host's resolver")
    return problems


def keymint_key_problems(names, read):
    """Returns what an image with the daemon lacks for KeyMint's root key from the host: the
    service that fetches it, in its own domain as KeyMint's user, the system script that makes
    its tmpfs folder and starts it before KeyMint, the folder's label, and the policy that lets
    only KeyMint read the key."""
    problems = []
    rc = read(CHANNELD_RC).decode().splitlines() if CHANNELD_RC in names else []
    for line, why in [
        (KEYMINT_KEY_SERVICE, "nothing fetches KeyMint's root key from the host"),
        ("    user nobody", "the key file is not KeyMint's user's"),
        ("    seclabel u:r:dsa_keymint_key:s0", "the key is fetched outside its own domain"),
        ("    oneshot", "init would ask the host for the key again, which it refuses"),
    ]:
        if line not in rc:
            problems.append(f"{CHANNELD_RC} lacks \"{line.strip()}\", so {why}")
    if KEYMINT_KEY_RC not in names:
        problems.append(f"{KEYMINT_KEY_RC} is missing, so KeyMint's root key has no folder and is never fetched")
    else:
        script = read(KEYMINT_KEY_RC).decode().splitlines()
        for line in ["on init", f"    mkdir {KEYMINT_KEY_DIR} 0700 nobody nobody", "    start dsa_keymint_key"]:
            if line not in script:
                problems.append(f"{KEYMINT_KEY_RC} lacks \"{line.strip()}\", so the key is not there "
                                "before KeyMint starts")
    contexts = (read("VENDOR/etc/selinux/vendor_file_contexts").decode().splitlines()
                if "VENDOR/etc/selinux/vendor_file_contexts" in names else [])
    if KEYMINT_KEY_CONTEXT not in contexts:
        problems.append(f"vendor_file_contexts lacks \"{KEYMINT_KEY_CONTEXT}\", so the key's folder has a "
                        "label other domains may read")
    if "SYSTEM/etc/selinux/plat_sepolicy.cil" in names:
        policy = read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        for rule, why in [
            ("(typeattributeset domain (dsa_keymint_key))", "the key is fetched in no domain of its own"),
            ("(allow hal_keymint_server dsa_keymint_key_file (file (read open getattr)))",
             "KeyMint cannot read its key"),
        ]:
            if rule not in policy:
                problems.append(f"the SELinux policy lacks {rule}, so {why}")
        readers = re.findall(r"\(allow\s+(\S+)\s+dsa_keymint_key_file\s+\(\s*file\s+\(([^)]*)\)", policy)
        others = sorted({who for who, perms in readers if "read" in perms.split() and who != "hal_keymint_server"})
        if others:
            problems.append(f"the SELinux policy lets {', '.join(others)} read KeyMint's root key")
    return problems


def cacerts_problems(names, read):
    """Returns what an image with the daemon lacks for the host's root certificates among
    Android's system certificates: the setup service, the system script that makes the tmpfs
    folder, runs the setup and binds the copies over Android's folders, the folder's label, and
    the policy that lets the daemon write there and every domain read it."""
    problems = []
    rc = read(CHANNELD_RC).decode() if CHANNELD_RC in names else ""
    service = rc[rc.find(CACERTS_SERVICE):].split("\n\n", 1)[0].splitlines() if CACERTS_SERVICE in rc else []
    if not service:
        problems.append(f"{CHANNELD_RC} lacks \"{CACERTS_SERVICE}\", so nothing copies Android's certificates "
                        "into the folders the host's roots go in")
    for line, why in [
        ("    user system", "the daemon, which runs as system, cannot write the copies"),
        ("    oneshot", "init would copy the certificates again whenever the setup exits"),
    ]:
        if service and line not in service:
            problems.append(f"{CHANNELD_RC} lacks \"{line.strip()}\" for the setup, so {why}")
    if CACERTS_RC not in names:
        problems.append(f"{CACERTS_RC} is missing, so Android never trusts the root certificates the host added")
    else:
        script = read(CACERTS_RC).decode().splitlines()
        for line in CACERTS_SCRIPT:
            if line not in script:
                problems.append(f"{CACERTS_RC} lacks \"{line.strip()}\", so the host's root certificates do not "
                                "reach Android's system certificates before the zygote starts")
    contexts = (read("VENDOR/etc/selinux/vendor_file_contexts").decode().splitlines()
                if "VENDOR/etc/selinux/vendor_file_contexts" in names else [])
    if CACERTS_CONTEXT not in contexts:
        problems.append(f"vendor_file_contexts lacks \"{CACERTS_CONTEXT}\", so the certificate folders have a "
                        "label apps may not read and the daemon may not write")
    if "SYSTEM/etc/selinux/plat_sepolicy.cil" in names:
        policy = read("SYSTEM/etc/selinux/plat_sepolicy.cil").decode()
        for rule, why in CACERTS_RULES.items():
            if rule not in policy:
                problems.append(f"the SELinux policy lacks {rule}, so {why}")
        rules = re.findall(r"\(allow\s+(\S+)\s+dsa_cacerts_file\s+\(\s*file\s+\(([^)]*)\)", policy)
        others = sorted({who for who, perms in rules
                         if {"write", "create", "append"} & set(perms.split()) and who != "dsa_channeld"})
        if others:
            problems.append(f"the SELinux policy lets {', '.join(others)} write Android's system certificates")
    return problems


# What the host's root certificates need of the policy, each with what goes wrong without it.
CACERTS_RULES = {
    "(allow dsa_channeld dsa_cacerts_file (file (create read write open getattr setattr rename unlink)))":
        "the daemon cannot add or take away the host's root certificates",
    "(allow domain dsa_cacerts_file (file (read open getattr map)))":
        "apps cannot read Android's system certificates once the copies are bound over them",
    "(allow domain dsa_cacerts_file (dir (search getattr read open)))":
        "apps cannot look a certificate up in the bound folders",
    "(allow init dsa_cacerts_file (dir (create search getattr setattr open read write add_name relabelto mounton)))":
        "init cannot make the folder or bind it",
    "(allow init system_security_cacerts_file (dir (mounton)))":
        "init cannot bind the copies over Android's certificate folders",
}


# Shared memory is a memfd in the guest (memory.cil), labelled with the tmpfs type of the
# domain that made it. Each pair is a domain and a type of memfd it must be able to map, read
# and write, because the type's owner hands it such memfds over binder; a refused one fails
# the whole binder call that carried it. Android's own policy has these rules only for its
# memfd_file class, or none, because on a phone the memory comes from /dev/ashmem.
MEMFD_SHARING = [
    # An audio track's or a recording's control block, in the app and in the player service.
    ("appdomain", "audioserver_tmpfs"),
    ("mediaserver", "audioserver_tmpfs"),
    ("system_server", "audioserver_tmpfs"),
    # The shared buffer of a static track (SoundPool, the click sounds).
    ("audioserver", "appdomain_tmpfs"),
    ("audioserver", "system_server_tmpfs"),
    # The buffers of a remote media extractor and of the media player service.
    ("appdomain", "mediaextractor_tmpfs"),
    ("appdomain", "mediaserver_tmpfs"),
    ("mediaserver", "mediaextractor_tmpfs"),
    # The camera service's metadata queues.
    ("appdomain", "cameraserver_tmpfs"),
    # The audio HAL's end of audioserver's and system_server's queues.
    ("hal_audio_server", "audioserver_tmpfs"),
    ("hal_audio_server", "system_server_tmpfs"),
    # The codec services' buffer pool queues, in every codec client.
    ("hal_codec2_client", "dsa_codec2_tmpfs"),
    ("hal_codec2_server", "dsa_codec2_tmpfs"),
]
# The memfd types that reach apps. An app runs at an MLS level of its own, above the s0 the
# services run at, and may write a file below its level only when the file's type is
# mlstrustedobject; no allow rule lifts that constraint.
MEMFD_TO_APPS = ["audioserver_tmpfs", "mediaextractor_tmpfs", "mediaserver_tmpfs", "cameraserver_tmpfs",
                 "system_server_tmpfs", "dsa_codec2_tmpfs"]
MEMFD_PERMS = {"read", "write", "getattr", "map"}
# The sensors HAL's, when the image has DSA's (sensors.cil): the event queue and the wake lock
# queue the framework's sensor service makes and hands it in ISensors.initialize, and an app's
# direct channel.
MEMFD_SHARING_SENSORS = [
    ("hal_sensors_server", "system_server_tmpfs"),
    ("hal_sensors_server", "appdomain_tmpfs"),
]


def memfd_sharing_problems(plat_sepolicy_text, sharing=None, to_apps=None):
    """Returns what the policy refuses of the memfd sharing DSA's guest needs: a pair of
    MEMFD_SHARING without read, write, getattr and map on files, or a type of MEMFD_TO_APPS
    that is not mlstrustedobject."""
    sharing = MEMFD_SHARING if sharing is None else sharing
    to_apps = MEMFD_TO_APPS if to_apps is None else to_apps
    granted = {}
    for subject, target, perms in re.findall(r"\(allow\s+(\S+)\s+(\S+)\s+\(\s*file\s+\(([^)]*)\)\s*\)\s*\)",
                                              plat_sepolicy_text):
        granted.setdefault((subject, target), set()).update(perms.split())
    trusted = set()
    for members in re.findall(r"\(typeattributeset\s+mlstrustedobject\s+\(([^()]*)\)\s*\)", plat_sepolicy_text):
        trusted.update(members.split())
    problems = []
    for subject, target in sharing:
        missing = MEMFD_PERMS - granted.get((subject, target), set())
        if missing:
            problems.append(f"the SELinux policy does not let {subject} {' '.join(sorted(missing))} the "
                            f"memfds labelled {target}, so the binder calls that carry them fail")
    for target in to_apps:
        if target not in trusted:
            problems.append(f"{target} is not mlstrustedobject, so an app, which runs at an MLS level of its "
                            "own, cannot write the memfds it labels and every binder call that carries one fails")
    return problems


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
           sensors=None, camera=None, arm_translation=None, framework=None, overlays=None, su_client=None,
           arch=None):
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
    in place of the build's own when `framework` maps their target_files names to their bytes,
    and the other system screen overlays (DESIGN_OVERLAYS) that `overlays` maps from their file
    names to their APKs. An image for `arch` x86_64 (the default) gets the empty folders, the
    empty file and the links a translation package is reached through; every image gets the
    folders the Google apps are laid over that the build lacks."""
    fstab = (HERE / "fstab.dsa").read_bytes()
    default_translator = (Path(arm_translation) / ARM_TRANSLATION_IMAGE).read_bytes() if arm_translation else None
    graphics_payload = Path(graphics_library).read_bytes() if graphics_library else None
    vulkan_payload = Path(vulkan_driver).read_bytes() if vulkan_driver else None
    channeld_payload = Path(channeld).read_bytes() if channeld else None
    su_payload = Path(su_client).read_bytes() if su_client else None
    if su_payload is not None and channeld_payload is None:
        raise ValueError("the su client requires the control channel daemon")
    agent_payload = Path(agent).read_bytes() if agent else None
    hwcomposer_payload = Path(hwcomposer).read_bytes() if hwcomposer else None
    overlay_payload = Path(framework_overlay).read_bytes() if framework_overlay else None
    settings_overlay_payload = Path(settings_overlay).read_bytes() if settings_overlay else None
    design_payloads = {name: Path(apk).read_bytes() for name, apk in (overlays or {}).items()}
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
    # DSA's KeyMint, when the framework build has it, in place of the KeyMint APEX.
    has_keymint = framework is not None and framework_has_keymint(framework)
    build_tag = image_build_tag([
        graphics_payload, vulkan_payload, channeld_payload, agent_payload, hwcomposer_payload,
        overlay_payload, settings_overlay_payload, *design_payloads.values(),
        *(power_payloads or {}).values(), *(codec2_payloads or {}).values(),
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

    if has_keymint:
        with zipfile.ZipFile(source) as z:
            problems = keymint_library_problems(framework[KEYMINT_BINARY], set(z.namelist()) | set(framework))
        if problems:
            raise ValueError("; ".join(problems))

    with zipfile.ZipFile(source) as zin, zipfile.ZipFile(dest, "w", zipfile.ZIP_STORED, allowZip64=True) as zout:
        names = set(zin.namelist())
        # Where a translation package is bound (x86_64 only), and the folders the Google apps
        # are laid over that the build lacks: both added empty, with their file modes.
        reach = (arch or DEFAULT_ARCH) == "x86_64"
        if reach:
            conflicts = arm_translation_reach_conflicts(names)
            if conflicts:
                raise ValueError("; ".join(conflicts))
        added_gapps_folders = missing_folders(names, gapps_folders())
        added_fs_config = {}
        if reach:
            for config, rules in ARM_TRANSLATION_REACH_FS_CONFIG.items():
                added_fs_config.setdefault(config, []).extend(rules)
        for folder in added_gapps_folders:
            config = GAPPS_PARTITION_FS_CONFIG[folder.split("/", 1)[0]]
            added_fs_config.setdefault(config, []).append(f"{folder.lower()} 0 0 755 capabilities=0x0")
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
            # init mounting the user's add-on disk, which those images are on.
            plat_patched += "\n" + ADDONS_POLICY.read_text()
            if framework is not None:
                # SurfaceFlinger telling the apps about a GPU reset.
                plat_patched += "\n" + GPU_RESET_POLICY.read_text()
            if channeld_payload is not None:
                # Its rules name the domain they declare, so they go in whole.
                plat_patched += "\n" + CHANNELD_POLICY.read_text()
                # The daemon's file sharing jobs, which name its program's label.
                plat_patched += "\n" + FILES_POLICY.read_text()
                # Root elevation: the su client, and the elevation helper, which is the
                # daemon's program in a mode of its own, with its sockets.
                plat_patched += "\n" + SU_POLICY.read_text()
                # The host's root certificates among Android's system certificates.
                plat_patched += "\n" + CACERTS_POLICY.read_text()
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
            if name == PLAT_FILE_CONTEXTS:
                text = with_zram_backing_context(text)
            if name == PLAT_FILE_CONTEXTS and channeld_payload is not None:
                text = update_plat_file_contexts(text)
            if name == "VENDOR/etc/selinux/vendor_file_contexts":
                text = update_vendor_file_contexts(text, channeld_payload is not None, has_hwcomposer,
                                                   has_power, codec2=has_codec2, audio=has_audio, gnss=has_gnss, sensors=has_sensors, camera=has_camera, keymint=has_keymint)
            file_contexts.append(text if text.endswith("\n") or not text else text + "\n")
        compiled_file_contexts = compile_file_contexts(otatools, "".join(file_contexts))

        # Every other variant of a module whose APEX goes: apexd would activate it instead.
        removed_variants = set(replaced_apex_variants(
            names, zin.read, removed_apexes(has_hwcomposer, has_power, has_gnss, has_sensors, has_camera)))
        removed_variant_stems = {apex_file_stem(name) for name in removed_variants}
        # What the published build compiled, for the apps the image keeps: compile_apps
        # compiles an app again once its dex no longer matches, and the checks hold the image
        # to it. It is read before the members are copied: writestr takes each member's
        # ZipInfo and points it into the new archive, so a member read from zin after it is
        # copied would be looked for at the wrong place.
        prebuilt = {apk: entry for apk, entry in prebuilt_oat(names, zin.read).items()
                    if not removed(apk, has_hwcomposer, has_power, has_gnss, has_sensors, has_camera)}
        for info in zin.infolist():
            name = info.filename
            if name == SU_CLIENT_BINARY and su_payload is not None:
                continue
            if name == AOSP_XBIN_SU_BINARY and channeld_payload is not None:
                continue
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
                elif name == SERVICES_JAR:
                    data = edit_services_jar(data, name)
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
                                   for a in REMOVED_APPS + ["DsaAgent"]
                                   + [Path(overlay).stem for overlay in SYSTEM_OVERLAY_FILES])
                    )
                    if agent_payload is not None:
                        data += AGENT_APKCERTS.encode() + b"\n"
                    if overlay_payload is not None:
                        data += FRAMEWORK_OVERLAY_APKCERTS.encode() + b"\n"
                    if settings_overlay_payload is not None:
                        data += SETTINGS_OVERLAY_APKCERTS.encode() + b"\n"
                    for overlay_name in design_payloads:
                        data += overlay_apkcerts(overlay_name).encode() + b"\n"
                elif name == SYSTEM_FS_CONFIG:
                    dropped = (["system/xbin", "system/xbin/su"] if channeld_payload is not None else []) + (
                        ["system/bin/su"] if su_payload is not None else [])
                    if dropped:
                        lines = [line for line in data.decode().splitlines()
                                 if line.split()[:1] not in ([p] for p in dropped)]
                        data = ("\n".join(lines) + "\n").encode()
                    data = add_filesystem_config(data.decode(),
                                                 TRACE_FS_CONFIG + ARM_TRANSLATION_FS_CONFIG
                                                 + (ARM_TRANSLATION_DEFAULT_FS_CONFIG
                                                    if default_translator is not None else [])
                                                 + GAPPS_FS_CONFIG + ADDONS_FS_CONFIG
                                                 + (["system/etc/init/dsa-su-guard.rc 0 0 644 capabilities=0x0",
                                                     "system/etc/init/dsa-keymint-key.rc 0 0 644 capabilities=0x0",
                                                     "system/etc/init/dsa-cacerts.rc 0 0 644 capabilities=0x0",
                                                     "system/xbin 0 2000 755 capabilities=0x0"]
                                                    if channeld_payload is not None else [])
                                                 + (["system/bin/su 0 0 755 capabilities=0x0"]
                                                    if su_payload is not None else [])
                                                 + added_fs_config.get(name, [])).encode()
                elif name == SYSTEM_EXT_FS_CONFIG and (agent_payload is not None or name in added_fs_config):
                    data = add_filesystem_config(data.decode(),
                                                 (AGENT_FS_CONFIG if agent_payload is not None else [])
                                                 + added_fs_config.get(name, [])).encode()
                elif name in added_fs_config:
                    data = add_filesystem_config(data.decode(), added_fs_config[name]).encode()
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
                                                               has_power, codec2=has_codec2, audio=has_audio, gnss=has_gnss, sensors=has_sensors, camera=has_camera, keymint=has_keymint)
                        if overlay_payload is not None:
                            text = add_filesystem_config(text, FRAMEWORK_OVERLAY_FS_CONFIG)
                        if settings_overlay_payload is not None:
                            text = add_filesystem_config(text, SETTINGS_OVERLAY_FS_CONFIG)
                        for overlay_name in design_payloads:
                            text = add_filesystem_config(text, overlay_fs_config(overlay_name))
                    data = text.encode()
                elif name == PLAT_FILE_CONTEXTS:
                    text = with_zram_backing_context(data.decode())
                    if channeld_payload is not None:
                        text = update_plat_file_contexts(text)
                    data = text.encode()
                elif name in ("VENDOR/etc/selinux/vendor_file_contexts", "META/vendor_file_contexts"):
                    data = update_vendor_file_contexts(data.decode(), channeld_payload is not None,
                                                       has_hwcomposer, has_power, codec2=has_codec2,
                                                       audio=has_audio, gnss=has_gnss, sensors=has_sensors, camera=has_camera, keymint=has_keymint).encode()
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
        zout.writestr(PREBUILT_OAT_RECORD, json.dumps(prebuilt, indent=2, sort_keys=True) + "\n")
        for directory in FSTAB_DIRS:
            zout.writestr(directory + "fstab.dsa", fstab)
        zout.writestr(MEMORY_RC, (HERE / "dsa-memory.rc").read_bytes())
        zout.writestr(DISPLAY_RC, (HERE / "dsa-display.rc").read_bytes())
        zout.writestr(TRACE_RC, (HERE / "dsa-trace.rc").read_bytes())
        zout.writestr(CONSOLE_RC, (HERE / "dsa-console.rc").read_bytes())
        zout.writestr(ZRAM_BACKING_RC, (HERE / "dsa-zram-backing.rc").read_bytes())
        zout.writestr(ARM_TRANSLATION_RC, (HERE / "dsa-arm-translation.rc").read_bytes())
        for target, source_file in ARM_TRANSLATION_PROPS.items():
            zout.writestr(target, source_file.read_bytes())
        if default_translator is not None:
            zout.writestr(ARM_TRANSLATION_DEFAULT_IMAGE, default_translator)
            zout.writestr(ARM_TRANSLATION_DEFAULT_PROPS,
                          ARM_TRANSLATION_PROPS["SYSTEM/etc/dsa/arm-translation-on.prop"].read_bytes())
        if reach:
            for folder in ARM_TRANSLATION_FOLDERS:
                zout.writestr(directory_entry(folder + "/"), b"")
            for name in ARM_TRANSLATION_BOUND_FILES:
                zout.writestr(name, b"")
            for name, target in ARM_TRANSLATION_LINKS.items():
                zout.writestr(link_entry(name), target.encode())
        zout.writestr(GAPPS_RC, (HERE / "dsa-gapps.rc").read_bytes())
        zout.writestr(ADDONS_RC, (HERE / "dsa-addons.rc").read_bytes())
        for target, source_file in GAPPS_PROPS.items():
            zout.writestr(target, source_file.read_bytes())
        zout.writestr(GAPPS_COMPILE_SCRIPT, (HERE / "gapps-compile.sh").read_bytes())
        for folder in added_gapps_folders:
            zout.writestr(directory_entry(folder + "/"), b"")
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
            zout.writestr(SU_GUARD_RC, (HERE / "dsa-su-guard.rc").read_bytes())
            zout.writestr(KEYMINT_KEY_RC, (HERE / "dsa-keymint-key.rc").read_bytes())
            zout.writestr(CACERTS_RC, (HERE / "dsa-cacerts.rc").read_bytes())
            zout.writestr(CHANNEL_MANIFEST, (HERE / "dsa-channel-manifest.xml").read_bytes())
        if su_payload is not None:
            zout.writestr(SU_CLIENT_BINARY, su_payload)
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
        if has_keymint:
            # The files the build has under the same name (libssl, should a build carry it) were
            # replaced above; the rest are added.
            for name in KEYMINT_FILES:
                if name not in replaced_framework:
                    zout.writestr(name, framework[name])
                    replaced_framework.add(name)
            zout.writestr(KEYMINT_RC, KEYMINT_RC_SOURCE.read_bytes())
        if (channeld_payload is not None or has_codec2 or has_camera) and VENDOR_SERVICE_CONTEXTS not in names:
            zout.writestr(VENDOR_SERVICE_CONTEXTS, update_vendor_service_contexts(
                "", codec2=has_codec2, channeld=channeld_payload is not None, camera=has_camera))
        if overlay_payload is not None:
            zout.writestr(FRAMEWORK_OVERLAY_APK, overlay_payload)
        if settings_overlay_payload is not None:
            zout.writestr(SETTINGS_OVERLAY_APK, settings_overlay_payload)
        for overlay_name, payload in design_payloads.items():
            zout.writestr(f"VENDOR/overlay/{overlay_name}", payload)
        if agent_payload is not None:
            zout.writestr(AGENT_APK, agent_payload)
            zout.writestr(AGENT_PERMISSIONS, AGENT_PERMISSIONS_SOURCE.read_bytes())
            zout.writestr(AGENT_DEFAULT_PERMISSIONS, AGENT_DEFAULT_PERMISSIONS_SOURCE.read_bytes())
            zout.writestr(AGENT_SYSCONFIG, AGENT_SYSCONFIG_SOURCE.read_bytes())
        if vulkan_payload is not None and not replaced_vulkan_payload:
            zout.writestr(VULKAN_RANCHU_LIBRARY, vulkan_payload)
            replaced_vulkan_payload = True
        if "VENDOR/etc/selinux/vendor_file_contexts" not in names:
            zout.writestr(
                "VENDOR/etc/selinux/vendor_file_contexts",
                update_vendor_file_contexts("", channeld_payload is not None, has_hwcomposer, has_power,
                                            codec2=has_codec2, audio=has_audio, gnss=has_gnss, sensors=has_sensors, camera=has_camera, keymint=has_keymint).encode(),
            )
    if graphics_payload is not None and not replaced_graphics_payload:
        dest.unlink(missing_ok=True)
        raise ValueError(f"target_files does not contain {GFXSTREAM_LIBRARY}")
    if framework is not None and set(framework) - replaced_framework:
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
    SU_CLIENT_BINARY,
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


# An image's build number (image.json "build") is its AOSP build number times 100 plus its
# revision, so a host orders two images of one build by revision, and any image of a newer
# build after them. A revision of 100 would read as the next build.
MAX_REVISION = 99


def version_of(spec, revision):
    """The version that names the image of the inputs.json entry `spec` at `revision`:
    "17.0.0-16373615-10" for "17.0.0-16373615-9", "17.0.0-16373615-10-arm64" for an arm64 one."""
    marker = f"-{spec['build_id']}-{spec.get('revision', 1)}"
    version = spec["version"]
    at = version.find(marker)
    rest = version[at + len(marker):] if at >= 0 else None
    if rest is None or not (rest == "" or rest.startswith("-")):
        raise ValueError(f"version {version} does not name build {spec['build_id']} "
                         f"revision {spec.get('revision', 1)}")
    return f"{version[:at]}-{spec['build_id']}-{revision}{rest}"


def revised(spec, revision):
    """The inputs.json entry `spec` as the image of `revision`, with the version that names it."""
    if not 1 <= revision <= MAX_REVISION:
        raise ValueError(f"revision {revision} is outside 1 to {MAX_REVISION}: the image's build "
                         f"number is the AOSP build times 100 plus the revision")
    if revision < spec.get("revision", 1):
        raise ValueError(f"revision {revision} is lower than inputs.json's {spec.get('revision', 1)}")
    return dict(spec, revision=revision, version=version_of(spec, revision))


def next_revision(spec, tags):
    """The revision a new image of `spec` gets, given the release tags already published: one
    past the highest published image of the same build and architecture, and never below
    inputs.json's revision. A published image is never replaced: a host that has it would not
    see the new one as an update, and installs that checked its hashes would fail."""
    published = {f"android-{version_of(spec, r)}": r for r in range(1, MAX_REVISION + 1)}
    highest = max((published[tag.strip()] for tag in tags if tag.strip() in published), default=0)
    revision = max(spec.get("revision", 1), highest + 1)
    if revision > MAX_REVISION:
        raise ValueError(f"build {spec['build_id']} has used every revision up to {MAX_REVISION}; "
                         f"move inputs.json to a newer AOSP build")
    return revision


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
    """The odex and vdex target_files members ART looks for beside an APK or a jar."""
    folder, file = apk.rsplit("/", 1)
    stem = file.rsplit(".", 1)[0]
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
    target_files whose member names are `names` that has its odex in its own partition, and
    for each jar make-image.py edits (EDITED_JARS) that has its odex beside it, for the guest's
    instruction set. `read` reads a member."""
    names = set(names)
    found = {}
    for odex in sorted(names):
        m = re.fullmatch(rf"((?:{PREBUILT_OAT_PARTITIONS})/.+)/oat/{COMPILED_ISA}/([^/]+)\.odex", odex)
        if not m:
            continue
        source = next((f"{m.group(1)}/{m.group(2)}{ext}" for ext in (".apk", ".jar")
                       if f"{m.group(1)}/{m.group(2)}{ext}" in names
                       and (ext == ".apk" or f"{m.group(1)}/{m.group(2)}{ext}" in EDITED_JARS)), None)
        if source is None:
            continue
        keys = oat_key_values(read(odex))
        found[source] = {
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
    permission_controller_edits). Without otatools nothing can pack it again, and it is
    left as it is."""
    if otatools is None:
        print(f"not editing {name}: there is no otatools to pack it again", flush=True)
        return data

    def edit(apk, apk_name):
        if not apk_name.endswith("/" + PERMISSION_CONTROLLER_APK):
            return apk
        return edit_permission_controller_apk(apk, apk_name)

    return edit_apex_apks(data, name, edit, otatools, work)


def apk_edits_left(apk, code=None, controllers=None, dimensions=None, theme_attributes=None):
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
        for xml, edits in (theme_attributes or {}).items():
            if xml in files and set_xml_theme_attributes(z.read(xml), edits)[1]:
                left.append(f"{xml}: " + ", ".join(f"{element} {attribute} not ?attr/{attribute}"
                                                   for element, attribute in edits))
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
        left = apk_edits_left(apk.read_bytes(), dimensions=permission_controller_edits())
        if left:
            problems.append(f"{found[0]}:{apk.relative_to(payload).as_posix()} still has {'; '.join(left)}, "
                            "so the last row of its lists sits on the window's edge, or its cards keep "
                            "corners of their own")
    shutil.rmtree(folder, ignore_errors=True)
    return problems


def services_profiles(spec, inputs):
    """{SERVICES_JAR: [art-profile, art-profile-extra]}: the text profiles the inputs.json
    entry `spec` pins for services.jar, in the folder `inputs` fetch.sh put them in, in the
    order the published build concatenated them (its art-profile-combined)."""
    paths = []
    for key in (SERVICES_PROFILE_INPUT, SERVICES_PROFILE_INPUT + "_extra"):
        if key not in spec["files"]:
            raise ValueError(f"inputs.json names no {key} for build {spec['build_id']}, so the edited "
                             "services.jar could not be compiled with the published build's profile")
        path = inputs / spec["files"][key]["name"]
        if not path.exists():
            raise ValueError(f"{path} is missing: run dsa/guest/image/fetch.sh first")
        paths.append(path)
    return {SERVICES_JAR: paths}


def compile_apps(signed, tools_zip, otatools, work, env, profiles=None):
    """Compiles COMPILED_APPS in the signed target_files, and again each app or edited jar
    whose dex no longer matches the odex the published build shipped (compile_jobs), and adds
    their odex and vdex files and filesystem rules to it. `profiles` maps a jar to the text
    profiles (frameworks/base's art-profile format) the published build compiled it with.
    Returns the bytes each app's compiled files take."""
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
            if profile is None and filter == "speed-profile" and apk in (profiles or {}):
                listing = host_apk.with_suffix(".profile.txt")
                listing.write_bytes(b"".join(Path(p).read_bytes() for p in profiles[apk]))
                profile = host_apk.with_suffix(".prof")
                try:
                    run(profman, f"--create-profile-from={listing}", f"--apk={host_apk}",
                        f"--dex-location={location}", f"--reference-profile-file={profile}", env=env)
                except subprocess.CalledProcessError:
                    print(f"{apk}: profman could not read the published build's profile, so it is "
                          "compiled with speed", flush=True)
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


def kernel_cmdline(partition_cmdline, hwcomposer=False, arch=DEFAULT_ARCH, camera=False, keymint=False):
    """Returns the kernel command line shared by every DSA guest image of `arch`. With
    `hwcomposer`, the image has DSA's composer in place of Cuttlefish's; with `camera`, DSA's
    camera HAL in place of Cuttlefish's emulated cameras, whose APEX is then not selected; with
    `keymint`, DSA's KeyMint in place of the KeyMint APEX, of which none is then selected."""
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
    if keymint:
        # apexd activates none of a module's APEXes whose selection is "none".
        apexes = {**apexes, KEYMINT_APEX_MODULE: "none"}
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
        # The density SurfaceFlinger, and so every app, starts at (dsa-display.rc). The host
        # replaces it with its main monitor's at each boot, as it does display_dpi.
        f"androidboot.lcd_density={DISPLAY_DPI}",
        "androidboot.cpuvulkan.version=0",
        # init_graphics.vendor.rc copies it to ro.opengles.version, which the build.prop files
        # have already claimed with the same value (OPENGLES_VERSION).
        f"androidboot.opengles.version={OPENGLES_VERSION}",
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
    parser.add_argument("--keys", type=Path, help="the image signing keys (required to make an image)")
    parser.add_argument("--revision", type=int,
                        help="the image's revision, at least inputs.json's; the version that names "
                             "the image follows it. The release workflow passes the next unpublished one")
    parser.add_argument("--next-revision", action="store_true",
                        help="print the revision the next image of --major and --arch gets, given "
                             "the release tags already published, one per line on standard input, "
                             "and make nothing")
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
    parser.add_argument("--su-client", type=Path,
                        help="the guest's unprivileged su client built for the guest")
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
                        help="aapt2 from the Android SDK's build-tools, which builds the system screen "
                             "overlays; found under ANDROID_HOME when not given, and without it the image has "
                             "no overlay")
    parser.add_argument("--graphics-dir", type=Path,
                        help="a directory containing guest graphics libraries (vulkan.ranchu.so, libOpenglSystemCommon.so)")
    parser.add_argument("--arm-translation", type=Path,
                        help="an ARM translation package folder (arm-translation.json and system.img) that "
                             "an x86_64 image carries as its default translator, and releases beside it")
    parser.add_argument("--framework", type=Path,
                        help="a folder from the Framework build (framework.json and the files it names): "
                             "SurfaceFlinger, libhwui and libEGL that recover from a GPU device loss, and "
                             "DSA's KeyMint, whose root key comes from the host, built from the image's own "
                             "build for its architecture")
    args = parser.parse_args()
    if args.arm_translation:
        checked_arm_translation(args.arm_translation, args.arch)

    spec = image_spec(json.loads((HERE / "inputs.json").read_text())["images"], args.major, args.arch)
    if args.next_revision:
        print(next_revision(spec, sys.stdin.read().splitlines()))
        return
    if args.keys is None:
        parser.error("--keys is required to make an image")
    if args.revision is not None:
        spec = revised(spec, args.revision)
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
    design_overlays = None
    aapt2 = args.aapt2 or find_aapt2()
    if aapt2:
        framework_res = work / "framework-res.apk"
        with zipfile.ZipFile(target_files) as z:
            framework_res.write_bytes(z.read(FRAMEWORK_RES))
            home_padding = SETTINGS_APP in z.namelist() and settings_has_home_padding(z.read(SETTINGS_APP))
            tables = system_overlay_targets(z, otatools, work, env)
        framework_overlay, settings_overlay, design_overlays = build_system_overlays(
            aapt2, framework_res, work, tables, home_padding)
    else:
        print("no aapt2, so the image has none of the system screen overlays", flush=True)

    modified = work / "modified.zip"
    modify(target_files, modified, graphics_library=graphics_library, vulkan_driver=vulkan_driver,
           otatools=otatools, channeld=args.channeld, agent=args.agent, hwcomposer=args.hwcomposer,
           framework_overlay=framework_overlay, settings_overlay=settings_overlay, power_hal=args.power_hal,
           codec2=args.codec2, audio=args.audio, gnss_hal=args.gnss_hal,
           sensors=args.sensors, camera=args.camera, arm_translation=args.arm_translation,
           framework=framework_payloads, overlays=design_overlays, su_client=args.su_client,
           arch=args.arch)

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
    compile_apps(signed, inputs / spec["files"]["dexpreopt_tools"]["name"], otatools, work, env,
                 profiles=services_profiles(spec, inputs))
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
                             camera=args.camera is not None,
                             keymint=framework_payloads is not None and framework_has_keymint(framework_payloads))
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
