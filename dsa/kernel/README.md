# kernel

The kernel of the Android VM. DSA builds and ships it, so users never build or replace a
kernel, and the user's WSL kernel is not touched.

| File | What it is |
|---|---|
| `wsl-kernel.tag` | The WSL kernel tag the source comes from |
| `android.config` | The options merged on top of that tag's `Microsoft/config-wsl` (x86_64) or `Microsoft/config-wsl-arm64` (arm64) |
| `android-arm64.config` | The options the arm64 kernel adds after `android.config` |
| `patches/` | Small patches applied to the pinned source before configuration, the same for both architectures |
| `build.sh` | Fetches the source into `.scratch/`, merges the configuration, checks it and builds `bzImage` (x86_64) or `Image` (arm64) |

## Architectures

The kernel is built for two hosts:

- **x86_64**, a `bzImage` the host boots directly (`LinuxKernelDirect`).
- **arm64**, for ARM64 Windows PCs, an `Image` with its EFI stub. Hyper-V on ARM64 has no
  direct Linux boot, so its UEFI firmware starts the stub, which loads the initrd named on
  its command line, as WSL does on the same hosts. The machine is described by ACPI, and the
  console is a PL011 UART (`ttyAMA0`). `android-arm64.config` names what that boot, VMBus
  and free page reporting need, so the build fails if a WSL tag ever drops one.

The patches touch only architecture-neutral code (VMBus's ACPI probe, which Hyper-V uses on
both architectures, the command-line partition parser, cgroup v1, wakeup sources, shmem),
so the same set applies to both builds.

## Why the WSL kernel

The WSL kernel is already tuned for this kind of VM: it carries the Hyper-V drivers
(VMBus, storage, networking, sockets, the balloon driver with free page reporting) and
the configuration Microsoft maintains for a lightweight Linux VM on Windows. Android's
needs come on top as one fragment, so following a new WSL tag is a one-line change.

The source is configured from Microsoft's baseline. A small checked-in patch extends the
command-line partition parser so Hyper-V can provide Android the partition UUID and name
metadata its virtual SCSI uevents omit.

## What the fragment adds

- **Binder and binderfs, built in.** Built in, binder uses the kernel's internal symbols
  directly and creates `/dev/binder`, `/dev/hwbinder` and `/dev/vndbinder` for Android's
  early services. An out-of-tree module on the stock WSL kernel cannot: the driver needs
  about a dozen kernel symbols that the WSL kernel does not export to modules.
- **What Android's userspace expects:** wake locks, dma-buf heaps for graphics buffers,
  utilisation clamps, uinput and evdev, file-based encryption and fs-verity, erofs and f2fs,
  dm-crypt, quotas, and the eBPF and netfilter pieces netd uses.
- **Everything built in.** The VM boots one image with no modules, so options
  `config-wsl` builds as modules, and their dependencies, are set to `y`.
- **Bootconfig**, which carries each boot's control channel secret from the host. The
  guest reads it from `/proc/bootconfig`, which apps cannot read.
- **Its own release name** (`-dsa`), so it is never mistaken for the WSL kernel.
- **pstore with ramoops**, so the kernel's last messages, a panic's report and Android's last
  log lines (pmsg) are kept in a region of RAM and shown in `/sys/fs/pstore` after a warm
  restart. `dsa logs collect` carries what is there.

## The ramoops region

The host puts the region on the kernel command line it boots every x86_64 VM with
(`dsa/host/platform/windows/src/vm.rs`):

```
memmap=1M$0x10000000 ramoops.mem_address=0x10000000 ramoops.mem_size=0x100000
ramoops.record_size=0x20000 ramoops.console_size=0x40000 ramoops.pmsg_size=0x40000 ramoops.ecc=1
```

- **Where: 1 MiB at 256 MiB.** A Hyper-V VM's memory starts at 0 and is contiguous up to
  the gap below 4 GiB, so 256 MiB is RAM in every VM DSA makes (the smallest is far larger).
  It is above the kernel's own image, which the boot protocol loads at 16 MiB and which
  decompresses to well under 240 MiB, and below where the boot loader puts the initrd, as
  high under 2 GiB as it fits. `memmap=...$...` marks it reserved before the kernel uses any
  memory, so nothing else is ever placed there.
- **How it is split:** the panic and oops records take 128 KiB each, the console 256 KiB and
  pmsg 256 KiB; ECC lets the kernel tell a corrupted record from a good one.
- **What it costs:** 1 MiB of the guest's memory that free page reporting never hands back.
- **What it keeps, and what it does not:** RAM keeps the region only across a warm restart of
  the same VM. HCS runs the VM with `StopOnReset`, so a guest that panics (`panic=-1` restarts
  it at once) ends its VM, and the next start is a new VM with empty memory. Until the host
  keeps a VM across a reset, the panic's report reaches the host on the serial console
  instead, which the host keeps in the console log and `dsa logs collect` carries
  (`host/android-console.log`).

ashmem is not enabled and does not exist in this kernel; Android uses memfd.

Two options of Android's own kernel are not in the upstream source: `DM_DEFAULT_KEY`
(metadata encryption) and `INCREMENTAL_FS` (streaming app installs). The guest image is
configured not to need them.

On arm64 the region is reserved by name instead, with the same split:

```
reserve_mem=1M:4096:dsa_ramoops ramoops.mem_name=dsa_ramoops ...
```

`memmap=` exists only on x86, and a Hyper-V ARM64 VM's memory map is the firmware's, so a
fixed address is not known to be RAM there. `reserve_mem=` lets the kernel pick the region
early in boot and ramoops find it by name.

## Building

```
dsa/kernel/build.sh --config-only               # merge and check, about a minute
dsa/kernel/build.sh                             # also build bzImage, tens of minutes
dsa/kernel/build.sh --arch arm64 [--config-only] # the same for arm64 (Image)
```

The check fails when any line of `android.config` did not survive the merge, which is how
Kconfig reports a missing dependency: by silently dropping the option. It also fails if
ashmem is enabled or memfd is not.

An arm64 kernel builds natively on an arm64 machine and is cross-compiled with
`aarch64-linux-gnu-` anywhere else. Each architecture builds out of tree in its own
directory under `.scratch/kernel/<tag>/`, so both come from one checkout.

CI runs the full build for both architectures in "Kernel test"
(`.github/workflows/test-kernel.yml`, the arm64 one on a native arm64 runner) when these
files change, by hand, and weekly. The weekly run warns when a newer WSL kernel tag
exists. To try one, run the workflow with its `tag` input, then update `wsl-kernel.tag`.
