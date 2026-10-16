# A dedicated lightweight VM for Android

Question: binder cannot be loaded as a module into the stock WSL kernel (see
`experiments/wsl-binder-module/`), and replacing the WSL kernel would change every WSL
distribution the user has. Can Android instead run in a lightweight VM of its own, with a
kernel that has binder built in, without bringing back the costs that made WSA heavy?

Answer, as of 2026-10-16: **yes, with the same Windows machinery WSL uses.** The pieces WSL
relies on to stay light (memory handed back to the host, sparse disks, Hyper-V sockets, a
directly booted kernel) are all documented options of the Host Compute System (HCS) API.
They are not private to WSL. The price is a privileged service, since only administrators
may create VMs through HCS, plus the work of configuring the VM ourselves.

Nothing here has run on Windows yet. The findings come from the HCS documentation and from
WSL's own source, which creates its VM through the same API. The checks that need a Windows
machine are listed at the end.

## Sources

| Source | Version |
|---|---|
| HCS documentation, [MicrosoftDocs/Virtualization-Documentation](https://github.com/MicrosoftDocs/Virtualization-Documentation/tree/main/virtualization/api/hcs) `virtualization/api/hcs/` | commit `7db63dd9` (2026-10-07) |
| WSL source, [microsoft/WSL](https://github.com/microsoft/WSL) (MIT) | `master` at `9a32b5ec` (2026-10-15) |
| WSL kernel configuration, [microsoft/WSL2-Linux-Kernel](https://github.com/microsoft/WSL2-Linux-Kernel/tree/linux-msft-wsl-6.18.54.1) `Microsoft/config-wsl` | `linux-msft-wsl-6.18.54.1` |

## Findings

### 1. HCS is a public API, and it is what WSL uses

- HCS (`computecore.dll`, `ComputeCore.h`) "provides the functionality to start and control
  VMs and containers" (`Overview.md`). `HcsCreateComputeSystem` is documented with a minimum
  client of Windows 10 1809 (`reference/HcsCreateComputeSystem.md`).
- WSL creates its VM with `HcsCreateComputeSystem` (`src/windows/common/hcs.cpp`), from a JSON
  document built in `WslCoreVm::GenerateConfigJson` (`src/windows/service/exe/WslCoreVm.cpp`).
- WSL 2 runs on Windows 11 Home with only the Virtual Machine Platform feature, and it goes
  through HCS. So HCS is present wherever WSL 2 works. This is an inference from those two
  facts, not a documented statement, and is listed under the checks below.

### 2. Only administrators can create a VM, so DSA needs a service

`Reference/HCSHResult.md`, `HCS_E_ACCESS_DENIED`: "Only administrators or users that are
members of the Hyper-V Administrators user group are permitted to access virtual machines
or containers."

WSL handles this the same way: the VM is created by `wslservice`, which runs as SYSTEM, on
behalf of the user's session. DSA would do the same. The installer registers a service that
creates and stops the Android VM, and the companion process in the user's session talks to
that service. This is new work compared with the WSL path, where WSL's service did it for us.

### 3. Memory goes back to the host the way it does for WSL

WSL sets these in `GenerateConfigJson`:

```
ComputeTopology.Memory.AllowOvercommit       = true   // backed by the pagefile, not pinned
ComputeTopology.Memory.EnableDeferredCommit  = true   // commit charged on first touch
ComputeTopology.Memory.EnableColdDiscardHint = true   // guest can hand free pages back
ComputeTopology.Memory.BackingPageSize       = Small  // newer builds, with a 64 KiB fault cluster
```

and adds `page_reporting.page_reporting_order=5` (128 KiB) or `9` (2 MiB) to the kernel
command line, depending on the Windows build. The schema documents each option
(`SchemaReference.md`, `Memory`): `AllowOvercommit` "the VM's memory is backed by the Windows
pagefile rather than physically backed, statically allocated memory", and
`EnableColdDiscardHint` "allowing it to trim non-zeroed pages from the working set".

On the guest side this needs `CONFIG_HYPERV_BALLOON` and `CONFIG_PAGE_REPORTING`, both set in
`config-wsl`. WSL also runs an idle-time reclaimer in its init (`src/linux/init/util.cpp`)
that drops cold page cache and compacts memory so that free page reporting can return large
blocks. Android has its own memory management (lmkd and the cached-process policy), so the
equivalent for us is tuning those, which is already in the plan for idle memory.

WSA's habit of keeping memory is therefore not inherent to a VM. A VM configured like WSL's
returns memory the same way.

### 4. The kernel boots directly, with our command line

`Chipset.LinuxKernelDirect` takes `KernelFilePath`, `InitRdPath` and `KernelCmdLine`
(schema 2.2). WSL uses it on x64. On ARM64 WSL boots through UEFI instead ("Linux kernel
direct boot is not yet supported on ARM64", `WslCoreVm.cpp`), which matters for Windows on
ARM later.

With a kernel of our own, binder and binderfs are built in, and Android's init can be the
first process. No Linux distribution and no LXC sit in between.

### 5. Hyper-V sockets are configured per VM, which settles the VM ID question

The `HvSocket` device takes `DefaultBindSecurityDescriptor`, `DefaultConnectSecurityDescriptor`
and a per-service `ServiceTable` (`SchemaReference.md`). WSL sets both defaults to an SDDL
string that allows SYSTEM and the current user. Because the VM is ours, we also know its ID
when we create it, so the open question from the WSL path (how to find the WSL VM's ID) does
not arise. The transport in `dsa/host/platform/windows/` already takes the VM ID as an
argument.

### 6. Disks: read-only system image, sparse data disk

SCSI attachments take `ReadOnly` and `AlwaysAllowSparseFiles`. WSL attaches its system and
module images read only, and its distribution disks can be sparse (`experimental.sparseVhd`
in `WslCoreConfig.h`). For Android:

- the system and vendor images are read-only files that an update replaces whole;
- user data goes on a sparse VHDX, trimmed from the guest with discard so that the file
  shrinks when data is deleted, or on a host folder (next item).

The disk complaint about WSA is mostly about a data disk that only grows. Sparse files plus
guest discard address it; it needs measuring on a real machine.

### 7. Host folders can be shared into the VM

The schema has a `Plan9` device with shares (`Plan9Share`: `Name`, `AccessName`, `Path`,
`Port`) and `VirtualSmb`. WSL serves its Windows drive mounts over Plan9, and over virtio-fs
when enabled; `config-wsl` has `CONFIG_9P_FS`, `CONFIG_NET_9P_VIRTIO` and `CONFIG_VIRTIO_FS`.
Sharing Downloads or Pictures into Android, and opening host files in Android apps, can use
the same mechanism.

### 8. WSL's own container VMs do not help with binder

WSL has a newer interface, `IWSLCVirtualMachineFactory` (`src/windows/service/inc/wslc.idl`),
that creates extra HCS VMs per session, with a display name for the `vmmem` process. It
always boots WSL's kernel (`HcsVirtualMachine.cpp` takes the kernel from WSL's own install).
The only image override, `RootVhdOverride`, is marked "used for debugging purposes only". So
it cannot carry binder either.

## What changes in the design

| Area | WSL container | Dedicated VM |
|---|---|---|
| Kernel | WSL's, without binder | Ours: a WSL kernel tag plus Android's kernel configuration, binder built in |
| What runs on it | WSL VM, a distribution, LXC, Android | Android's init directly |
| Effect on the user's WSL | None, or every distribution if the kernel is replaced | None |
| Isolation | Shares a kernel with the user's distributions | Its own kernel |
| VM creation | WSL's service | A DSA service running as SYSTEM |
| Memory return | WSL's settings | The same HCS settings, set by us |
| Memory when the user also runs WSL | One VM | Two VMs: one more kernel and its base memory, to be measured |
| Hyper-V socket VM ID | Must be discovered | Known, since we create the VM |
| Graphics | Vulkan remoting (no dxg needed) | Unchanged |
| Transport, protocol, session, compositor, HAL, agent | | Unchanged |

`GuestLauncher` in `dsa/host/core` already separates where the guest runs from the session.
A dedicated VM is one more launcher next to `WslLauncher`, and the WSL container can stay as
a second backend for the day the WSL kernel carries binder.

The kernel can start from a WSL kernel tag, which already carries the Hyper-V drivers
(`CONFIG_HYPERV`, storage, networking, sockets, balloon) tuned for this VM type, with
Android's required options (binder, binderfs and the rest of AOSP's kernel configuration
fragments) added as one fragment. Starting from Android's common kernel and adding the
Hyper-V drivers is the alternative. Which base to use is a decision to make with the kernel
work, not here.

## Checks that need Windows

1. HCS on Windows 11 Home: a SYSTEM service calls `HcsCreateComputeSystem` with a minimal
   `LinuxKernelDirect` VM on a Home machine with only Virtual Machine Platform enabled.
2. Memory return: with `AllowOvercommit`, `EnableDeferredCommit` and `EnableColdDiscardHint`
   set and page reporting on in the guest, the `vmmem` process shrinks after the guest frees
   memory. Record the numbers and the machine.
3. Hyper-V sockets: the host connects to and accepts from the VM with the VM's own ID and the
   security descriptor above, without a `GuestCommunicationServices` registry entry.
4. Sparse data disk: after deleting data in the guest and trimming, the VHDX file shrinks.
5. Running beside WSL: the Android VM and the WSL VM run at the same time, and the extra
   memory of the second kernel is measured.
