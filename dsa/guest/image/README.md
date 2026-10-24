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

```
dsa/guest/image/fetch.sh 17
dsa/guest/image/make-keys.sh .scratch/image-keys
uv run dsa/guest/image/make-image.py --major 17 --keys .scratch/image-keys
uv run dsa/guest/image/check-image.py dsa/target/image/17.0.0-16373615 .scratch/image-keys
```

It needs `gdisk` and `qemu-utils` (Debian and Ubuntu package names), Java and OpenSSL.

## What changes

- The launcher, SystemUI and the keyboard app are removed: the desktop is the shell, and
  text input goes through the host's input method.
- The build becomes a user build: not debuggable, adb only when authorised.
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
