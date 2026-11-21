# Share sheet (AOSP IntentResolver)

Upstream: `platform/packages/modules/IntentResolver`, at the commit each image's AOSP build was
made from (one pin per Android version in `../apps.json`). It builds only in an Android tree, in
the Framework build's services job, and the image puts it where the published one was
(`SYSTEM/priv-app/IntentResolver/`), signed with the platform key.

`patches/0001-follow-the-dsa-design.patch`:

- The sheet's title has a style of its own, bold and larger, where it took the framework's
  window title as it is.

The sheet's corners are the framework overlay's (`config_bottomDialogCornerRadius`), which the
sheet's background reads.
