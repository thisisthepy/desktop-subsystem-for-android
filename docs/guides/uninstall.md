# Uninstalling

## Windows

Uninstall Desktop Subsystem for Android™ (DSA) in either way:

- Windows **Settings** > **Apps** > **Installed apps**: find Desktop Subsystem for Android,
  then select **Uninstall**.
- Run setup again and select **Uninstall**.

Uninstalling removes these, for every user of the PC, signed in or not:

- the program, the `DsaHost` service, the `dsa` command, DSA Settings and the APK Installer;
- the downloaded Android images and updates;
- the Start menu entries of Android apps and their entries in **Installed apps**;
- **Android** in File Explorer, the `.apk` file association, and the context menu entries;
- DSA's settings and logs.

It **keeps** each user's Android disk, `%LOCALAPPDATA%\DSA\android.vhdx`, so a later
install finds your apps and data again. To remove your data too, run `dsa reset-data --yes`
before you uninstall, or delete that file afterwards.

If folder redirection moved a user's Start menu, that user's Android app entries stay,
because the uninstaller looks only in the default place. Delete them by hand.

Your WSL setup is untouched, because DSA never changes it.

## Linux (not released yet)

```
sudo apt remove desktop-subsystem-for-android    # Debian, Ubuntu
sudo dnf remove desktop-subsystem-for-android    # Fedora
```

Removing the package also removes the package repository entry it added.

---

Android is a trademark of Google LLC. Windows is a trademark of the Microsoft group of
companies. DSA is not affiliated with, endorsed by or sponsored by Google or Microsoft.
