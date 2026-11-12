# guest/image

Makes the Android image the VM boots, without building AOSP: it takes the build inputs
Google CI publishes for the Cuttlefish x86_64 device, or for the 64-bit only arm64 one
(`aosp_cf_arm64_only_phone`) for ARM64 Windows hosts, changes them for DSA and re-signs
them with the project's keys. `make-image.py --arch arm64` makes the arm64 image; the
default is x86_64. The arm64 guest runs no 32-bit ARM code, because the target has none.

| File | What it is |
|---|---|
| `inputs.json` | The pinned inputs: per Android version, the CI build number and the SHA-256 of its `target_files`, `otatools` and `dexpreopt_tools`, and the URL and SHA-256 of `services.jar`'s ART profile at the build's own frameworks/base commit |
| `fetch.sh` | Downloads one version's inputs for one architecture (`x86_64` by default, or `arm64`) into `.scratch/aosp-inputs/<build>-<arch>/` and checks them |
| `make-keys.sh` | Makes a set of signing keys (CI makes a throwaway set each run) |
| `pack-keys.sh`, `unpack-keys.sh` | Split the key set into repository secrets and put it back |
| `make-image.py` | Changes, re-signs and lays out the image |
| `check-image.py` | Checks a made image |
| `fstab.dsa` | The guest's mount table on Hyper-V |
| `dsa-console.rc` | Quiets the serial console once boot completes, so it carries only the kernel's early boot and a panic |
| `unavailable-features.xml` | Hardware features the VM lacks, withdrawn from the system |
| `dsa-channeld.rc` | The service that runs the control channel daemon, and the trigger that drops the guest's caches when it asks |
| `dsa-keymint-key.rc` | Makes the tmpfs folder for KeyMint's root key and starts the daemon's mode that fetches it from the host, on init |
| `dsa-cacerts.rc` | Binds tmpfs copies of Android's system certificate folders over the originals before the zygote starts, so the daemon can add the root certificates the host added beside Android's |
| `dsa-memory.rc` | Turns on the zram swap and tunes the kernel for it |
| `dsa-zram-backing.rc` | Makes the folder for zram's backing file on the data disk and removes the last boot's file |
| `dsa-display.rc` | Starts SurfaceFlinger, and so every app, at the density the boot names (the user's main monitor's) |
| `dsa-channel-manifest.xml` | The vendor manifest entry declaring the daemon's binder service |
| `dsa-trace.rc` | Turns on main thread timing in every app when the input trace is on |
| `dsa-arm-translation.rc`, `arm-translation-on.prop`, `arm-translation-off.prop` | Use an ARM translation package (the one the user installed, or the image's default one), only on a boot the host marks (`../arm-translation/README.md`) |
| `dsa-gapps.rc`, `gapps-on.prop`, `gapps-off.prop` | Lay the Google apps the user installed (`dsa gapps install`) over the app and configuration folders of `/product` and `/system_ext`, only on a boot the host marks |
| `sepolicy/` | Policy added to the image: vsock for graphics, the control daemon's domain and binder service, the memory controls (`memory.cil`), the audio module's socket to the host (`audio.cil`), and what the agent needs to force Doze and set the battery (`agent.cil`) |

```
dsa/guest/image/fetch.sh 17
dsa/guest/image/make-keys.sh .scratch/image-keys
ANDROID_NDK_HOME=/path/to/ndk dsa/guest/channeld/build-android.sh .scratch/channeld
dsa/guest/hwcomposer/build.sh .scratch .scratch/build-hwcomposer
dsa/guest/audio/build.sh .scratch .scratch/build-audio
(cd dsa/guest/agent && ./kotlin do assembleRelease)
uv run dsa/guest/image/make-image.py --major 17 --keys .scratch/image-keys --channeld .scratch/channeld/dsa-channeld \
    --hwcomposer .scratch/build-hwcomposer/dsa-hwcomposer --audio .scratch/build-audio/dsa-audio \
    --agent dsa/guest/agent/build/outputs/apk/release/dsa-agent-release-unsigned.apk
uv run dsa/guest/image/check-image.py dsa/target/image/17.0.0-16373615 .scratch/image-keys
```

It needs `gdisk` and `qemu-utils` (Debian and Ubuntu package names), Java and OpenSSL, and
aapt2 from the Android SDK's build-tools for the system screen overlays.

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
  While idle it does so again when memory has grown back, and then has the kernel report
  every free page to the host again (`page_reporting.rereport`). What stays cold in zram goes
  on to a backing file on the data disk (`/data/per_boot/zram_swap`, 512 MiB, made again each
  boot): once no app has been open for ten minutes, at most every ten minutes, the daemon has
  init write zram's pages unused for half an hour, incompressible ones first, at most 32 MiB a
  run and 512 MiB a day. ART keeps less free heap
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
- The host window's title bar is Android's status bar. The agent gives each task status bar
  and caption bar insets as high as the title bar, but stock Android hands a freeform window
  only its caption bar and IME insets, so an app that pads its toolbar by its status bar inset
  drew it under the title bar. `services.jar`'s dex is edited (`SERVICES_CODE`): the window
  manager's `InsetsPolicy` keeps the system bars of a floating window, not only its caption
  bar. The jar's stored dex files stay aligned, and the jar is compiled again with the
  published build's filter, class loader context, app image and profile (`services/art-profile`
  and `art-profile-extra` at the build's own frameworks/base commit, pinned in `inputs.json`).
  The framework overlay below also sets Android's status bar height resources
  (`status_bar_height`, `status_bar_height_default` and the rest) to the title bar's height at
  each scale's density, for the apps that read those instead of their insets. `check-image.py`
  fails an image without the edit, with `services.jar`'s dex unaligned, or with status bar
  heights that are not the title bar's.
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
- The screens Android draws itself have one design: Settings, Files (DocumentsUI), the
  permission screens and dialogs (PermissionController), the install dialogs
  (PackageInstaller), the share sheet (IntentResolver), and the framework's dialogs and
  toasts. A large title at the top of a Settings screen folds into the toolbar as the list
  scrolls, and starts folded in a window under 600 dp tall, where it would take a third of
  the height. List rows are grouped in rounded cards (26 dp outer corners, square where two
  rows of a group meet), titles are bold, the accent is blue, light and dark themes follow
  the same rules, and dialogs have 26 dp corners with their buttons side by side along the
  bottom. The text is the image's own Roboto at its bold weight; every drawing is DSA's own.
  - Static overlays in `/vendor/overlay` carry it, enabled at every boot: the framework
    overlay above (colours, dialog and sheet corners, the dialog button bar, the toast, and
    bold dialog, window and toolbar titles), the Settings overlay above (the cards, the large
    titles and the home page's title and search bar), and one each for Files (its Material 3
    look, which it offers devices through an overlayable switch), PermissionController (the
    dialog's bold title), PackageInstaller (bold titles) and the share sheet (its headline).
  - Each overlay is built with aapt2 against the image's own framework-res, and checked
    against the app it overlays before it is built: an overlay that would replace a resource
    the app does not have stops the image. Every overlay's references into the framework are
    checked against that framework-res before any is built: one that names a resource it
    lacks, or a private one other than through `@*android:`, stops the image with all of
    them listed, since aapt2 links an overlay only to the framework's public resources and
    would stop at the first. It records the SHA-256 of the two resource tables
    in `assets/dsa-overlay.json`; `check-image.py` fails an image with an overlay missing, or
    one built against another framework-res or another build of its app.
  - What no overlay can reach is edited in the app. SettingsLib's toolbar layouts start the
    large title folded in every window (`app:expanded="false"` in the layout itself), so
    their compiled layouts in `Settings.apk` take the value from the app bar's theme
    (`?attr/expanded`), and the Settings overlay's version of that theme says false below
    600 dp and true from 600 dp up. PermissionController declares `<overlayable>` without its
    list backgrounds, so its compiled backgrounds get the cards' corners.
  - The fold follows the window's height when a screen is created. Settings' home page keeps
    its views when its window is resized, so a window resized across 600 dp keeps its title
    area until the page opens again. Files has no title of its own to enlarge: its layouts
    are not overlayable, so it gets the colours and its rounded Material 3 look only.
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
  the package's erofs image (the user's from `arm-translation.img` on the add-on disk, or
  the default one from `/system/etc/dsa/arm-translation/default.img`) at
  `/mnt/dsa-arm-translation` before the zygote starts, and binds its parts onto empty places
  an x86_64 image carries for them: `system/lib64` onto `/system_ext/lib64`, on the linker
  search path where the zygote finds the bridge by name; `system/lib64/arm64` and
  `system/bin/arm64` onto the same paths; `system/bin` onto `/system/bin/dsa-arm-translation`,
  where the binfmt_misc rules start the runners; and `system/etc/ld.config.arm64.txt` and
  `system/lib64/libberberis_exec_region.so` onto the same paths. Berberis opens that library by
  its absolute path, not by name, so without it every ARM app aborts as it starts. Links lead `/system/etc/cpuinfo.arm64.txt` and `/system/etc/berberis` into the
  mount. Nothing is laid over `/system/bin`, `/system/lib64` or `/system/etc`: a service run
  through an overlay is looked up with init's credentials, and bpfloader's start failed that
  way and rebooted Android. It then mounts binfmt_misc and registers the package's handlers.
  `make-image.py` refuses a build that already has files in those places, and `check-image.py`
  checks them. `sepolicy/arm-translation.cil` lets init do so. Only an x86_64 image made with
  `--arm-translation` has the default package and
  its "default" file, so a boot told to use one on any other image stays x86_64 only.
- With `--framework` (a folder from the "Framework build" workflow), SurfaceFlinger, libhwui
  and libEGL are builds of the image's own source with DSA's recovery from a GPU device loss
  (`../framework/patches`). When the host's GPU resets, SurfaceFlinger and every app's renderer
  drop their GPU context, build a new one once the GPU answers, and draw every window again,
  instead of aborting and restarting the framework. The files must come from the image's own
  build number and architecture; `image.json` lists their SHA-256 under `framework`, and
  `check-image.py` finds them in the image. `sepolicy/gpu-reset.cil` lets SurfaceFlinger tell
  the apps of a reset through `debug.dsa.gpu.resets`.
- The same Framework build makes DSA's KeyMint, `dsa-keymint`. The KeyMint APEX the published
  build selects wraps every key under a fixed all-zero root key, so a copy of the guest's disk
  gives up every key on it. `dsa-keymint` is the same nonsecure KeyMint, patched to take its
  root key from the host: dsa-channeld fetches it each boot and leaves it on tmpfs as
  `/dev/dsa_keymint/root_key`. New keys are wrapped under the host's key; keys wrapped before
  under the built-in one still open, and Android is asked to upgrade each one, which wraps it
  again under the host's key, so an existing install loses no key. Without a key from the host
  it behaves as the APEX did. When the framework folder has it, `make-image.py` installs it as
  vendor files (`/vendor/bin/hw/dsa-keymint`, the `libssl` it needs, `dsa-keymint.rc`, and the
  VINTF fragments and feature file the APEX carries), labels it as the APEX labelled its binary
  (`hal_keymint_default_exec`, so it runs in the same domain as the same service), and selects
  no KeyMint APEX (`androidboot.vendor.apex.com.android.hardware.keymint=none`). `image.json`
  lists its files' SHA-256 under `framework`, and `check-image.py` checks that they are in the
  image, that it declares every instance the service registers and has every library it needs,
  and that no KeyMint APEX is active beside it. A framework folder from before the KeyMint has
  none of these files, and the image keeps the APEX.
- Google Play stays out of the image. A user who wants it runs `dsa gapps install` (or the
  settings app's Google Play item): the host downloads LiteGapps' Core build for this Android
  and makes an erofs image of its apps, permission lists, system configuration and framework
  files, `gapps.img` on the add-on disk. While one is installed the host puts
  `androidboot.dsa.gapps=on` in the bootconfig. Then `dsa-gapps.rc` mounts the image and lays
  each of its folders (`app`, `priv-app`, `framework`, and `etc`'s
  `permissions`, `sysconfig`, `default-permissions` and `preferred-apps`) over the same folder
  of `/product` and `/system_ext` before the zygote starts, so each app's privileged permission
  list stays in its own partition, where the package manager reads it. A partition's root is
  never laid over: its `bin` holds services, which would fail to start through the overlay.
  The image adds those folders where the build lacks them, so each overlay finds both layers. The product `build.prop` imports
  `/system/etc/dsa/gapps-${ro.boot.dsa.gapps:-off}.prop` just before the ARM translation
  import: the "on" file sets the fingerprint of the oem partition, which the image does not
  have, so the boot is an upgrade and the package manager installs the new system apps for every
  user and grants their default permissions. It also sets `ro.control_privapp_permissions=log`:
  a privileged permission that no `privapp-permissions` list names is then logged and granted,
  where `enforce` stops `system_server` at every boot. The host's image carries a list of every
  permission its privileged apps ask for (`dsa-gapps-privapp-permissions.xml` in each
  partition's `etc/permissions`), so the log should stay empty. `sepolicy/gapps.cil` lets init
  lay the image over.
- The add-on images a user installs (the Google apps, an ARM translation package) are on the
  user's add-on disk, which the host builds, one per user, and attaches to the VM read only.
  Its one partition, `dsa_addons`, is an erofs file system holding each image as a plain,
  uncompressed file (`gapps.img`, `arm-translation.img`). While the host attaches it, it puts
  `androidboot.dsa.addon_disk=on` in the bootconfig, and `dsa-addons.rc` mounts the partition
  at `/mnt/dsa-addons` at `post-fs`, before the scripts above mount the images from it with
  `directio`. The images used to be in the encrypted `/data`, where ext4 gives an encrypted file
  no direct I/O and every page read from an image was cached twice; on the add-on disk erofs
  reads each one directly. For one boot after an update from a DSA that kept an image in
  `/data` (`/data/vendor/dsa/gapps/system.img`, `/data/vendor/dsa/arm-translation/system.img`),
  the host says `androidboot.dsa.gapps_image=data` or `androidboot.dsa.arm_translation_image=data`
  and the scripts mount that copy; meanwhile the host reads it through dsa-channeld, puts it on
  the add-on disk, and has dsa-channeld delete it, so the next boot mounts it from the disk.
  `sepolicy/addons.cil` lets init mount the disk, and `check-image.py` checks the scripts.
- Cuttlefish's `seriallogging.rc` is removed. Its logger writes to a virtio console Hyper-V
  does not have, and init would restart it every five seconds for as long as the guest runs.
- `sepolicy/memory.cil` lets the audio HAL and the software codecs share memory through
  memfds. Refused, each falls back to an ashmem device the kernel does not have, and audio
  policy never starts. It also lets the media services' memfds reach apps and each other: an audio
  track's control block (audioserver's), a media extractor's and the media player service's
  buffers, and the camera service's queues. An app runs at an MLS level of its own, so those
  types are `mlstrustedobject`, as `system_server_tmpfs` is in the platform's policy; without
  that, binder refused every reply that carried one, and no app played a sound while SELinux
  was enforcing. `check-image.py` checks each of those rules
  (`make-image.py`'s `MEMFD_SHARING`), and the image workflow's boot check plays sound in an
  ordinary app with SELinux enforcing (`dsa/guest/media-probe/`, run by
  `dsa/host/platform/linux/tests/media.rs`).
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
- Every app starts at the density of the user's main monitor (130 dpi at a scale of 100%,
  196 at 150%). The image's kernel command line names 130 (`androidboot.dsa.display_dpi` for the
  hwcomposer, `androidboot.lcd_density` for SurfaceFlinger), and the host replaces both at each
  boot. `dsa-display.rc` sets `ro.sf.lcd_density` from it, and no `build.prop` sets that
  property, since a `build.prop` value would come first and hold every boot at 130. An app
  that started at another density than its window's monitor would be relaunched as soon as
  the window asked for the right one.
- DSA's primary audio module (`/vendor/bin/hw/dsa-audio`, from `dsa/guest/audio/`) is added when
  `--audio` names it. It serves `IModule/default` in the audio HAL's domain and sends the
  guest's sound to the host, which plays it on the user's default output device. The primary
  module leaves `audio_policy_configuration.xml`, so the image's own audio service no longer
  makes one that plays nowhere, and keeps the remote submix and Bluetooth modules.
  `sepolicy/audio.cil` lets the audio HAL reach the host over vsock.
- The agent (`dsa/guest/agent/`) is added as a privileged app in
  `/system_ext/priv-app/DsaAgent/`, with its privileged permission list, and is signed with
  the platform key. It tells the host about each app task, so each gets its own window.
  `sysconfig-dsa-agent.xml` goes with it to `/system_ext/etc/sysconfig/`: Android's backup
  manager uses a backup transport only when a sysconfig file lists it, and the agent's keeps
  app data in the host's backup folder once the user turns backup on (it is off until then).
- KeyMint's root key comes from the host, which keeps it sealed by the computer's TPM (the
  Secure Enclave on a Mac), and by the user's credential store only where there is none, so a
  copy of the guest's disk opens none of Android's keys. `dsa-keymint-key.rc` makes the
  tmpfs folder `/dev/dsa_keymint` (mode 0700, owned by `nobody`, KeyMint's user) on init and
  starts `dsa-channeld --keymint-key` (`dsa-channeld.rc`), a oneshot that runs as `nobody` in
  its own domain. It asks the host on vsock port 5604, past the control channel's handshake,
  and writes `root_key` there: `key <hex>`, or `none <reason>` when the host has none or cannot
  be reached within 25 seconds. The key never reaches the user's disk. `channeld.cil` labels the
  folder `dsa_keymint_key_file`, which only that domain writes and only KeyMint
  (`hal_keymint_server`) reads; `check-image.py` refuses an image that lets any other domain
  read it.
- The root certificates a user or administrator added to the host (an internet filter's or a
  company proxy's) become system certificates, beside Android's own. Android reads its system
  certificates from `/apex/com.android.conscrypt/cacerts` and `/system/etc/security/cacerts`,
  which are read only. `dsa-cacerts.rc` makes the tmpfs folder `/dev/dsa_cacerts` at
  post-fs-data, has `dsa-channeld --cacerts-setup` copy both folders into it, and binds each
  copy over the original before the zygote starts, so every app's mount namespace has them.
  The daemon then writes each root the host sends there, named as Android names its own
  (`<subject hash>.<n>`), and takes away those the host no longer names, or all of them when
  the user turns the setting off. It lists what it added in `/dev/dsa_cacerts/host-added`, so
  Android's own files are never touched. Being one tmpfs, a change shows in every app at once,
  and the agent then has the running apps forget the trust anchors they looked up.
  `cacerts.cil` labels the folder `dsa_cacerts_file`, which only the daemon writes and every
  domain reads; `check-image.py` refuses an image that lets any other domain write it.
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
