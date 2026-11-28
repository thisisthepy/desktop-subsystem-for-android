# Package installer (AOSP PackageInstaller)

Upstream: `platform/frameworks/base`, folder `packages/PackageInstaller`, at the commit each
image's AOSP build was made from (one pin per Android version in `../apps.json`). It builds only
in an Android tree, in the Framework build's services job, and the image puts it where the
published one was (`SYSTEM/priv-app/PackageInstaller/`), signed with the platform key.

`patches/0001-follow-the-dsa-design.patch`:

- The install and uninstall dialogs always use their Material 3 design: a rounded dialog with
  its buttons side by side in the dynamic colours. The published app chooses it by a device
  configuration value that the build leaves off, and draws its older dialog otherwise.
- The dialog title is bold.
