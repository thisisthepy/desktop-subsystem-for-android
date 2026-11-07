# Memory and idle behaviour

Desktop Subsystem for Android™ (DSA) gives memory back to your PC while Android runs, not
only when it stops. There is nothing to set up for this. This page explains what happens,
and how to see it.

## See how much memory Android uses

Open the **Memory** page of DSA Settings, or run `dsa memory`. Both show:

- **On this PC**: what Android's virtual machine, the window manager and each app window
  hold.
- **Inside Android**: memory in use, cache, compressed memory and the largest processes.
  Android reports this a few minutes after it starts.

## Doze, the idle state

When every Android window has been closed or minimized for 30 seconds, Android's screen
turns off and Android enters Doze, as a phone does in a pocket.

- Notifications, alarms and widgets keep working in Doze. The CPU rests in between.
- `dsa status` says `Dozing`. The **General** page of DSA Settings says **Dozing (idle)**.
- Showing any Android window wakes Android at once.

Android also follows your PC's sleep. It saves its state before the PC sleeps and sets its
clock when the PC wakes.

## Freezing (off by default)

Freezing stops every process in Android. It uses no CPU then, but notifications, alarms and
widgets stop too. That is why DSA never freezes Android unless you ask.

- To freeze now: **Freeze** under **Freeze Android now** on the **General** page, or
  `dsa freeze`.
- To freeze after 30 minutes in Doze: turn on **Freeze Android when idle** on the
  **Advanced** page, or `dsa settings set freeze on`.
- Opening an Android window, or input in one, resumes Android.

## How memory goes back

- Android's virtual machine takes memory from your PC only when Android first uses it.
- Memory that Android frees is returned to your PC while Android runs.
- When an app's last window closes, Android ends that app's processes, so their memory goes
  back within seconds. An app with a widget on the desktop, or one still working in the
  background (such as a music player), keeps running.
- While no app window is open, Android drops its file caches and moves idle memory to
  compressed memory, so more of it can go back.
- The window manager gives back the memory it does not use 30 seconds after the last app
  window closes.

## Memory limit

The limit is **Automatic** by default, which sets no fixed limit. Android starts at 3 GB and grows while it runs, up to your PC's memory less what Windows keeps (10% of the memory or 2 GB, whichever is larger). Android holds only what it uses.
To set a limit, use **Memory limit** on the **Memory** page, or:

```
dsa settings set memory-limit 4g     # 4 GiB
dsa settings set memory-limit auto   # back to automatic
```

A new limit applies at the next start of Android.

---

Android is a trademark of Google LLC. Windows is a trademark of the Microsoft group of
companies. DSA is not affiliated with, endorsed by or sponsored by Google or Microsoft.
