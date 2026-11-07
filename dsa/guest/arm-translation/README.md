# guest/arm-translation

Most Android apps ship native code for ARM only. The guest is x86_64, so such an app installs
and runs only through a translator that plugs into Android as its native bridge. The ones
Android products use (Houdini, Google's libndk_translation) are proprietary, and upstream
Berberis translates RISC-V only, so each x86_64 image carries an open source default
(Digitalis, below), which Android uses from its first start. The user can replace it with a
package: one they build, or Google's, which the host makes on request from Google's own
emulator image (`dsa arm-translation fetch google`). Removing that package brings the default
back.

A package is a folder holding:

| File | What it is |
|---|---|
| `arm-translation.json` | `name`, `version`, `library` (the bridge in `system/lib64`), `abis` (`["arm64-v8a"]`), `license` |
| `system.img` | an erofs image of `system/lib64`, `system/bin` and `system/etc`, with `system/lib64/libdsa_native_bridge.so` naming the bridge |

```
dsa arm-translation install <folder>   # sends it to Android, used from the next start
dsa arm-translation fetch google       # makes and installs a package of Google's translator
dsa arm-translation status             # which translator Android uses, and the ABIs it installs for
dsa arm-translation remove             # back to the image's default translator, if it has one
dsa arm-translation use-default        # the same, refused where there is no default translator
dsa settings set arm-translation off   # keeps the package, boots without any translator
```

`fetch google` asks first, saying where the download comes from and that Google's Android SDK
License covers it (`--yes` agrees). It reads Google's list of Google APIs x86_64 system images,
downloads the one for the guest's API level, checks its size and SHA-1 against the list, and
takes only the translator's files out of the image's system partition:
`libndk_translation.so` and the `libndk_translation_*.so` libraries it loads, `lib64/arm64/`
and `bin/arm64/`, the arm64 program runner, the arm64 binfmt_misc rules,
`ld.config.arm64.txt` and the arm64 cpuinfo. They become a package like any other, named
"Google ARM translator". The download is deleted afterwards. DSA never ships or redistributes
those files.

At boot the host adds `androidboot.dsa.arm_translation=on` to the bootconfig while a
package is installed and the setting is on, and `androidboot.dsa.arm_translation=default`
while none is installed, the setting is on and the image has a default package. The image
then imports the native bridge and ABI properties (`arm-translation-on.prop`, or the same
lines as `arm-translation-default.prop`) and mounts that package over `/system`
(`dsa-arm-translation.rc`), then mounts binfmt_misc, which nothing else in Android mounts, and
registers the package's handlers so ARM64 programs an app starts run too. Without either
value, the image is unchanged and Android installs x86_64 apps only. The guest has no 32-bit
zygote, so only 64-bit ARM apps (`arm64-v8a`) can run. The host records what each boot was
told, so `dsa arm-translation status` says when a change waits for the next start.

## The default translator

The "Translator build" workflow (`.github/workflows/translator.yml`) builds Digitalis, an
Apache-2.0 ARM64 backend for Berberis, on GitHub's standard runners, for Android 16
(`android16`) and 17 (`android17`), runs its host tests (`berberis_arm64_host_tests`), and
keeps each version's package as an artifact. `make-image.py --arm-translation` puts that
package's `system.img` into each x86_64 image's system partition as
`/system/etc/dsa/arm-translation/default.img`, so it is signed and verified with the image,
updated with it, and shared by every user of the computer. `image.json` describes it under
`arm_translation` (its `system_image` names that path), which is how the host knows the image
has one. The release also carries the package folder as `arm-translation.zip`, which installs
like any other package; the host never downloads it. arm64 images carry none: ARM apps run
on them as they are, and `check-image.py` fails an arm64 image that has one.

A whole Digitalis tree is an AOSP checkout of about 300 GB once built, so the workflow
fetches only the projects `digitalis-lock.json` lists, each at an exact commit and the
large ones in part. The proxies link the framework libraries of DSA's pinned x86_64 image,
and the package takes ICU, BoringSSL and SQLite from the pinned arm64 image of the same
Android version. Everything the native bridge replaces (libc, the linker, the proxied
libraries) is built.

| File | What it is |
|---|---|
| `digitalis-lock.json` | The pins: per version, the Digitalis manifest and AOSP superproject commits, and every project the build fetches |
| `digitalis_lock.py` | Reads and checks the lock |
| `update-lock.py` | Moves the lock to the newest commits of each branch, or pins projects added to it (`--missing`) |
| `assemble-digitalis.py` | Lists the files the build makes, takes libraries from the pinned images, and assembles the bundle |
| `make-package.py` | Turns a translator's bundle into a package (needs `mkfs.erofs`) |
| `build-digitalis.sh` | Builds the same pins in a full Digitalis tree, in Digitalis' own container. It needs a full AOSP build's disk and hours |

```
uv run dsa/guest/arm-translation/update-lock.py --missing
dsa arm-translation install <folder with arm-translation.json and system.img>
```
