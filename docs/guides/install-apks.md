# Installing APKs

Desktop Subsystem for Android™ (DSA) ships no app store. You install apps from APK files,
with the APK Installer or the `dsa` command. This page describes DSA on Windows.

## Install with the APK Installer

1. Double-click an `.apk` file in File Explorer. It opens in the **APK Installer**.
2. Check what the APK is. The window shows it without starting Android:
   - the app's name, icon, package and version;
   - the publisher, taken from the signing certificate, because an APK names no publisher;
   - the source: the host the file was downloaded from, or its folder;
   - under **Capabilities**, the first permissions it asks for. **More** shows all of them, the
     Android versions it is made for, its size, its processor types and its certificate's
     SHA-256.

   When the APK has code only for another processor type, the window warns you.
3. Select **Install**. When the app is installed already, the button reads **Update** or
   **Install again**. Android starts first when it is not running.
4. With **Launch when ready** ticked (the default), the app opens once it is installed and
   the installer closes. Otherwise select **Launch**, or open the app later from the Start
   menu.

While Android is running, the window also says whether the file is a new install, an
update, the same version again, or something Android refuses: an older version, or the same
app signed with another key.

You can also drop APK files on the APK Installer, on DSA Settings, or on an app's window.
Each APK opens in an installer window of its own.

## Install from the command line

```
dsa apk-info app.apk            # what the APK is; reads the file without starting Android
dsa install app.apk             # install it; starts Android when it is not running
dsa install app.apk --progress  # also print each step
```

When Android refuses the APK, `dsa install` exits with code 1, and its message starts with
`not installed (<reason>):`.

## Why an install is refused

| Reason | Meaning | What to do |
|---|---|---|
| `incompatible` | The app needs another processor type, a newer Android version, or a feature this device lacks | See [Apps built only for ARM](apps.md#apps-built-only-for-arm), or [Android versions](android-versions.md) |
| `conflicting-signature` | The app is installed already, signed with another key | Uninstall the installed app first |
| `downgrade` | A newer version of the app is installed | Uninstall the installed app first |
| `invalid-apk` | The file is not a signed APK that Android can read | Get the APK again |
| `insufficient-storage` | Android has no room for the app | Free space in Android |
| `blocked` | A policy of this Android does not allow the app | |
| `other` | Anything else; Android's own message follows | |

An APK can be up to 2 GiB. Installs run one at a time: a second one waits for the first.

## Uninstall an app

Use any of these:

- Windows **Settings** > **Apps** > **Installed apps**.
- The **Apps** page of DSA Settings: **Uninstall** next to the app.
- `dsa apps uninstall <package>`.

Apps that are part of Android cannot be uninstalled.

---

Android is a trademark of Google LLC. Windows is a trademark of the Microsoft group of
companies. DSA is not affiliated with, endorsed by or sponsored by Google or Microsoft.
