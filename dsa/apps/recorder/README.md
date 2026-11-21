# Recorder (LineageOS Recorder)

Upstream: `LineageOS/android_packages_apps_Recorder`, `lineage-24.0`, pinned in
`../apps.json`. The name is "Recorder" upstream already.

`patches/0001-use-the-aosp-package-name.patch` sets the application ID to
`com.android.soundrecorder`, the AOSP sound recorder's, so apps that ask for a recorder by
that name find it. The code keeps its `org.lineageos.recorder` namespace.

`patches/0002-follow-the-dsa-design.patch`:

- The Material library moves from 1.9.0 to 1.14.0-alpha09, the version the other default apps
  use, because 1.9.0 has no Material 3 Expressive theme. The theme takes Material 3 Expressive
  and points the shape roles, cards and the large title at `dsa_design.xml`.
- The list of recordings gets a large collapsing title, folded at once in a window lower than
  600 dp, and its rows are drawn as one card (`CardGroups`).

The recording screen and the settings dialog have no title area or list, so they keep their
layout.
