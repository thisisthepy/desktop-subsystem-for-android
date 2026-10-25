# Desktop Subsystem for Android™

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/thisisthepy/desktop-subsystem-for-android/blob/main/LICENSE)

**English** · [한국어](https://github.com/thisisthepy/desktop-subsystem-for-android/blob/main/docs/locales/README_ko.md)

**Android apps as real desktop windows, on an open-source stack.**

*Android runs in a lightweight VM of its own. Each app gets its own native window, Start menu
entry and notifications.*

> DSA is an independent open-source project. It is not affiliated with, endorsed by or
> sponsored by Microsoft or Google. Android is a trademark of Google LLC. Windows is a
> trademark of the Microsoft group of companies.

---

## Status

**Design stage. There is no code to run yet.** The repository holds the architecture and the
requirements; implementation starts once the requirements are agreed. Windows 11 is the
first host. Linux and macOS hosts are planned, not started.

## Why

Microsoft ended support for the Windows Subsystem for Android on March 5, 2025. Its core
parts were closed binaries, so nobody outside Microsoft can carry it on. DSA aims for the
same window integration, built from open source, and lighter to keep running in the
background.

## How it is meant to work

| Part | Approach | Status |
|---|---|---|
| Android | A lightweight VM of its own, created with the same Windows API as WSL and booting straight into Android. Your WSL setup is left untouched | planned |
| Kernel | DSA ships the VM's kernel with binder built in; no kernel build for users | planned |
| Windows | A custom hwcomposer hands each Android task to the DSA Window Manager, which shows it in a Win32 window of the app's own small program, named and drawn as the app, so the taskbar and Task Manager show each app as itself. No WSLg, no RDP | planned |
| Graphics | Every app's OpenGL ES runs through ANGLE on Vulkan, and Vulkan runs on the host GPU through remoting (starting from gfxstream) | planned |
| Devices | Video codecs, audio and power move to the host as HAL services; the Android framework stays in the guest | planned |
| Shell | No Android launcher, status bar or keyboard: the Start menu, the notification centre and the Windows IME take their place | planned |
| Idle | Android's Doze is the idle state, so notifications and widgets keep working while the CPU rests | planned |
| Widgets | Android widgets on the Windows desktop | planned |

```
[Android VM]                                    [Windows host: DSA service]
 app → SurfaceFlinger
   → hwcomposer ─── window control, input ───── window manager → each app's program: one window per task
   → ANGLE → Vulkan ─── Vulkan remoting ───────→ host Vulkan
 agent ─── apps, notifications, intents, IME ──→ Start menu, notifications, TSF
 HAL clients ─── RPC Binder ──────────────────→ host codecs, audio, power
```

## What DSA will not ship

- **Google Play services or an app store.** Licensing makes them a user's choice, not
  something the project can bundle.
- **Microsoft's WSA binaries.** DSA does not reuse or redistribute them.

## Repository

- `dsa/`: all code (empty until the first agreed requirement is implemented)
- `docs/locales/`: translations of this README
- `scripts/tests/`: repository checks

Work happens on the `develop` branch. `main` is published from it through a pull request.

## License

[Apache-2.0](https://github.com/thisisthepy/desktop-subsystem-for-android/blob/main/LICENSE)
