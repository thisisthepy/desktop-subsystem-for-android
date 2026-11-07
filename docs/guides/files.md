# Files

Desktop Subsystem for Android™ (DSA) lets you open Android's files in File Explorer, share
your Windows folders with Android apps, and send a file to an Android app. This page
describes DSA on Windows.

## Open Android's files in File Explorer

Use any of these. Each starts Android when it is not running.

- **Android** in the navigation pane of File Explorer, directly below WSL's **Linux**.
- **Android files** on the **Advanced** page of DSA Settings.
- `dsa files open`. `dsa files` says where the storage is, without opening it.

You see Android's shared storage: the files Android apps saved, such as photos and
downloads. You can copy files in and out as in any folder. File Explorer reaches the storage
through Windows' WebDAV client. If the storage does not answer, DSA shows an error with the
reason.

## Share your Windows folders with Android apps

Your Documents, Downloads, Pictures, Music, Videos and Desktop folders show in Android's
shared storage under `Host`, such as `Host/Pictures`. Android apps can open and save files
there. An app needs Android's storage permission first, and Windows lets it do only what
your own account may do.

Sharing is on by default. To change it, open the **Advanced** page of DSA Settings:

- Turn off **Share Windows folders with Android** to share no folder.
- Under **Shared folders**, clear a folder to stop sharing it. It leaves Android at once.

From the command line:

```
dsa settings set shared-folders Documents,Pictures   # only these; also all or none
dsa settings set file-sharing off                     # share no folder
```

Earlier versions used a folder named `Windows` instead of `Host`. Android removes the old
folder once it is empty, so files you saved there stay.

## Send a file to an Android app

- **Drag and drop.** Drag a file from File Explorer onto an app's window. The file goes to
  that app. An APK goes to the APK Installer instead. Folders are not sent.
- **Context menu.** Right-click a file and select **Open in an Android app** or **Share with
  an Android app**. On Windows 11 they are under **Show more options**. Android then asks
  which app takes the file.
- **Command line.** `dsa open <file>` and `dsa share <file>`.

These send files up to 64 MiB. A larger file is refused before anything is sent. To give an
app a larger file, put it in a shared folder instead.

## Get a file from Android

- Save it in Android under `Host/<folder>`. It is then in that folder on your PC.
- Or open **Android** in File Explorer and copy the file out.

---

Android is a trademark of Google LLC. Windows is a trademark of the Microsoft group of
companies. DSA is not affiliated with, endorsed by or sponsored by Google or Microsoft.
