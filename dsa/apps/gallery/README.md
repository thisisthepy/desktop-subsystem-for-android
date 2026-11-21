# Gallery (LineageOS Glimpse)

Upstream: `LineageOS/android_packages_apps_Glimpse`, `lineage-24.0`, pinned in
`../apps.json`. The name is "Gallery" upstream already.

`patches/0001-use-the-aosp-package-name.patch` sets the application ID to
`com.android.gallery3d`, the AOSP gallery's, so apps that open the gallery by name find it.
The code keeps its `org.lineageos.glimpse` namespace: only the installed package changes.

`patches/0002-follow-the-dsa-design.patch`:

- The theme and the media viewer's theme take Material 3 Expressive, and the theme points the
  shape roles, cards and the large title at `dsa_design.xml`.
- The settings screen opens with its large title folded in a window lower than 600 dp, and
  groups its preferences into cards, one per category.
