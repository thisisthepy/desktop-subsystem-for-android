# Apps and windows

Each Android app task opens as a desktop window of its own, with its own taskbar button.
There is no emulator frame around a phone screen. This page describes Desktop Subsystem
for Android™ (DSA) on Windows.

## Open an app

- **Start menu.** Each installed app has a Start menu entry with its own name and icon, so
  Windows search finds it. Opening it starts Android first when Android is not running.
- **DSA Settings.** On the **Apps** page, select **Open** next to the app.
- **Command line.** `dsa apps list` lists the installed apps with their packages.
  `dsa launch <package>` opens one, for example `dsa launch com.android.settings`.

When another program's Start menu entry already has the app's name, DSA adds the package to
the name, such as `Clock (com.example.clock)`. DSA changes or removes only the entries it
made.

## Work with app windows

- Each task has its own window, taskbar button and Alt+Tab entry, with the app's name and
  icon. Task Manager lists each app under its own name.
- Snap layouts, the Windows 11 title bar buttons and live resizing work. Resizing does not
  restart the app.
- Mouse, touch, pen (pressure, eraser and barrel button) and keyboard all work. Text comes
  from your Windows input method, including Korean and other languages; there is no Android
  keyboard app.
- Screen readers such as Narrator read the app through UI Automation.
- Closing a window asks Android to end that task.

## Set an app's window size

On the **Apps** page of DSA Settings:

- Turn off **Resizable** to keep the app's windows at the size they open at.
- Pick the size the app's windows open at: the size Android chooses, or a phone or tablet
  size in portrait or landscape.

The change applies to the next window the app opens. From the command line:

```
dsa app-window <package> --fixed-size on        # cannot be resized or maximized
dsa app-window <package> --start-size 1280x800  # opens at this size; auto lets Android choose
dsa app-window <package> reset                  # back to the defaults
```

## Notifications

Android notifications show as Windows notifications, with the app's name and icon.

- Select a notification to open the app.
- Dismiss it, and it is dismissed in Android too, unless the app keeps it shown.
- Notifications keep arriving while Android is in Doze. See [Memory](memory.md).

## Apps running in the background

An app that keeps running after its window closes, such as a music player, shows as an
icon in the notification area. Its tooltip is the app's notification.

- Select the icon to open the app.
- Right-click it for **Open**, the notification's own actions, and **Stop**.
- **Stop** asks first. Tick **Do not ask again** to skip the question from then on. To be
  asked again, turn on **Ask before stopping an app from the notification area** on the
  **Running in the background** page of DSA Settings.
- An app that plays media also works with the media keys and the Windows media controls.

The **Running in the background** page of DSA Settings lists these apps, with **Open** and
**Stop** for each. From the command line: `dsa background`, `dsa background open <package>`
and `dsa background stop <package>`.

## Widgets on the desktop

An Android widget can sit on your desktop in a window of its own. It stays below other
windows, stays when you show the desktop, and is not in the taskbar or Alt+Tab.

1. Open the **Widgets** page of DSA Settings.
2. Under **Widgets your apps offer**, select **Add to desktop** next to a widget. If the app
   asks you to set the widget up, its setup opens in a window.
3. To move a widget, drag the round **More** button that appears at its top right corner
   when you point at it. You can also press anywhere on the widget, hold still for half a
   second, and then drag. A quick drag stays the widget's own, so its lists still scroll.
   Drag its edges to resize it. DSA remembers where you put it.
4. Select the **More** button, or right-click the widget, for **Resize**, to pick one of
   the sizes it allows, and **Remove**. The arrow keys, Enter and Esc work in this menu
   too, and a letter picks an item: R for **Resize** and M for **Remove**.

From the command line: `dsa widgets list`, `dsa widgets add <provider>` and
`dsa widgets remove <id>`.

## Links and the clipboard

- A web link you tap in an Android app opens in your default browser. A link that an
  installed app has verified as its own opens in that app.
- To open every web link in Android apps instead, turn on **Open web links in Android
  apps** on the **Advanced** page of DSA Settings (`dsa settings set links-in-android on`).
- Text you copy is shared both ways between Windows and Android. To stop it, turn off
  **Share the clipboard with Android** on the **Advanced** page
  (`dsa settings set clipboard off`).

## Apps built only for ARM

On an x64 PC, an app with code only for ARM processors runs through an ARM translator.
Android on an x64 PC comes with one, the open source translator Digitalis, and uses it from
its first start. You can use another instead:

- **Google's ARM translator.** On the **Advanced** page, select **Get** beside **Google's ARM
  translator**, or run `dsa arm-translation fetch google`. DSA first shows where it comes
  from and Google's terms (the
  [Android SDK License](https://developer.android.com/studio/terms)), and downloads nothing
  until you accept. Your PC then downloads the Android emulator image Google offers for your
  Android version (about 2 GB), keeps only the translator and deletes the rest. DSA does not
  ship or pass on Google's translator.
- **A package you build or get elsewhere**, such as one made from the open source translator
  Digitalis: `dsa arm-translation install <folder>`.

Then restart Android: stop it on the **General** page of DSA Settings, then open an app.

To go back to the translator that comes with Android, select **Use** beside **Use the default
translator** on the **Advanced** page, or run `dsa arm-translation use-default`. It removes the
package you installed, from the next start of Android.

`dsa arm-translation status` says which translator Android uses. The **Run apps built only for
ARM** switch on the **Advanced** page turns translation on or off without removing anything.
Only 64-bit ARM apps (`arm64-v8a`) can run. On an ARM64 PC, Android runs ARM apps itself and
needs no translator.

---

Android is a trademark of Google LLC. Windows is a trademark of the Microsoft group of
companies. DSA is not affiliated with, endorsed by or sponsored by Google or Microsoft.
