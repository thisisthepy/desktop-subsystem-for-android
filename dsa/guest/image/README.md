# guest/image

Makes the Android image the VM boots, without building AOSP: it takes the build inputs
Google CI publishes for the Cuttlefish x86_64 device, or for the 64-bit only arm64 one
(`aosp_cf_arm64_only_phone`) for ARM64 Windows hosts, changes them for DSA and re-signs
them with the project's keys. `make-image.py --arch arm64` makes the arm64 image; the
default is x86_64. The arm64 guest runs no 32-bit ARM code, because the target has none.

| File | What it is |
|---|---|
| `inputs.json` | The pinned inputs: per Android version, the CI build number and the SHA-256 of its `target_files`, `otatools` and `dexpreopt_tools` |
| `fetch.sh` | Downloads one version's inputs for one architecture (`x86_64` by default, or `arm64`) into `.scratch/aosp-inputs/<build>-<arch>/` and checks them |
| `make-keys.sh` | Makes a set of signing keys (CI makes a throwaway set each run) |
| `pack-keys.sh`, `unpack-keys.sh` | Split the key set into repository secrets and put it back |
| `make-image.py` | Changes, re-signs and lays out the image |
| `check-image.py` | Checks a made image |
| `fstab.dsa` | The guest's mount table on Hyper-V |
| `dsa-console.rc` | Quiets the serial console once boot completes, so it carries only the kernel's early boot and a panic |
| `unavailable-features.xml` | Hardware features the VM lacks, withdrawn from the system |
| `dsa-channeld.rc` | The service that runs the control channel daemon, and the trigger that drops the guest's caches when it asks |
| `dsa-memory.rc` | Turns on the zram swap and tunes the kernel for it |
| `dsa-channel-manifest.xml` | The vendor manifest entry declaring the daemon's binder service |
| `dsa-trace.rc` | Turns on main thread timing in every app when the input trace is on |
| `dsa-arm-translation.rc`, `arm-translation-on.prop`, `arm-translation-off.prop` | Use an ARM translation package (the one the user installed, or the image's default one), only on a boot the host marks (`../arm-translation/README.md`) |
| `dsa-gapps.rc`, `gapps-on.prop`, `gapps-off.prop` | Lay the Google apps the user installed (`dsa gapps install`) over `/product` and `/system_ext`, only on a boot the host marks |
| `sepolicy/` | Policy added to the image: vsock for graphics, the control daemon's domain and binder service, the memory controls (`memory.cil`), the audio module's socket to the host (`audio.cil`), and what the agent needs to force Doze and set the battery (`agent.cil`) |

```
dsa/guest/image/fetch.sh 17
dsa/guest/image/make-keys.sh .scratch/image-keys
ANDROID_NDK_HOME=/path/to/ndk dsa/guest/channeld/build-android.sh .scratch/channeld
dsa/guest/hwcomposer/build.sh .scratch .scratch/build-hwcomposer
dsa/guest/audio/build.sh .scratch .scratch/build-audio
(cd dsa/guest/agent && ./gradlew assembleRelease)
uv run dsa/guest/image/make-image.py --major 17 --keys .scratch/image-keys --channeld .scratch/channeld/dsa-channeld \
    --hwcomposer .scratch/build-hwcomposer/dsa-hwcomposer --audio .scratch/build-audio/dsa-audio \
    --agent dsa/guest/agent/build/outputs/apk/release/dsa-agent-release-unsigned.apk
uv run dsa/guest/image/check-image.py dsa/target/image/17.0.0-16373615 .scratch/image-keys
```

It needs `gdisk` and `qemu-utils` (Debian and Ubuntu package names), Java and OpenSSL, and
aapt2 from the Android SDK's build-tools for the framework overlay.

## What changes

- The launcher, SystemUI and the keyboard app are removed: the desktop is the shell, and
  text input goes through the host's input method.
- Apps a desktop has no use for are removed too (`UNNEEDED_APPS` in `make-image.py`, each
  with its reason): telephony apps without a modem, screen savers and wallpaper pickers, the
  launcher's search widget, Bluetooth MIDI, the old AOSP Calendar, Gallery and Music apps
  (no longer maintained upstream; the user installs their own), and developer and demo apps.
  Each is a leaf that
  nothing else needs; WebView, the providers, PackageInstaller, PermissionController,
  DocumentsUI and the other apps that apps call stay. `make-image.py` prints each file it
  removes.
- The guest keeps its memory small, so the host gets back what it does not use. It swaps to
  zram of half its memory (`fstab.dsa`, `dsa-memory.rc`), and lmkd watches the swap as well
  as memory pressure. When no app task has been in use for a minute (none open, or all
  minimized), or most of its memory is cache, the control daemon sets
  `vendor.dsa.memory.reclaim`. Once boot has finished, init moves cold pages to zram, drops the
  clean caches and compacts memory, which free page reporting then hands back to Hyper-V.
  While idle it does so again when memory has grown back. ART keeps less free heap
  (`MEMORY_PROPS` in `make-image.py`). Only the daemon
  may set the property, and only init's vendor scripts may write those kernel settings.
- The oemlock, UWB, Bluetooth and Thread HAL APEXes are removed. Their devices
  (Cuttlefish's virtio consoles, radios) do not exist on Hyper-V, they abort in a loop, and
  system_server waits for the oemlock HAL forever. Bluetooth comes back as a host HAL.
- `unavailable-features.xml` withdraws the Bluetooth features, so system_server does not
  start a Bluetooth stack that has no controller to talk to, and the UWB feature, so nothing
  waits for its removed HAL.
- Settings' sound screen loses its media output switcher and its cast volume group. Their
  controllers need the Bluetooth service, which system_server does not start without the
  Bluetooth features: SettingsLib's media manager is then built without its routing half, and
  opening the screen took the Settings process and its window down. `make-image.py` removes
  the two preferences from the compiled `res/xml/sound_settings.xml` in `Settings.apk`
  (`SETTINGS_CONTROLLERS_WITHOUT_BLUETOOTH`), and prints each one it removes. Settings is
  re-signed with every other app. They come back with Bluetooth.
- Settings' accessibility screen loses its hearing devices entry, and two methods in
  `Settings.apk`'s dex files are edited (`SETTINGS_CODE_WITHOUT_BLUETOOTH`): SettingsLib's
  `BluetoothUtils.isAudioSharingSupported` returns false, and
  `AccessibilitySettings.onAttach` no longer hands the fragment manager to the hearing aid
  controller that cannot exist. Both dereferenced the null Bluetooth adapter, and took
  Settings down when Connected devices or Accessibility opened. Each edit keeps the method's
  size, and the dex checksum and signature are written again. `check-image.py` fails an image
  that still has either.
- The system apps users open (`COMPILED_APPS`: Settings, Files, the package installer, the
  share sheet, WebView, the agent and a few more) are compiled to the guest's machine code (x86_64 or arm64) when
  the image is made. The build compiles them only into system_other, which the guest does not
  have, and with the verify filter only, so they ran interpreted and each Settings screen took
  100 to 350 ms to open. `make-image.py` runs the build's own `dex2oat` (from
  `dexpreopt_tools.zip`, pinned in `inputs.json`) against the guest's boot image, after the
  dex edits and the signing, and puts each odex and vdex beside its APK: Settings with
  `speed`, the others with `speed-profile` when the APK carries a profile and `speed`
  otherwise. It prints the size each app adds. `check-image.py` fails an image whose compiled
  app is missing, was made for other dex checksums than the APK's, uses another filter, boot
  image or set of shared libraries. The apps whose dex it edits (`EDITED_APPS`: Settings and
  Files) are compiled with `--abort-on-hard-verifier-error`, so an edited method the verifier
  rejects stops the build instead of closing the app when it loads. PermissionController is
  not compiled: it is inside the permission APEX, where ART takes no odex.
- The apps the published build itself compiled into their own partition (KeyChain,
  SettingsProvider, Telecom and a few more on Android 16) keep their odex, vdex and app image:
  signing leaves an APK's dex as it is. `make-image.py` records them from the published
  target_files (`META/dsa_prebuilt_oat.json`), and compiles any of them whose dex it changes
  again, with the filter, shared libraries and boot image the published build used.
  `check-image.py` fails an image where one of them lost a file, or has one made for another
  dex, another boot image or another filter.
- A static overlay (`/vendor/overlay/DsaFrameworkOverlay.apk`) sets the framework's tap
  timeout to 0. A view inside anything that scrolls, which includes every list row, waits that
  long before it shows that it is pressed, in case the touch becomes a scroll, so a mouse
  click's ripple started 100 ms late. A touch that becomes a scroll now shows the row pressed
  until it moves past the touch slop. `make-image.py` builds the overlay with aapt2 from the
  Android SDK (`--aapt2`, or found under `ANDROID_HOME`) against the image's own framework.
- A second static overlay (`/vendor/overlay/DsaSettingsOverlay.apk`) replaces Settings'
  toolbar back icon (`settingslib_expressive_icon_back`). SettingsLib draws its 40 dp circle
  16 dp into the 56 dp button, so the circle's centre is 8 dp right of the button's. The
  framework draws the button's hover layer and ripple around the button's centre, and they
  showed 8 dp left of the circle at every density. The replacement keeps the 56 dp and puts
  8 dp on each side of the circle. aapt2 builds it the same way as the framework overlay.
  The same overlay replaces the list every Settings preference screen shows its rows in
  (androidx.preference's `preference_recyclerview`) with one that keeps 24 dp below the last
  row. Settings takes the space under a list only from the navigation bar's inset
  (SettingsLib's `EdgeToEdgeUtils` pads the content view by it), and a DSA window has no
  navigation bar, so the last row sat on the window's bottom edge. The space is inside the
  list, so rows still scroll to the edge. The overlay names what it replaces in a resource
  map (`android:resourcesMap`), which makes the framework rewrite the layout's id and style
  attribute to Settings' own.
- The lists that set their own padding get the same 24 dp, edited in the apps themselves
  (`LIST_BOTTOM_MARGIN_DP`). Settings' home page: on Android 16 its
  `TopLevelSettings.onCreateRecyclerView` no longer calls `setPadding(0, 0, 0, 0)` over the
  overlay's layout; on Android 17, `isRunningInFreeformWindow` says yes (the host draws the
  caption), so Android's own freeform padding applies, and the overlay sets that
  `dashboard_padding_bottom` to 24 dp. Settings' All apps list gets `paddingBottom` in its
  compiled `manage_applications_apps.xml`. PermissionController's `preference_recyclerview`
  gets it too; the app declares `<overlayable>`, so no overlay may replace its layouts, and it
  lives in the permission APEX, which `make-image.py` unpacks with otatools' `deapexer`,
  edits and packs with `apexer` before signing, as signing itself packs it, and checks again
  after signing. Files (DocumentsUI) on Android 16 pads its list's bottom by the height of
  the bar below it, and by 0 in windows without one: `getSaveLayoutHeight` now returns at
  least 24 dp in pixels. Android 17's Files keeps 48 dp of its own. The dex edits keep each
  method's size; `check-image.py` fails an image without them.
- With the input trace on (`androidboot.hardware.dsa_input_trace=1`), `dsa-trace.rc` makes every
  app's main thread log each message it takes 16 ms or more to handle, or starts 16 ms or more
  late.
- ARM app translation stays off unless the host turns it on. The product `build.prop` ends
  with an import of
  `/system/etc/dsa/arm-translation-${ro.boot.dsa.arm_translation:-off}.prop`: the "off" file
  sets nothing; the "on" file, which the host selects while the user has a translation package
  installed (`dsa arm-translation install`) and on, and the "default" file (the same lines),
  which it selects while they have none and the image has a default package, name the native
  bridge and add `arm64-v8a` after `x86_64` to the ABIs. Then `dsa-arm-translation.rc` mounts
  the package's erofs image (the user's from `/data/vendor/dsa/arm-translation/system.img`, or
  the default one from `/system/etc/dsa/arm-translation/default.img`) over `/system`'s `lib64`,
  `bin` and `etc` before the zygote starts, mounts binfmt_misc and registers the package's
  handlers. `sepolicy/arm-translation.cil` lets init do so; dsa-channeld writes and removes the
  user's image. Only an x86_64 image made with `--arm-translation` has the default package and
  its "default" file, so a boot told to use one on any other image stays x86_64 only.
- With `--framework` (a folder from the "Framework build" workflow), SurfaceFlinger, libhwui
  and libEGL are builds of the image's own source with DSA's recovery from a GPU device loss
  (`../framework/patches`). When the host's GPU resets, SurfaceFlinger and every app's renderer
  drop their GPU context, build a new one once the GPU answers, and draw every window again,
  instead of aborting and restarting the framework. The files must come from the image's own
  build number and architecture; `image.json` lists their SHA-256 under `framework`, and
  `check-image.py` finds them in the image. `sepolicy/gpu-reset.cil` lets SurfaceFlinger tell
  the apps of a reset through `debug.dsa.gpu.resets`.
- Google Play stays out of the image. A user who wants it runs `dsa gapps install` (or the
  settings app's Google Play item): the host downloads LiteGapps' Core build for this Android
  and sends an erofs image of its apps, permission lists, system configuration and framework
  files, which dsa-channeld keeps at `/data/vendor/dsa/gapps/system.img`. While one is installed
  the host puts `androidboot.dsa.gapps=on` in the bootconfig. Then `dsa-gapps.rc` mounts the
  image and lays its `product` and `system_ext` folders over `/product` and `/system_ext`
  before the zygote starts, so each app's privileged permission list stays in its own
  partition, where the package manager reads it. The product `build.prop` imports
  `/system/etc/dsa/gapps-${ro.boot.dsa.gapps:-off}.prop` just before the ARM translation
  import: the "on" file sets the fingerprint of the oem partition, which the image does not
  have, so the boot is an upgrade and the package manager installs the new system apps for every
  user and grants their default permissions. `sepolicy/gapps.cil` lets init do so.
- Cuttlefish's `seriallogging.rc` is removed. Its logger writes to a virtio console Hyper-V
  does not have, and init would restart it every five seconds for as long as the guest runs.
- `sepolicy/memory.cil` lets the audio HAL and the software codecs share memory through
  memfds. Refused, each falls back to an ashmem device the kernel does not have, and audio
  policy never starts.
- `wifi.rc` no longer sets up its kernel trace instance when boot completes. The VM has no
  Wi-Fi device to trace, and on Hyper-V those tracefs writes hold up lmkd's own tracepoint
  setup past its two second watchdog, which then kills the app the user just opened.
- The build becomes a user build (`ro.debuggable=0`). adbd is in it but stopped, and starts
  only while the user's developer mode is on (`dsa settings set developer-mode on`, or the
  settings app). dsa-channeld then sets `vendor.dsa.adb`, and `dsa-channeld.rc` starts or stops
  adbd to match. adbd listens on vsock port 5555 alone, never on TCP, and asks for no
  authorisation (`ro.adb.secure=0`), because the guest has no SystemUI to show the prompt.
  The host bridges that port to `127.0.0.1:58526` (the `adb-port` setting), so
  `adb connect 127.0.0.1:58526` reaches it. `sepolicy/adb.cil` lets adbd listen on vsock.
- The control channel daemon (`/vendor/bin/dsa-channeld`) is added and runs in its own
  SELinux domain, the only one that may read `/proc/bootconfig`, where the host puts the
  boot's secret. It connects to the host, and tells the host when Android has finished booting
  (`vendor.dsa.boot_completed`, which init sets) and whether the agent is connected. The host
  calls Android active only then.
- The daemon registers the binder service `io.github.thisisthepy.dsa.channel.IChannel/default`,
  which the agent and the hwcomposer call. `dsa-channel-manifest.xml` declares it in the
  vendor manifest, because servicemanager refuses a stable service no manifest declares.
  `vendor_service_contexts` labels it, and the policy lets only the daemon register it and
  only `system_app`, `platform_app` and the hwcomposer find it, call it and be called back.
- DSA's composer HAL (`/vendor/bin/hw/dsa-hwcomposer`, from `dsa/guest/hwcomposer/`)
  replaces Cuttlefish's two composer APEXes when `--hwcomposer` names it. It sends the host
  each task's frames rather than one picture of the whole display. The kernel command line
  sets its display (`androidboot.dsa.display_width`, `_height`, `_dpi`) and drops the
  settings only Cuttlefish's composer read.
- DSA's primary audio module (`/vendor/bin/hw/dsa-audio`, from `dsa/guest/audio/`) is added when
  `--audio` names it. It serves `IModule/default` in the audio HAL's domain and sends the
  guest's sound to the host, which plays it on the user's default output device. The primary
  module leaves `audio_policy_configuration.xml`, so the image's own audio service no longer
  makes one that plays nowhere, and keeps the remote submix and Bluetooth modules.
  `sepolicy/audio.cil` lets the audio HAL reach the host over vsock.
- The agent (`dsa/guest/agent/`) is added as a privileged app in
  `/system_ext/priv-app/DsaAgent/`, with its privileged permission list, and is signed with
  the platform key. It tells the host about each app task, so each gets its own window.
- The published kernel modules are dropped. They belong to Google's kernel; DSA boots its
  own kernel with the drivers built in.
- `fstab.dsa` replaces the mount table. Every partition is found by its name
  (`/dev/block/by-name/...`), never by a kernel disk name such as `/dev/block/sda`. Hyper-V
  names its SCSI disks in the order it probes them, and that order changes when the host
  attaches another disk. The kernel command line names the system disk by the GUID written
  into its GPT (`blkdevparts=<disk GUID>:...`), and DSA's kernel reads that GUID from each
  disk, so the system disk's partition names land on it whichever kernel name it gets. The
  data disk's partitions get their names from its own GPT. `check-image.py` fails an image
  whose command line, mount tables or init scripts name a disk as `sda`.
- Android's log (`logcat`) stays in logd's memory and is never written to a file or to the
  serial console. The control daemon reads it only while the host asks (`dsa logcat`, the
  settings app's log view, `dsa logs collect`) and sends it on a vsock port of its own.
  Cuttlefish's logger, which writes to a virtio console Hyper-V does not have, is removed.
  logd also keeps the kernel's log (`ro.logd.kernel`), and once boot completes the serial
  console prints only emergencies (`dsa-console.rc`), because each byte written to the
  emulated port traps out of the VM and takes time from vsync and input. The console keeps
  the kernel's early boot and a panic, which the host keeps as the VM's console log.
- The image reports DSA's device identity instead of Cuttlefish's, in every partition's
  `build.prop` and in the generic ramdisk's: manufacturer `thisisthepy`, brand `DSA`, model
  `Desktop Subsystem for Android`, device and product name `dsa_x86_64` (`dsa_arm64` in an
  arm64 image), `ro.build.characteristics=tablet`, and `user` in every fingerprint. The
  product `build.prop` ends with `import` lines that take these properties, and the chip's
  (`ro.soc.*`), from `/second_stage_resources/system/etc/ramdisk/build.prop`. At every boot
  the host appends a small cpio archive with that file to the boot's initrd, holding the
  computer's manufacturer, brand, model, model name and chip, and what the user changed in
  the settings; first stage init keeps it for the second stage, and init reads it before the
  property service locks `ro.*`. `image.json` names the build's release, ID, incremental
  version and tags under `identity`, and its `arch`, so the host can compose a fingerprint.
- Every APK, APEX and verified boot image is re-signed. The published build is signed with
  AOSP's test keys, whose private halves are public.

## Tracing a click

The input trace follows one click from the host window to the screen. Each process on the
path writes `input-trace stage=...` lines:

| Stage | Where | Placed at |
|---|---|---|
| `app.pointer` | the app window host, when Windows hands it the press or release, or when `dsa tap` says a press is coming (`source=dsa-tap`) | `host=` |
| `agent.inject` | the agent, when the framework has taken the event | guest `mono=` |
| `app.frame` | the agent, from the app's `gfxinfo framestats`: main thread `late`, `input`, `animation`, `traversal`, `record`; render thread `sync`, `issue`, `swap`, `dequeue`, `gpu` | its intended vsync |
| `sf.frame` | the agent, from `dumpsys SurfaceFlinger --latency` of each of the app's layers: `desired`, `ready`, `actual` | `actual`, when SurfaceFlinger presented it |
| `hwc.frame` | the hwcomposer, when it has composed and posted the task's frame | guest `mono=` |
| `wm.frame` | the window manager, when the window host has copied the frame out | `host=` |
| `app.present` | the window host, after its `Present` | `host=` |
| `app.visible_change` | the window host, for the first frame after a press that changed enough of the 64-pixel square around the press point by enough to be seen (`changed=` is the share of its pixels), compared with what the square showed when the press came | `host=` |
| `app.shown` | the window host, from the swap chain's frame statistics: the vertical blank the present was shown at | `vblank=` |
| `clock` | the service, once a second: the guest's clock against the host's | |

Host lines carry `host=`, milliseconds on the performance counter, which every host process
shares. Guest lines carry `mono=`, the guest's `CLOCK_MONOTONIC`. The service pairs the two:
it sends the guest's control daemon its send time, the daemon answers at once with its own
clock, and the `clock` line gives the round trip and the guest's offset from the trip's
midpoint.

To take a trace:

1. Turn the host's side on: run `dsa settings set input-trace on` as an administrator (or
   set `DSA_INPUT_TRACE=1` for the processes in the user's session). The setting is the
   computer's; `dsa settings set input-trace off` turns it off again. The per-stage resize
   trace is the same kind of setting, `resize-trace`. Earlier releases used empty
   `input-trace` and `resize-trace` files instead; the service turns any it finds into the
   settings and removes them.
2. Turn the guest's side on: add `androidboot.hardware.dsa_input_trace=1` to `kernel_cmdline`
   in the service's configuration. The service probes the clocks only for a guest that boots
   with it.
3. Restart DSA (the service and the user's processes), so every process reads the setting.
4. Click in an app window, with a second or more between clicks.
5. Lay the clicks out on the host clock:

   ```
   uv run scripts/input-trace.py
   ```

   With no arguments it reads `%LOCALAPPDATA%\DSA\logs\*.log` and the machine's
   `%ProgramData%\DSA\logs\*.log`, or the `logs` folder of the data folder chosen at install
   (the service's log, with the `clock` lines), and Android's log, with the guest's stages,
   through `dsa logcat -d -b all`. Android keeps its log in memory, so run it soon after the
   clicks. Files or folders given as arguments are read instead, such as the output of
   `dsa logcat -d -b all` saved to a file.
   For each press and release it prints the stages that follow on the same task or app,
   until the next press or release, with the time since the press and the gap to the stage
   before. Then, for each press, the time to the first app frame, to the first visible
   change around the press point, and to the next screen.

To click without moving the mouse, `dsa tap <package> <x> <y>` presses at a point of the app's
task through the local API, and `measure.ps1 -Part click` in `dsa/host/platform/windows/tools`
does the clicks and step 5 itself.

Whether the trace is on or not, each task window logs when it presented its first frame and
when that frame was shown; `dsa launch <package> --wait-first-frame` prints both, from the
launch request.

Turn the trace off again with `dsa settings set input-trace off` and by removing the kernel
command line setting. While on,
it writes a line per stage and the agent reads the app's frame timings after each release.

## What comes out

In `dsa/target/image/<version>/`:

- `system.vhdx`: the read-only system disk (dynamic). It holds `super` with Android and
  its verified boot data, and every user's VM attaches it as it is.
- `data.vhdx`: the template of a user's data disk (dynamic). It holds empty `misc`,
  `metadata` and `userdata`, which Android writes and formats on first boot. `misc` is
  here because the boot control HAL writes the boot state into it. The service copies the
  template for a new user and keeps each user's disk across updates.
- `initrd.img`: the vendor ramdisk followed by the generic one.
- `bzImage` (x86_64) or `Image` (arm64), with `--kernel`: the guest kernel (`dsa/kernel/`).
  The host boots an image's own kernel when it has one, so a kernel update ships with the
  image. `make-image.py` refuses a kernel built for the other architecture.
- `image.json`: the version, the architecture (`arch`), the source build, the kernel command
  line, and the files' SHA-256. An arm64 image's version ends in `-arm64`, so both images of
  one build are published side by side.
- `signed-target_files.zip`: the signed build inputs, kept to make later images and update
  packages from. It is not part of the image and is never published.

## Moving to a newer build

Change the build number and the two SHA-256 values in `inputs.json`. The Image test
workflow makes and checks the image from it.

To publish a remade image of the same build, after a fix to how images are made, raise
`revision` and the version's suffix (`17.0.0-16373615-2`). The image's `build`, which
orders images of one Android version, is the build number times 100 plus the revision, so
the service treats the remade image as newer and moves to it.

## Releasing

The Image release workflow (`.github/workflows/release-image.yml`, run by hand) makes
every image in `inputs.json` with the project's keys and the kernel it builds, publishes
each as the GitHub release `android-<version>`, and publishes the signed list the service
reads as the release `images`. It needs these repository secrets:

- `DSA_ANDROID_KEYS_1` and `DSA_ANDROID_KEYS_2`: the key set, made once and kept, since
  every later image must be signed with the same keys for an update to keep each user's
  data. A secret holds at most 48 KB and the key set is larger, so `pack-keys.sh` splits
  it and prints the command for each part:

  ```
  dsa/guest/image/make-keys.sh .scratch/release-keys
  dsa/guest/image/pack-keys.sh .scratch/release-keys .scratch/release-keys-packed
  ```

- `DSA_RELEASE_SIGNING_KEY`: the list's key, from `dsa-release keygen`.

Keep a copy of both outside GitHub: a secret cannot be read back, and losing the Android
keys means no image can update an existing install again.
