# kernel

The kernel of the Android VM. DSA builds and ships it, so users never build or replace a
kernel, and the user's WSL kernel is not touched.

| File | What it is |
|---|---|
| `wsl-kernel.tag` | The WSL kernel tag the source comes from |
| `android.config` | The options merged on top of that tag's `Microsoft/config-wsl` |
| `build.sh` | Fetches the source into `.scratch/`, merges the configuration, checks it and builds `bzImage` |

## Why the WSL kernel

The WSL kernel is already tuned for this kind of VM: it carries the Hyper-V drivers
(VMBus, storage, networking, sockets, the balloon driver with free page reporting) and
the configuration Microsoft maintains for a lightweight Linux VM on Windows. Android's
needs come on top as one fragment, so following a new WSL tag is a one-line change.

The source is used unmodified. Only the configuration differs, which keeps the difference
from Microsoft's kernel small enough to read in one file.

## What the fragment adds

- **Binder and binderfs, built in.** Built in, binder uses the kernel's internal symbols
  directly. An out-of-tree module on the stock WSL kernel cannot: the driver needs about a
  dozen kernel symbols that the WSL kernel does not export to modules.
- **What Android's userspace expects:** wake locks, dma-buf heaps for graphics buffers,
  utilisation clamps, uinput and evdev, file-based encryption and fs-verity, erofs and f2fs,
  dm-crypt, quotas, and the eBPF and netfilter pieces netd uses.
- **Everything built in.** The VM boots one image with no modules, so options
  `config-wsl` builds as modules, and their dependencies, are set to `y`.
- **Its own release name** (`-dsa`), so it is never mistaken for the WSL kernel.

ashmem is not enabled and does not exist in this kernel; Android uses memfd.

Two options of Android's own kernel are not in the upstream source: `DM_DEFAULT_KEY`
(metadata encryption) and `INCREMENTAL_FS` (streaming app installs). The guest image is
configured not to need them.

## Building

```
dsa/kernel/build.sh --config-only   # merge and check, about a minute
dsa/kernel/build.sh                 # also build bzImage, tens of minutes
```

The check fails when any line of `android.config` did not survive the merge, which is how
Kconfig reports a missing dependency: by silently dropping the option. It also fails if
ashmem is enabled or memfd is not.

CI runs the full build in "Kernel test" (`.github/workflows/test-kernel.yml`) when these
files change, by hand, and weekly. The weekly run warns when a newer WSL kernel tag
exists. To try one, run the workflow with its `tag` input, then update `wsl-kernel.tag`.
