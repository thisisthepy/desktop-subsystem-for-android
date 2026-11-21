# SettingsLib (AOSP, in `frameworks/base`)

Upstream: `platform/frameworks/base`, folder `packages/SettingsLib`, at the commit each image's
AOSP build was made from (one pin per Android version in `../apps.json`). It is not an app:
Settings and PermissionController both build their screens from it, so its patches are laid
over the Android tree once, before either app (`apps.py tree-order`). Each app's record names
SettingsLib's pin and patches too, so the image check refuses an app built without them.

Only `design/java/.../DsaDesign.java` is copied in (into
`CollapsingToolbarBaseActivity/src`), and the apps take it from here: a second copy of the class
in the same APK would not build.

`patches/0001-follow-the-dsa-design.patch`:

- `CollapsingToolbarDelegate` opens the large title expanded in a window 600 dp or more high and
  folded into the toolbar below it. SettingsLib's layouts open it folded on every page, and
  PermissionController does not let an overlay change that. A sub-page shown in the same
  activity follows the same rule, where SettingsLib folded it always.
- The large and the folded title are bold, with the variable font's weight set to match, and
  the title shrinks into the toolbar as the list scrolls rather than fading.
