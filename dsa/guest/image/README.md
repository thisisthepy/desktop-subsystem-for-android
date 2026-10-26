# guest/image

Makes the Android image the VM boots, without building AOSP: it takes the build inputs
Google CI publishes for the Cuttlefish x86_64 device, changes them for DSA and re-signs
them with the project's keys.

| File | What it is |
|---|---|
| `inputs.json` | The pinned inputs: per Android version, the CI build number and the SHA-256 of its `target_files` and `otatools` |
| `fetch.sh` | Downloads one version's inputs into `.scratch/aosp-inputs/<build>/` and checks them |
| `make-keys.sh` | Makes a set of signing keys (CI makes a throwaway set each run) |
| `pack-keys.sh`, `unpack-keys.sh` | Split the key set into repository secrets and put it back |
| `make-image.py` | Changes, re-signs and lays out the image |
| `check-image.py` | Checks a made image |
| `fstab.dsa` | The guest's mount table on Hyper-V |
| `console-logcat.rc` | The service that writes Android's log to the serial console |
| `unavailable-features.xml` | Hardware features the VM lacks, withdrawn from the system |
| `dsa-channeld.rc` | The service that runs the control channel daemon, and the trigger that drops the guest's caches when it asks |
| `dsa-memory.rc` | Turns on the zram swap and tunes the kernel for it |
| `dsa-channel-manifest.xml` | The vendor manifest entry declaring the daemon's binder service |
| `sepolicy/` | Policy added to the image: vsock for graphics, the control daemon's domain and binder service, and the memory controls (`memory.cil`) |

```
dsa/guest/image/fetch.sh 17
dsa/guest/image/make-keys.sh .scratch/image-keys
ANDROID_NDK_HOME=/path/to/ndk dsa/guest/channeld/build-android.sh .scratch/channeld
dsa/guest/hwcomposer/build.sh .scratch .scratch/build-hwcomposer
(cd dsa/guest/agent && ./gradlew assembleRelease)
uv run dsa/guest/image/make-image.py --major 17 --keys .scratch/image-keys --channeld .scratch/channeld/dsa-channeld \
    --hwcomposer .scratch/build-hwcomposer/dsa-hwcomposer \
    --agent dsa/guest/agent/build/outputs/apk/release/dsa-agent-release-unsigned.apk
uv run dsa/guest/image/check-image.py dsa/target/image/17.0.0-16373615 .scratch/image-keys
```

It needs `gdisk` and `qemu-utils` (Debian and Ubuntu package names), Java and OpenSSL.

## What changes

- The launcher, SystemUI and the keyboard app are removed: the desktop is the shell, and
  text input goes through the host's input method.
- Apps a desktop has no use for are removed too (`UNNEEDED_APPS` in `make-image.py`, each
  with its reason): telephony apps without a modem, screen savers and wallpaper pickers, the
  launcher's search widget, Bluetooth MIDI, and developer and demo apps. Each is a leaf that
  nothing else needs; WebView, the providers, PackageInstaller, PermissionController,
  DocumentsUI and the other apps that apps call stay. `make-image.py` prints each file it
  removes.
- The guest keeps its memory small, so the host gets back what it does not use. It swaps to
  zram of half its memory (`fstab.dsa`, `dsa-memory.rc`), and lmkd watches the swap as well
  as memory pressure. When no app task has been open for a minute, or most of its memory is
  cache, the control daemon sets `vendor.dsa.memory.reclaim`, and init drops the clean caches
  and compacts memory, which free page reporting then hands back to Hyper-V. Only the daemon
  may set the property, and only init's vendor scripts may write those kernel settings.
- The oemlock, UWB, Bluetooth and Thread HAL APEXes are removed. Their devices
  (Cuttlefish's virtio consoles, radios) do not exist on Hyper-V, they abort in a loop, and
  system_server waits for the oemlock HAL forever. Bluetooth comes back as a host HAL.
- `unavailable-features.xml` withdraws the Bluetooth features, so system_server does not
  start a Bluetooth stack that has no controller to talk to.
- `wifi.rc` no longer sets up its kernel trace instance when boot completes. The VM has no
  Wi-Fi device to trace, and on Hyper-V those tracefs writes hold up lmkd's own tracepoint
  setup past its two second watchdog, which then kills the app the user just opened.
- The build becomes a user build: not debuggable, adb only when authorised.
- The control channel daemon (`/vendor/bin/dsa-channeld`) is added and runs in its own
  SELinux domain, the only one that may read `/proc/bootconfig`, where the host puts the
  boot's secret. It connects to the host, which makes the session active.
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
- The agent (`dsa/guest/agent/`) is added as a privileged app in
  `/system_ext/priv-app/DsaAgent/`, with its privileged permission list, and is signed with
  the platform key. It tells the host about each app task, so each gets its own window.
- The published kernel modules are dropped. They belong to Google's kernel; DSA boots its
  own kernel with the drivers built in.
- `fstab.dsa` replaces the mount table. All partitions are on the user's one Android
  disk, found by the super partition's UUID, because Android creates its partition links
  for Hyper-V's SCSI disks only on that disk.
- Android's log (`logcat`) goes to the serial console, which the host keeps as the VM's
  console log (`C:\ProgramData\DSA\logs\android-*.log` on Windows). Cuttlefish writes it to a
  virtio console, which Hyper-V does not have, and a user build has no adb to read it with.
- Every APK, APEX and verified boot image is re-signed. The published build is signed with
  AOSP's test keys, whose private halves are public.

## What comes out

In `dsa/target/image/<version>/`:

- `system.vhdx`: the read-only system disk (dynamic). It holds `super` with Android and
  its verified boot data, and every user's VM attaches it as it is.
- `data.vhdx`: the template of a user's data disk (dynamic). It holds empty `misc`,
  `metadata` and `userdata`, which Android writes and formats on first boot. `misc` is
  here because the boot control HAL writes the boot state into it. The service copies the
  template for a new user and keeps each user's disk across updates.
- `initrd.img`: the vendor ramdisk followed by the generic one.
- `bzImage`, with `--kernel`: the guest kernel (`dsa/kernel/`). The host boots an image's
  own kernel when it has one, so a kernel update ships with the image.
- `image.json`: the version, the source build, the kernel command line, and the files'
  SHA-256.
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
