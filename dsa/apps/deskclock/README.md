# Clock (LineageOS DeskClock)

Upstream: `LineageOS/android_packages_apps_DeskClock`, `lineage-24.0`, pinned in
`../apps.json`. The package is `com.android.deskclock` and the name is "Clock" upstream
already, so nothing is renamed.

`patches/0001-follow-the-dsa-design.patch`:

- The app theme and the settings toolbar theme follow the system's light or dark theme
  (`DynamicColors.DayNight`) instead of always dark, because the design has both themes. The
  theme points the Material shape roles, cards and the large title at `dsa_design.xml`.
- `toolbar_base_layout.xml`, the frame of the settings, ringtone and city screens, wraps its
  toolbar in a large collapsing title, and the title bar takes the surface colour instead of the
  primary one, so the large title reads as part of the page.
- `ToolbarBaseActivity` folds that title at once in a window lower than 600 dp.
- The settings and screensaver settings lists group their preferences into cards, one per
  category (`PreferenceCards`).

The clock is allowed to run its alarms while the guest saves power, and is granted its
notification permission from the first boot, as LineageOS does (`sysconfig` and
`default_permissions` in `../apps.json`).
