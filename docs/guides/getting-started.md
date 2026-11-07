# Getting started

Desktop Subsystem for Android™ (DSA) runs Android in a lightweight virtual machine with
its own kernel. Each Android app opens in its own window on your desktop.

## Status by platform

| Host | Status |
|---|---|
| Windows 11, x64 | Preview, released. Features are tested in CI; testing by hand on real hardware is still in progress. |
| Windows 11, ARM64 | In progress. Builds and installs in CI; not released yet. |
| Linux, x64 and ARM64 | In progress. The `.deb` and `.rpm` packages build in CI; not released yet. |
| macOS, Apple silicon | In progress. `DSA.app`, its `.pkg` and its `.dmg` build in CI; not released yet. |

## Install on Windows

You need 64-bit Windows 11 on an x64 PC, with the Windows feature **Virtual Machine
Platform** turned on.

1. Turn on Virtual Machine Platform. Open **Turn Windows features on or off**, tick
   **Virtual Machine Platform**, select **OK**, and restart. Setup does not turn it on for
   you: it stops with a message if the feature is off.
2. Download `DSA-v<version>-win-x64-setup.exe` and `SHA256SUMS.txt` from the
   [latest release](https://github.com/thisisthepy/desktop-subsystem-for-android/releases/latest).
3. Check the download. The hash this command prints must match the line in
   `SHA256SUMS.txt`:

   ```
   certutil -hashfile DSA-v<version>-win-x64-setup.exe SHA256
   ```

4. Run setup, select **Install**, and approve the administrator prompt.
5. Wait for the first Android image. When setup does not carry one, the DSA service
   downloads it in the background. `dsa status` shows the download.
6. Open an app from the Start menu, or check from a terminal:

   ```
   dsa start --wait
   dsa launch com.android.settings
   ```

## What setup installs

- The `DsaHost` service, which runs each user's Android.
- The `dsa` command.
- **DSA Settings** in the Start menu.
- **Android** in the navigation pane of File Explorer, which opens Android's files.
- The APK Installer, which opens `.apk` files.
- The DSA Window Manager (`dsa-wm`). It starts when you sign in and starts Android then,
  so the first app you open does not wait for Android to boot.

Each Windows user has their own Android and their own data disk,
`%LOCALAPPDATA%\DSA\android.vhdx`. Other users cannot read it.

DSA uses its own kernel and its own virtual machine. It does not use or change your WSL
settings, kernel or distributions.

## Updates

DSA updates itself and the Android image. Both are signed. If an Android image fails to
boot, DSA goes back to the previous one, and `dsa status` says so. An administrator can turn
updates off on the **Updates** page of DSA Settings.

## Linux (in progress, not released)

DSA for Linux is built as a `.deb` (Debian, Ubuntu) and an `.rpm` (Fedora, openSUSE). It
needs KVM (`/dev/kvm`) and a Wayland desktop. Android runs in a crosvm virtual machine.

Installing a package also adds DSA's signed package repository, so later versions arrive
through `apt`, `dnf`, `zypper` or the software center. DSA has no updater of its own on
Linux.

- Debian and Ubuntu: `/etc/apt/sources.list.d/dsa.sources`, with the key in
  `/usr/share/keyrings/dsa-archive-keyring.gpg`.
- Fedora and openSUSE: `/etc/yum.repos.d/dsa.repo` (and `/etc/zypp/repos.d/dsa.repo` where
  zypper is present), with the key in `/etc/pki/rpm-gpg/RPM-GPG-KEY-dsa`.

The repository is to be served from
`https://thisisthepy.github.io/desktop-subsystem-for-android/`. It is not published yet.

Once released, the first install looks like this:

```
sudo apt install ./desktop-subsystem-for-android_<version>_amd64.deb   # Debian, Ubuntu
sudo dnf install ./desktop-subsystem-for-android-<version>*.rpm       # Fedora
```

The package installs the `dsa-hostd` system service, the `dsa-devices` and
`dsa-compositor` user services, the `dsa` command in `/usr/bin`, the **DSA Files** menu
entry, and the file actions **Open in an Android app**, **Share with an Android app** and
**Install in Android**.

## macOS (in progress, not released)

DSA for macOS runs Android on Apple's Virtualization framework, on Apple silicon. It is
built as `DSA.app`, in an installer package (`DSA-<version>.pkg`) and a disk image
(`DSA-<version>.dmg`). The CI builds are signed ad hoc, so they run only on the machine that
built them. No release signed with a Developer ID exists yet.

## Next

- [Apps and windows](apps.md)
- [Installing APKs](install-apks.md)

---

Android is a trademark of Google LLC. Windows is a trademark of the Microsoft group of
companies. macOS is a trademark of Apple Inc. DSA is not affiliated with, endorsed by or
sponsored by Google, Microsoft or Apple.
