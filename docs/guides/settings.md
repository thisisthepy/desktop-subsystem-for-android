# Settings

**DSA Settings** is the settings app of Desktop Subsystem for Android™ (DSA) on Windows.
Open it from the Start menu. It shows in English or Korean, following your Windows display
language, and follows your Windows theme.

Every change DSA Settings makes goes through the `dsa` command, so the command line sees
the same values:

```
dsa settings                        # every setting and its value
dsa settings set <name> <value>     # change one; switches take on or off
```

Some settings apply to every user of the PC, so only an administrator can change them. DSA
Settings marks them with a shield icon and asks for administrator approval when you change
one. They are marked "(admin)" below.

## Pages

| Page | What you find there | Command line |
|---|---|---|
| **General** | Android's state, **Start**, **Stop**, **Freeze Android now**, the last problem, **Open my log folder** | `dsa status`, `dsa start`, `dsa stop`, `dsa freeze` |
| **Memory** | **Memory limit**, the size of Android's VM, what each part uses | `memory-limit`, `dsa memory` |
| **Apps** | Installed apps: **Open**, **App info**, **Uninstall**, **Create desktop shortcut**, **Resizable**, the size windows open at | `dsa apps`, `dsa app-window` |
| **Running in the background** | Apps running with no window: **Open**, **Stop**; whether to ask before stopping | `dsa background`, `stop-without-asking` |
| **Widgets** | Widgets on your desktop, and **Add to desktop** | `dsa widgets` |
| **Display & GPU** | **Render GPU**, **Restart the window manager**, **Turn off Windows touch gestures in DSA app windows** (Windows 11 only, on by default) | `render-gpu`, `dsa gpus`, `touch-gestures-off` |
| **Device identity** | This computer first, read only: its name, maker and model, and **Device specifications** with **Copy**. Then the manufacturer, model and build fingerprint Android reports to apps | `dsa settings device`, `device.<field>` |
| **Network** | How Android reaches the network, its address, who answers its names (DNS) and its gateway, **Advanced networking**, **Developer mode** (adb) | `dsa network`, `advanced-networking`, `developer-mode`, `adb-port`, `adb-auto-connect` |
| **Updates** | **Android**: the running version and build, the version it follows, the Android update with its progress and **Check now** (admin), **Update the Android image automatically** (admin), **Version to use** (admin). **DSA**: **Update DSA automatically** (admin), **Get pre-release versions** (admin), **Install now** | `dsa android version`, `dsa android use`, `image-updates`, `program-updates`, `prerelease-updates`, `dsa update` |
| **Advanced** | Clipboard, files, web links, freeze when idle, reset, ARM apps, traces (admin), log folders | see below |
| **Logs** | Android's live log, **Save diagnostics** | `dsa logcat`, `dsa logs collect` |
| **About** | Versions, licences, source code | |

## The Advanced page

| Setting | Default | Command line |
|---|---|---|
| **Share the clipboard with Android** | On | `clipboard` |
| **Share Windows folders with Android** | On | `file-sharing` |
| **Shared folders** | All six | `shared-folders` |
| **Freeze Android when idle** | Off | `freeze` |
| **Open web links in Android apps** | Off | `links-in-android` |
| **Reset Android's data** | | `dsa reset-data` |
| **Run apps built only for ARM** | | `arm-translation` |
| **Google's ARM translator** (not on ARM64) | | `dsa arm-translation fetch google` |
| **Use the default translator** (not on ARM64) | | `dsa arm-translation use-default` |
| **Input trace** (admin) | Off | `input-trace` |
| **Resize trace** (admin) | Off | `resize-trace` |

## When a change applies

- **Render GPU**: when the window manager starts again, at your next sign-in or when you
  select **Restart the window manager**.
- **Memory limit**, **Device identity**, **Advanced networking**, **Run apps built only for
  ARM** and the Android version: at the next start of Android. To restart Android, stop it
  on the **General** page, then open an app.
- Everything else: at once.

## How Android finds names

Your computer answers every name Android's apps look up, with the same resolver Windows apps
use (on Linux and macOS, the system's resolver). So your hosts file, your company's name rules,
a VPN's own DNS servers and a DNS filter apply to Android's apps as they do to the computer's.
Android does not ask a DNS server itself.

- This is the same with **Advanced networking** on: your computer is on the same network and
  knows its names, and a VPN or DNS filter on it still applies.
- Android's own **Private DNS** is off by default, because it would ask a DNS server around your
  computer. If you choose a private DNS server in Android's settings, Android uses it.
- If DSA cannot answer for a while, Android asks the network's DNS servers until it can again.
- The **DNS** row of the **Network** page, and `dsa network`, say which one answers now: your
  computer's resolver, or the network's DNS servers, which they list.

## Values the command line takes

- `render-gpu`: `display` (the GPU of the primary monitor, the default), `integrated`,
  `discrete`, or part of a GPU's name as `dsa gpus` lists it.
- `memory-limit`: `auto` (the default: no fixed limit, up to the PC's memory less what Windows keeps), or a
  size such as `4g` or `3072` (MiB).
- `shared-folders`: `all`, `none`, or names separated by commas, such as
  `Documents,Pictures`.
- `adb-port`: a TCP port from 1 to 65535. The default is 58526.
- `touch-gestures-off`: `on` (the default) turns Windows 11's three- and four-finger touch
  gestures off while a DSA app window is in front, so apps get up to ten fingers, and puts
  your own setting back when another window comes to the front. If Windows refuses, the
  setting can't be used and the settings app says why; you can still turn the gestures off
  for every app in Windows Settings, under Bluetooth & devices, Touch.
- `prerelease-updates`: `on` takes pre-releases as well as stable releases, `off` takes stable
  releases only. It starts from the installer you used: a pre-release installer turns it on,
  a stable one leaves it off, and upgrades keep your choice. Every release before 1.0 is a
  pre-release. Turning it off never moves DSA back to an older version; DSA waits for a stable
  release newer than the one installed.
- `device.<field>`: `manufacturer`, `brand`, `model`, `device`, `name`, `characteristics`,
  `fingerprint`, or a part of the fingerprint such as `fingerprint.release`.
  `dsa settings reset device` puts every value back.

---

Android is a trademark of Google LLC. Windows is a trademark of the Microsoft group of
companies. DSA is not affiliated with, endorsed by or sponsored by Google or Microsoft.
