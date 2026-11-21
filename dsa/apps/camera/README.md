# Camera (LineageOS Aperture)

Upstream: `LineageOS/android_packages_apps_Aperture`, `lineage-24.0`, pinned in
`../apps.json`. The name is "Camera" upstream already. The camera works when the VM has a
camera (the host lends its cameras to the guest's camera HAL). The app must also stay open on
a VM with no camera; that has not been checked on such a VM yet.

`patches/0001-use-the-aosp-package-name.patch` sets the application ID to
`com.android.camera2`, the AOSP camera's, so apps that open the camera by name find it. The
code keeps its `org.lineageos.aperture` namespace.

`patches/0002-follow-the-dsa-design.patch`:

- The Material library moves from 1.9.0 to 1.14.0-alpha09, the version the other default apps
  use, because 1.9.0 has no Material 3 Expressive theme. The theme and the viewfinder's theme
  take Material 3 Expressive, and the theme points the shape roles, cards and the large title at
  `dsa_design.xml`.
- The settings screen opens with its large title folded in a window lower than 600 dp, and
  groups its preferences into cards, one per category.

The viewfinder has no title area, so the large title rule does not apply there.
