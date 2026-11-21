# Settings (AOSP Settings)

Upstream: `platform/packages/apps/Settings`, at the commit each image's AOSP build was made
from (one pin per Android version in `../apps.json`). It builds only in an Android tree, in the
Framework build's services job, with SettingsLib's patches (`../settingslib`), and the image puts
it where the published Settings was (`SYSTEM_EXT/priv-app/Settings/`), signed with the platform
key. `make-image.py` still makes its edits of the compiled app on top (the Bluetooth
preferences, the list end, the large title's theme value), and the Settings overlay is built
against this APK.

`patches/0001-follow-the-dsa-design.patch`:

- The home page has its large title only in a window 600 dp or more high (the overlay's
  `layout-h600dp`). The activity handles size changes itself, so a window resized across that
  height kept the layout it opened with. It is now opened again when the height crosses 600 dp,
  and only then, so a resize within one side of the line does not rebuild it.
