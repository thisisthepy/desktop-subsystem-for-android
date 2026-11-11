# kernel

The kernel of the Android VM. DSA builds and ships it, so users never build or replace a
kernel, and the user's WSL kernel is not touched.

| File | What it is |
|---|---|
| `wsl-kernel.tag` | The WSL kernel tag the source comes from |
| `android.config` | The options merged on top of that tag's `Microsoft/config-wsl` (x86_64) or `Microsoft/config-wsl-arm64` (arm64) |
| `android-arm64.config` | The options the arm64 kernel adds after `android.config` |
| `virtio.config` | The virtio drivers the macOS host's VM needs, merged last for both architectures |
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

The same two kernels boot on macOS. Apple's Virtualization framework loads the kernel
itself: the uncompressed arm64 `Image` on Apple silicon, the `bzImage` on Intel Macs. Its
devices are virtio devices on PCI (block, network, console `hvc0`, sockets, balloon,
entropy), which `virtio.config` builds in. Only one guest-to-host socket transport may
register, so `patches/vsock-hyperv-first.patch` links Hyper-V's first: it registers only
when VMBus is there and otherwise leaves the place to virtio's.

The patches touch only architecture-neutral code (VMBus's ACPI probe, which Hyper-V uses on
both architectures, the command-line partition parser, cgroup v1, wakeup sources, shmem,
free page reporting), so the same set applies to both builds.

Memory the guest frees goes back to the host at once. Upstream free page reporting waits two
seconds before each pass, and again after each pass that used up its budget, so a large
free reached the host slice by slice over many seconds; `patches/page-reporting-no-delay.patch`
starts every pass at once. The host chooses the smallest piece reported
(`page_reporting.page_reporting_order` on the command line): 4 KiB where it backs memory in
4 KiB pages, so a freed page need not wait for compaction to gather a whole block. A host
that faults memory in larger pieces (Hyper-V's 64 KiB fault clusters, a Linux host's huge
pages) takes reported neighbours back in with a page the guest touches again, and the guest
never offers those a second time. `patches/page-reporting-rereport.patch` adds
`page_reporting.rereport`: writing 1 empties the per-CPU free lists and reports every free
page again. The guest's idle reclaim writes it after it has compacted memory.

zram writes pages that stay cold for long to a backing file on the guest's data disk
(`CONFIG_ZRAM_WRITEBACK`), chosen by the time each page was last used
(`CONFIG_ZRAM_TRACK_ENTRY_ACTIME`). Upstream clears that time when it compresses a page again,
so every page zstd had compressed again looked unused since boot;
`patches/zram-recompress-keeps-access-time.patch` keeps it.

## Why the WSL kernel

The WSL kernel is already tuned for this kind of VM: it carries the Hyper-V drivers
(VMBus, storage, networking, sockets, the balloon driver with free page reporting) and
the configuration Microsoft maintains for a lightweight Linux VM on Windows. Android's
needs come on top as one fragment, so following a new WSL tag is a one-line change.

The source is configured from Microsoft's baseline. A small checked-in patch extends the
command-line partition parser so Hyper-V can provide Android the partition UUID and name
metadata its virtual SCSI uevents omit. The same patch lets `blkdevparts=` name a disk by
the GUID in its GPT header as well as by its kernel name: Hyper-V names disks `sda`, `sdb`
in probe order, which changes when the host attaches another disk.

## What the fragment adds

- **Binder and binderfs, built in.** Built in, binder uses the kernel's internal symbols
  directly and creates `/dev/binder`, `/dev/hwbinder` and `/dev/vndbinder` for Android's
  early services. An out-of-tree module on the stock WSL kernel cannot: the driver needs
  about a dozen kernel symbols that the WSL kernel does not export to modules.
- **The su guard, built in.** `patches/dsa-su-guard-lsm.patch` adds a small security module,
  `security/dsa_su`, so the image's su binary exists only for the Android uids the host has
  allowed. For any other task a lookup, stat, access, open or exec of a watched su inode
  returns `ENOENT`. The host's control daemon drives it through
  `/sys/kernel/security/dsa_su/control`: it names the su inode to watch, the uids that may
  see it, and whether they may (enabled) or nobody may (disabled). The daemon has it hide su
  from every app when it starts, so a guest with root elevation off has no su for any app.
  One write is one change, and a watched path the image does not have is skipped. The module
  is listed in `CONFIG_LSM`, before `selinux`, or it would never initialise. Built in,
  because the image ships no kernel modules.
- **What Android's userspace expects:** wake locks, dma-buf heaps for graphics buffers,
  utilisation clamps, uinput and evdev, file-based encryption and fs-verity, erofs and f2fs,
  dm-crypt, quotas, and the eBPF and netfilter pieces netd uses.
- **Everything built in.** The VM boots one image with no modules, so options
  `config-wsl` builds as modules, and their dependencies, are set to `y`.
- **Bootconfig**, which carries each boot's control channel secret from the host. The
  guest reads it from `/proc/bootconfig`, which apps cannot read.
- **Its own release name** (`-dsa`), so it is never mistaken for the WSL kernel.
- **pstore on a disk (pstore/blk)**, so the kernel's last messages, an oops's report and
  Android's last log lines (pmsg) are kept on a small disk and shown in `/sys/fs/pstore` at
  the next boot. `dsa logs collect` carries what is there, and the guest then clears it.

## The pstore disk

A panic ends the VM: HCS runs it with `StopOnReset`, and `panic=-1` restarts the guest at
once, so the next start is a new VM with empty memory. A log kept in RAM (ramoops) would be
gone by then, so the kernel keeps it on a disk instead.

The host attaches `pstore.vhd`, beside the user's Android disk, as the last disk of the VM
(`dsa/host/platform/windows/src/pstore_disk.rs`). It is a fixed VHD of 4 MiB, so it never
grows, with one GPT partition whose UUID is fixed. The host names that partition on the kernel
command line:

```
pstore_blk.blkdev=PARTUUID=5f8b3c2e-9a41-4d6e-b07c-1e2d3f4a5b6c pstore_blk.best_effort=y
pstore_blk.kmsg_size=64 pstore_blk.console_size=256 pstore_blk.pmsg_size=256
```

- **Found by UUID:** the guest names its disks in the order they finish probing, so the
  partition UUID finds the disk whatever its name.
- **Waiting for the disk:** pstore/blk looks its disk up at `late_initcall`, and Hyper-V's
  storage and SCSI disk drivers probe asynchronously, so the disk may not exist yet.
  `patches/pstore-blk-wait-for-disk.patch` waits for the probes started so far first, as
  mounting the root file system does.
- **How it is split:** the console and pmsg take 256 KiB each, and the rest of the partition
  (about 2.5 MiB) holds oops records of 64 KiB, the oldest replaced first.
- **What it costs:** no guest memory is reserved, unlike ramoops' region.

What a panic leaves, and what it does not: pstore/blk writes through the normal block path,
which cannot run once the kernel has panicked (Hyper-V's storage driver has no panic write).
The console and pmsg zones are written as they fill, and from atomic context about half a
second later, so they hold the log up to just before the panic. An oops the kernel survives is
written whole. The panic's own report is not; it reaches the host on the serial console, which
the host keeps in the console log and `dsa logs collect` carries (`host/android-console.log`).

Once the host has the records, the guest unlinks them from `/sys/fs/pstore`, which erases them
from the disk, so the next collection does not carry them again.
ashmem is not enabled and does not exist in this kernel; Android uses memfd.

Two options of Android's own kernel are not in the upstream source: `DM_DEFAULT_KEY`
(metadata encryption) and `INCREMENTAL_FS` (streaming app installs). The guest image is
configured not to need them.

The pstore disk works the same on arm64: it is found by its partition UUID, so no memory
region has to be reserved on either architecture.

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

CI builds a kernel once per input. A kernel built from the same files here, the same
merged configuration and the same build tools is kept as an Actions cache, and the next
run that needs it, a "Kernel test" or an "Image release", takes it from there
(`.github/actions/guest-kernel`). The configuration is still merged and checked on every
run, and the source checked out, so the release bundles the source of exactly the kernel
it ships.
