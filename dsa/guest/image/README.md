# guest/image

Makes the Android image the VM boots, without building AOSP: it takes the build inputs
Google CI publishes for the Cuttlefish x86_64 device, changes them for DSA and re-signs
them with the project's keys.

| File | What it is |
|---|---|
| `inputs.json` | The pinned inputs: per Android version, the CI build number and the SHA-256 of its `target_files` and `otatools` |
| `fetch.sh` | Downloads one version's inputs into `.scratch/aosp-inputs/<build>/` and checks them |
| `make-keys.sh` | Makes a set of signing keys (CI makes a throwaway set each run) |
| `make-image.py` | Changes, re-signs and lays out the image |
| `check-image.py` | Checks a made image |
| `fstab.dsa` | The guest's mount table on Hyper-V |

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
- Every APK, APEX and verified boot image is re-signed. The published build is signed with
  AOSP's test keys, whose private halves are public.

## What comes out

In `dsa/target/image/<version>/`:

- `disk.vhdx`: the template of a user's Android disk (dynamic). It holds `super` with
  Android, its verified boot data, and empty `misc`, `metadata` and `userdata`, which
  Android formats on first boot. The service copies it for a new user, and on an update
  writes the new image's Android partitions into each user's disk.
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
