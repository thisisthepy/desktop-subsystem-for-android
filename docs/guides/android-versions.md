# Android versions

Desktop Subsystem for Android™ (DSA) supports the newest two Android major versions:
today Android 16 and Android 17. By default it runs the newest one. Some apps run on only
one of them, so you can choose.

## See which version runs

Open the **Android** page of DSA Settings. **Running now** shows the version, its build and
its security patch level. Or run:

```
dsa android version   # the version that runs, and the ones you can choose
```

## Change the version

Every user of the PC runs the same Android version, so only an administrator can change
it.

1. Open the **Android** page of DSA Settings.
2. Under **Version to use**, pick a version, or **Always the newest**.
3. Select **Change**, and approve the administrator prompt.

The chosen version is downloaded and used from the next start of Android. From an
administrator terminal:

```
dsa android use 16
dsa android use latest
```

### Moving to an older version erases data

Android cannot read data that a newer version wrote. Moving to an older major version
therefore erases the Android data of every user of the PC: the apps they installed, their
accounts and all app data. DSA's settings stay. This cannot be undone.

DSA Settings asks you to confirm with **Erase data and change**. On the command line, add
`--erase-data`:

```
dsa android use 16 --erase-data
```

Moving to a newer version keeps your apps and data.

## Updates

- The chosen version keeps receiving security updates.
- When a version leaves support, DSA moves to the next one and keeps your apps.
- DSA never moves to an older major version on its own.
- Each image is checked against a signed list before it is used. An image that fails to
  boot is replaced by the previous one.

An administrator can turn image updates off with **Update the Android image automatically**
on the **Updates** page of DSA Settings, or with `dsa settings set image-updates off`.

---

Android is a trademark of Google LLC. Windows is a trademark of the Microsoft group of
companies. DSA is not affiliated with, endorsed by or sponsored by Google or Microsoft.
