# Troubleshooting and logs

This page helps you find out why Desktop Subsystem for Android™ (DSA) does not work, read
Android's log, and send a problem report. It covers Windows, and Linux where noted.

## Check the state first

```
dsa status          # Android's state, any problem, and any pending update
dsa start --wait    # start Android and wait until it has booted
```

The **General** page of DSA Settings shows the same state.

| `dsa status` | DSA Settings | Meaning |
|---|---|---|
| `Active` | **Running** | Android has booted and DSA's agent in it is connected |
| `Dozing` | **Dozing (idle)** | Android is idle. See [Memory](memory.md) |
| `Frozen` | **Frozen** | Android is frozen until you open one of its windows |
| `Stalled` | **Not ready** | Android runs but has taken too long. The next line says why and what to do |

## Common problems

| Problem | What to do |
|---|---|
| Setup says Virtual Machine Platform is off | Turn on the Windows feature, restart, and run setup again. See [Getting started](getting-started.md) |
| No app window opens | Sign out and sign in again. The window manager starts at sign-in |
| An app opens at a bad size | `dsa app-window <package> reset` |
| Android does not boot after an image update | Nothing. DSA goes back to the previous image on its own, and `dsa status` says so |
| Android has no network | Open the **Network** page of DSA Settings, or run `dsa network`. It shows the mode, address, DNS servers and any problem |
| Android is broken and nothing else helps | Reset Android's data (below) |

### Reset Android's data

Resetting erases every app you installed, your accounts and all app data. Android then
starts as it was first installed. The Android image and DSA's settings stay.

- In DSA Settings: **Advanced** page > **Reset Android's data** > **Reset**.
- On the command line: `dsa reset-data` says what would be lost. `dsa reset-data --yes`
  erases it.

## Read Android's log

Android's log stays in memory in Android and is never written to a file. It reaches your PC
only while something reads it.

- In DSA Settings: the **Android log** page. You can filter by tag or text, and pause.
- On the command line:

  ```
  dsa logcat                        # follow the log; Ctrl+C ends it
  dsa logcat -d -b all *:W          # what the log holds now, warnings and above
  dsa logcat ActivityManager:I *:S  # one tag only
  ```

## Send a problem report

1. Save the diagnostics. On the **Android log** page select **Save diagnostics**, or run:

   ```
   dsa logs collect -o diag.zip
   ```

   It takes a minute or more.
2. Attach the zip file to an
   [issue](https://github.com/thisisthepy/desktop-subsystem-for-android/issues).

The archive holds Android's whole log, its crash records (tombstones, ANR traces, dropbox,
pstore), a summary of Android's system services, and the DSA logs on this PC that concern
you, without other users' lines. `summary.txt` in the archive says what could not be
gathered.

## Where logs are on Windows

| Folder | Files |
|---|---|
| `%LOCALAPPDATA%\DSA\logs\` | `dsa-wm.log` (the window manager), `dsa-app-window.log` (app windows) |
| `%ProgramData%\DSA\logs\` | `dsa-hostd.log` (the service), `android-<SID>.log` (each user's Android console, at most 2 MiB a boot), `setup-<version>.log` (self-updates) |

To open them, use **Open my log folder** and **Open the service's log folder
(administrators)** on the **Advanced** page of DSA Settings.

## Where logs are on Linux (not released yet)

The services write to the systemd journal:

```
journalctl -u dsa-hostd
journalctl --user -u dsa-devices -u dsa-compositor
```

## Every command

`dsa` with no arguments prints every command. Add `--json` for output that programs can
read.

---

Android is a trademark of Google LLC. Windows is a trademark of the Microsoft group of
companies. DSA is not affiliated with, endorsed by or sponsored by Google or Microsoft.
