# Calculator (LineageOS ExactCalculator)

Upstream: `LineageOS/android_packages_apps_ExactCalculator`, `lineage-24.0`, pinned in
`../apps.json`. The package is `com.android.calculator2` and the name is "Calculator" upstream
already, so nothing is renamed.

`patches/0001-follow-the-dsa-design.patch` points the theme (Material 3 Expressive with
dynamic colour upstream) at the shape roles, cards and large title of `dsa_design.xml`, so its
buttons, sheets and dialogs take the same corners as the other apps.

The calculator has no list and no title area (its top is the display of the expression), so
the card and the large title rules do not apply to it.
