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
| **Apps** | Installed apps: **Open**, **App info**, **Uninstall**, **Resizable**, the size windows open at | `dsa apps`, `dsa app-window` |
| **Running in the background** | Apps running with no window: **Open**, **Stop**; whether to ask before stopping | `dsa background`, `stop-without-asking` |
| **Widgets** | Widgets on your desktop, and **Add to desktop** | `dsa widgets` |
| **Display & GPU** | **Render GPU**, **Restart the window manager** | `render-gpu`, `dsa gpus` |
| **Android** | The running version, **Version to use** (admin) | `dsa android version`, `dsa android use` |
| **Updates** | **Update DSA automatically** (admin), **Update the Android image automatically** (admin), **Install now** | `program-updates`, `image-updates`, `dsa update` |
| **Device identity** | The manufacturer, model and build fingerprint Android reports to apps | `dsa settings device`, `device.<field>` |
| **Network** | How Android reaches the network, its address, DNS servers and gateway, **Advanced networking** | `dsa network`, `advanced-networking` |
| **Advanced** | Clipboard, files, web links, freeze when idle, reset, ARM apps, developer mode, traces (admin), log folders | see below |
| **Android log** | Android's live log, **Save diagnostics** | `dsa logcat`, `dsa logs collect` |
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
| **Developer mode** | Off | `developer-mode`, `adb-port` |
| **Input trace** (admin) | Off | `input-trace` |
| **Resize trace** (admin) | Off | `resize-trace` |

## When a change applies

- **Render GPU**: when the window manager starts again, at your next sign-in or when you
  select **Restart the window manager**.
- **Memory limit**, **Device identity**, **Advanced networking**, **Run apps built only for
  ARM** and the Android version: at the next start of Android. To restart Android, stop it
  on the **General** page, then open an app.
- Everything else: at once.

## Values the command line takes

- `render-gpu`: `display` (the GPU of the primary monitor, the default), `integrated`,
  `discrete`, or part of a GPU's name as `dsa gpus` lists it.
- `memory-limit`: `auto` (the default: no fixed limit, up to the PC's memory less what Windows keeps), or a
  size such as `4g` or `3072` (MiB).
- `shared-folders`: `all`, `none`, or names separated by commas, such as
  `Documents,Pictures`.
- `adb-port`: a TCP port from 1 to 65535. The default is 58526.
- `device.<field>`: `manufacturer`, `brand`, `model`, `device`, `name`, `characteristics`,
  `fingerprint`, or a part of the fingerprint such as `fingerprint.release`.
  `dsa settings reset device` puts every value back.

---

Android is a trademark of Google LLC. Windows is a trademark of the Microsoft group of
companies. DSA is not affiliated with, endorsed by or sponsored by Google or Microsoft.
