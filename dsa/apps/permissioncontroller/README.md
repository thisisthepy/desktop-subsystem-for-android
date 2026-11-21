# Permissions (AOSP PermissionController)

Upstream: `platform/packages/modules/Permission`, folder `PermissionController`, at the commit
each image's AOSP build was made from (one pin per Android version in `../apps.json`). It builds
only in an Android tree, in the Framework build's services job, with SettingsLib's patches
(`../settingslib`), which give its permission screens the large title rule. It lives in the
permission APEX, so it has no install path of its own: the job builds the APK target that
`apk_pattern` names, and `make-image.py` puts it into the APEX's payload in place of the
published APK, then makes its compiled edits (the cards) and packs the APEX again.

`patches/0001-follow-the-dsa-design.patch`:

- The permission dialog's title is bold.
- The dialog's buttons asked for a font family the guest does not ship; they use the system's
  medium sans serif, so the font is the image's own and not a fallback chosen by chance.
