# Music (LineageOS Twelve)

Upstream: `LineageOS/android_packages_apps_Twelve`, `lineage-24.0`, pinned in
`../apps.json`. The name is "Music" upstream already.

`patches/0001-use-the-aosp-package-name.patch` sets the application ID to
`com.android.music`, the AOSP music player's, so apps that open the music player by name find
it. The code keeps its `org.lineageos.twelve` namespace.

`patches/0002-follow-the-dsa-design.patch`:

- The theme (Material 3 Expressive upstream) points the shape roles, cards and the large title
  at `dsa_design.xml`.
- The settings screen and the album, artist, genre and playlist pages
  (`CollapsingToolbarLayoutFragment`) open with their large title folded in a window lower than
  600 dp. The settings list groups its preferences into cards, one per category.
