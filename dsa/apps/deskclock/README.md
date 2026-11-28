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

`patches/0002-follow-the-day-and-night-theme.patch`:

- Upstream is dark only, so its text styles (the big time, the date, labels, the empty alarm
  list), the inactive tabs, the city list, the alarm screen and the stopwatch marker are white
  or translucent white. In the light theme they showed white on a light background. They take
  the theme's `colorOnSurface` and `colorOnSurfaceVariant` instead, and the translucent colours
  are black in the light theme and white in the dark one (`values-night/dsa_colors.xml`).
- The widgets keep their own colours.

`patches/0003-tint-the-icons-from-the-theme.patch`:

- Upstream's icons are vector drawables filled white with no tint, and several places show
  them untinted: the toolbar's settings action, the alarm's ringtone, vibrate and delete
  icons, the alarm volume setting, the alarm screen's alarm button and the analog clock. Each
  takes a default tint from the theme (`colorControlNormal`, or `colorOnSurface` for the clock
  and the alarm button), which a place that tints the icon still replaces. The expand caret
  and the timer's close button stay white, on their grey disc.
- On the alarm screen the dismiss button's circle takes `colorOnSurface`, so its icon (the
  window's colour) shows on it, and the snooze icon takes `colorOnSecondary`, which reads on
  the snooze circle.
- The screensaver is always on black, so its layout takes the dark theme's colours
  (`ThemeOverlay.Material3.Dark`) for the date, the next alarm and the analog clock.

The clock is allowed to run its alarms while the guest saves power, and is granted its
notification permission from the first boot, as LineageOS does (`sysconfig` and
`default_permissions` in `../apps.json`).
