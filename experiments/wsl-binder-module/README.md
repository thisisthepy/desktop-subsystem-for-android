# Binder as an external module on the stock WSL kernel

Question: can binder (with binderfs) be loaded into the kernel that WSL ships, as an
external module, without replacing that kernel?

Answer, as of 2026-10-16: **not with the upstream driver.** The driver links against
kernel symbols that the stock WSL kernel does not export to modules. Only a patched copy of
the driver can load, and each way of patching it has a cost. The options are at the end;
choosing one is the owner's decision, so nothing under `dsa/kernel/` or
`.github/workflows/` is built on top of this yet.

Reproduce with `check-symbols.sh [tag]` in this folder. It fetches the tag into
`.scratch/wsl-binder-module/<tag>/`, runs `make modules_prepare` (not a full kernel build),
compiles the binder sources out of tree and lists the symbols they need that nothing
exports.

## What was checked

| Item | Version |
|---|---|
| WSL kernel, current | [`linux-msft-wsl-6.18.54.1`](https://github.com/microsoft/WSL2-Linux-Kernel/tree/linux-msft-wsl-6.18.54.1) (commit `d504d40a`) |
| WSL kernel, previous series | [`linux-msft-wsl-6.6.123.2`](https://github.com/microsoft/WSL2-Linux-Kernel/tree/linux-msft-wsl-6.6.123.2) (commit `a07f9ea8`) |
| WSL itself | [`microsoft/WSL`](https://github.com/microsoft/WSL) `master` at `9a32b5ec` (2026-10-15), which pins `Microsoft.WSL.Kernel` 6.18.54.1 in `packages.config` |
| `.wslconfig` reference | [`WSL/wsl-config.md`](https://github.com/MicrosoftDocs/WSL/blob/main/WSL/wsl-config.md) in MicrosoftDocs/WSL, read 2026-10-16 |

## Findings

### 1. Binder is not in the stock kernel and cannot be selected as a module

- `Microsoft/config-wsl` has `# CONFIG_ANDROID_BINDER_IPC is not set` (both tags).
- Upstream `drivers/android/Kconfig` declares `ANDROID_BINDER_IPC`, `ANDROID_BINDERFS` and the
  Rust `ANDROID_BINDER_IPC_RUST` as `bool`, not `tristate`. Kbuild cannot build them as
  modules, so building binder "as a module" always means building a copy of the sources out
  of tree, as `check-symbols.sh` does.

### 2. `.wslconfig` `kernelModules` cannot be used with the stock kernel

The documentation describes `kernelModules` as "an absolute Windows path to a custom Linux
kernel modules VHD". The WSL source makes the restriction explicit: in
`src/windows/service/exe/WslCoreVm.cpp`, when `kernelModules` is set and `kernel` is not,
the VM refuses to start with `MessageMismatchedKernelModulesError`, whose English text is
"Customised kernel modules were supplied without specifying a customised kernel."

So a modules VHD of our own is only possible together with a kernel of our own. With the
stock kernel, the remaining route is to load a module from inside a distribution with
`insmod`, after the VM has started. The `loadKernelModules` setting only names modules that
are already in the stock modules VHD.

### 3. Signing and vermagic do not block an external module

From `Microsoft/config-wsl` (6.18.54.1):

| Option | Value | Consequence |
|---|---|---|
| `CONFIG_MODULES` | `y` | Modules can be loaded |
| `CONFIG_MODULE_SIG` | not set | No signature is checked |
| `CONFIG_MODVERSIONS` | `y` | Every imported symbol carries a CRC that must match the running kernel |
| `CONFIG_MODULE_FORCE_LOAD` | `y` | `insmod -f` can skip the checks (not something to ship) |
| `CONFIG_LOCALVERSION` | `-microsoft-standard-WSL2` | vermagic is `<version>-microsoft-standard-WSL2 SMP preempt mod_unload modversions` |

A module built from the same tag with the same configuration has matching vermagic and CRCs
(the CRCs come from the type signatures, not the compiler). One trap: building from a git
checkout makes `setlocalversion` append `+` to the release (`6.18.54.1-microsoft-standard-WSL2+`),
which breaks vermagic. `make LOCALVERSION=` keeps the release exact; `check-symbols.sh`
does this. The cost is that a module
matches exactly one kernel release. WSL updates its kernel on its own schedule, so every new
WSL kernel release needs a new build, and the installer has to pick the build that matches
`uname -r` or refuse clearly.

### 4. The upstream driver needs symbols the kernel does not export

`check-symbols.sh` output:

| Symbol | 6.6.123.2 | 6.18.54.1 | What binder uses it for |
|---|---|---|---|
| `security_binder_set_context_mgr` | missing | missing | LSM check when a process becomes the context manager |
| `security_binder_transaction` | missing | missing | LSM check on every transaction |
| `security_binder_transfer_binder` | missing | missing | LSM check when a binder reference is passed |
| `security_binder_transfer_file` | missing | missing | LSM check when a file descriptor is passed |
| `task_work_add` | missing | missing | Deferred close of a received file descriptor |
| `close_fd_get_file` / `file_close_fd` | missing | missing | Taking a file out of the fd table for that deferred close (renamed in 6.8) |
| `can_nice` | missing | missing | Priority inheritance across a transaction |
| `__wake_up_pollfree` | missing | missing | Waking epoll waiters when a thread exits |
| `init_ipc_ns`, `put_ipc_ns` | missing | missing | binderfs: one instance per IPC namespace |
| `zap_page_range_single` | missing | missing | Unmapping buffer pages that the shrinker frees |
| `list_lru_add`, `list_lru_del` | exported then | missing | The free-page LRU (6.18 only exports the `_obj` variants) |
| `lock_vma_under_rcu` | not used | missing | Lock-free VMA lookup in the shrinker |

A module that references any of these does not load: the kernel reports an unknown symbol.
The list grew by three between 6.6 and 6.18, which is the maintenance cost in miniature.

Two kernel features matter for the usual workaround (looking the addresses up at run time):

- `CONFIG_KPROBES=y` and `CONFIG_KALLSYMS_ALL=y`, so a module can find
  `kallsyms_lookup_name` through a kprobe and then look up the rest, data symbols included.
- `CONFIG_X86_KERNEL_IBT=y`. With indirect branch tracking active, objtool seals the
  `ENDBR` of functions whose address is never taken inside the kernel, and an indirect call
  to a sealed function faults. `can_nice`, `task_work_add` and the others are called
  directly in the kernel, so calling them through a looked-up pointer can fault. Whether IBT
  is actually active depends on the CPU and on Hyper-V exposing CET to the WSL VM; that has
  to be checked on real hardware (`dmesg | grep -i ibt`, the `ibt` flag in
  `/proc/cpuinfo`). `ibt=off` on the kernel command line (`.wslconfig` `kernelCommandLine`,
  which does work with the stock kernel) turns it off for the whole VM.

`ashmem` is not involved: binder and binderfs do not need it, and `CONFIG_MEMFD_CREATE=y`
is set.

## Options

| | Option | What it costs | What it buys |
|---|---|---|---|
| A | Patched out-of-tree binder; unexported symbols looked up at run time through kprobes and kallsyms | A fork of binder per kernel series; a build per WSL kernel release; may need `ibt=off` for the whole VM; private symbols change between kernel releases without notice | Stock kernel stays; the user builds nothing |
| B | Patched out-of-tree binder; unexported calls replaced with exported ones or dropped | Same fork and per-release builds. Some calls have no exported equivalent: the LSM hooks would be skipped (no SELinux checks on binder), epoll wake-up on thread exit and priority inheritance would be approximated | No private symbol lookups, no IBT question |
| C | Our own kernel: the WSL tag with `ANDROID_BINDER_IPC` and `ANDROID_BINDERFS` built in, shipped by the installer through `.wslconfig` `kernel` and `kernelModules` | The kernel affects every distribution the user has; we follow every WSL kernel release ourselves; the user loses WSL's own kernel updates while it is set | Upstream binder as is; no private symbols; this is the stopgap the current decision already names |
| D | Ask Microsoft to enable `ANDROID_BINDER_IPC` and `ANDROID_BINDERFS` in `config-wsl` | Time, and the answer is not ours | Removes the problem; the long-term goal in the current decision |

**Decision (owner, 2026-10-16):** none of A to C becomes the default path. A and B depend on
how the WSL kernel looks inside, which changes with every release (the missing symbols went
from 11 to 14 between 6.6 and 6.18), and C changes every WSL distribution the user has.
Android runs in a dedicated lightweight VM with its own kernel instead; see
`experiments/android-utility-vm/`. D goes ahead in parallel: the issue text is drafted in
`upstream-proposal.md` for the owner to submit. If the WSL kernel gains binder, a WSL
container backend can be added next to the VM.

D is worth starting whatever else is chosen. The current decision prefers an external
module on the stock kernel; this experiment shows that this means A or B, not an unmodified
driver.

## Not checked here

- Loading anything on a real WSL VM. This environment is Linux without WSL or Hyper-V. The
  findings come from the sources and from compiling the driver; loading a module (and the
  IBT question above) needs a Windows machine.
- A full kernel build. With one, modpost resolves symbols from `Module.symvers` instead of
  the source search in `check-symbols.sh`. The search was checked by hand for every symbol
  in the table: each has a definition and no `EXPORT_SYMBOL*` anywhere in the tree.
