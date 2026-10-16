# Draft: enable binder and binderfs in the WSL kernel

This is a draft for an issue on [microsoft/WSL](https://github.com/microsoft/WSL/issues), the
place the WSL kernel README sends feature requests. It is not submitted; the owner submits
it. Everything below the line is the proposed issue text.

---

**Title:** Enable `CONFIG_ANDROID_BINDER_IPC` and `CONFIG_ANDROID_BINDERFS` in the WSL kernel

**Summary**

Please build the binder IPC driver and binderfs into the WSL kernel:

```
CONFIG_ANDROID_BINDER_IPC=y
CONFIG_ANDROID_BINDERFS=y
CONFIG_ANDROID_BINDER_DEVICES=""
```

With an empty `ANDROID_BINDER_DEVICES`, the kernel creates no global binder device nodes.
Binder is reachable only through a binderfs instance that someone mounts, so a WSL
distribution that does not use it sees no change.

**Why**

Running Android userspace in a container (the approach of Waydroid on Linux desktops)
needs binder. On WSL today the only way to get it is to replace the kernel with `kernel=` in
`.wslconfig`. That affects every WSL 2 distribution the user has, stops the kernel updates
WSL ships, and is refused on machines whose policy disallows custom kernels.

A module is not a way out. Upstream declares `ANDROID_BINDER_IPC`, `ANDROID_BINDERFS` and
`ANDROID_BINDER_IPC_RUST` as `bool`, and `kernelModules` without `kernel` is refused by
design. Built out of tree against `linux-msft-wsl-6.18.54.1`, the driver imports 14 symbols
that the kernel does not export (among them the `security_binder_*` LSM hooks,
`task_work_add`, `file_close_fd` and `zap_page_range_single`), so it cannot load. Working
around that means looking up private symbols at run time, which breaks with kernel updates
and runs into IBT, which the WSL kernel enables. The full analysis and a script that
reproduces it are at <link to experiments/wsl-binder-module on develop>.

**Cost and risk**

- Size: the binder driver, its allocator and binderfs, built in. To be measured on the
  current tag before submitting (`size vmlinux` with and without).
- Attack surface: binderfs sets `FS_USERNS_MOUNT`, so an unprivileged user namespace can
  mount an instance. This is the main point to discuss. Binder is the same code that runs on
  every Android device and is heavily fuzzed there, and it is already enabled in the kernels
  of mainstream desktop Linux distributions that support Waydroid (to be confirmed with
  current config files before submitting).
- Maintenance: none beyond following upstream; the driver is in mainline.

**What this enables**

Android userspace in a WSL container, sharing WSL's VM and memory management instead of
starting a second VM, for any project that wants it.

---

## Before submitting

- Measure the size change on the current WSL tag.
- Check which desktop distributions enable binder and binderfs today, and cite their
  configuration files, or drop that sentence.
- Replace the link placeholder with the published location of this experiment. The
  experiments folder is on `develop` only.
