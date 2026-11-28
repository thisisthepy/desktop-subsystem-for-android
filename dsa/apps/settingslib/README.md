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
- Every preference group is a card on its own surface (the lowest surface container in light,
  a high one in dark), which stands clear of the window behind it. The outer corners are 26 dp
  and the corners between two rows of a card 6 dp, and each group title has 28 dp of room above
  it, so one group reads apart from the next.
- A pressed row morphs its shape: its corners take the outer radius while it is held, and fade
  back when it is let go. It also sinks a little and springs back, drawn with a path shaped like
  a spring's response, as the framework's animators take a path and not spring physics.
- Group titles are emphasized: bold, in the accent colour.

The image lays nothing over these: the Settings overlay and the edits `make-image.py` makes in the
compiled Settings and PermissionController leave the cards, their colour, radius and spacing,
and the titles to this source, and `check-image.py` refuses an image that overrides one of them.
