# Files (AOSP DocumentsUI)

Upstream: `platform/packages/apps/DocumentsUI`, at the commit each image's AOSP build was made
from (one pin per Android version in `../apps.json`), so the app matches the framework it runs
on. The package is `com.android.documentsui` already, and the image puts it where the published
DocumentsUI was (`SYSTEM/priv-app/DocumentsUI/`), signed with the platform key, as the
published one is.

It builds only in an Android tree, in the Framework build's services job (see `../README.md`).

`patches/0001-follow-the-dsa-design.patch`, with the Material 3 flag:

- `apps.py patch-tree` turns on DocumentsUI's own Material 3 look (the `use_material3` flag) in
  the tree's release configuration. The published Android 16 build has it on already; Android
  17's does not, and without it the app draws its older look and none of the rules below.
- The Material 3 theme points the shape roles, cards and the large title at
  `dsa_design.xml`.
- The app bar, which already has a large collapsing title in the Material 3 look, opens folded
  in a window lower than 600 dp: `BaseActivity.expandAppBar`, which every move to another
  folder calls, expands it only in a higher window.

The image's system screen overlay for DocumentsUI is built against this APK, not the published
one, so the overlay matches the app the image has.
