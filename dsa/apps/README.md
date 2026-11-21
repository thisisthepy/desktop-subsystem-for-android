# Default apps

The guest image ships seven apps of its own: a clock, a gallery, a camera, a music player, a
calculator, a recorder and a file manager. No calendar is shipped: the user installs the one
they want.

| Folder | App | Upstream | Package |
| --- | --- | --- | --- |
| `deskclock` | Clock | LineageOS DeskClock | `com.android.deskclock` |
| `gallery` | Gallery | LineageOS Glimpse | `com.android.gallery3d` |
| `camera` | Camera | LineageOS Aperture | `com.android.camera2` |
| `music` | Music | LineageOS Twelve | `com.android.music` |
| `calculator` | Calculator | LineageOS ExactCalculator | `com.android.calculator2` |
| `recorder` | Recorder | LineageOS Recorder | `com.android.soundrecorder` |
| `documentsui` | Files | AOSP DocumentsUI | `com.android.documentsui` |

Each app takes the package name of the AOSP app it replaces, so an app that asks for the clock
or the gallery by its AOSP name finds it, and the image leaves the AOSP app out. The name users
see is the plain one ("Clock", "Gallery"), in every language the app is translated into. The
LineageOS apps already use those names, so no string is patched for it.

## Where the source comes from

Nothing upstream is kept here. `apps.json` pins each app: the LineageOS apps to a commit of
their `lineage-24.0` branch, and DocumentsUI to the commit each image's own AOSP build was
made from (that build's `manifest_<build_id>.xml` names it), one pin per Android version, so it
matches the framework it runs on. `apps.py` fetches a pin into `.scratch/apps/<app>/`, checks
the commit, copies the design files from `design/` into the app, and applies
`<app>/patches/*.patch` in order.

```
uv run dsa/apps/apps.py fetch camera
uv run dsa/apps/apps.py build camera --out .scratch/default-apps
```

## How each app is built

- The LineageOS apps carry their own Gradle builds. `apps.py build` runs the app's Gradle
  wrapper (`assembleRelease`) against the published Android SDK and the Maven libraries the
  app names, with no Android tree. DeskClock and ExactCalculator, which are Soong apps in the
  Android tree as well, have the same Gradle build upstream, so they build this way too.
- DocumentsUI builds only in an Android tree. It compiles against the system API, which no
  published SDK has, its feature flags are Java that aconfig generates, its resources are
  chosen by flag (`res/flag(...)`), and its statistics logging is code another tree tool
  generates. So it is built in the whole tree the Framework build's services job already
  fetches for `services.jar`. That tree is the published build's own manifest, so DocumentsUI
  there is at the pinned commit. `apps.py patch-tree` lays the design files and patches over
  `packages/apps/DocumentsUI` and turns DocumentsUI's Material 3 flag on in the tree's release
  configuration before the tree is analysed, the job builds the module's APK, and
  `apps.py record` names it the way `build` does.

The release APKs are unsigned (DocumentsUI's carries the tree's test key). `make-image.py
--default-apps <folder>` signs each with the image's keys, as it signs the agent: the release
key, and the platform key for DocumentsUI.

Each built APK goes to the output folder under the file name of its place in the image, with
`<app>.json` beside it: the app, its package, the source commit, and the SHA-256 of every patch
and design file it was built with. `make-image.py` refuses a folder whose record does not match
what this folder has now, and `check-image.py` refuses an image without the seven apps under
their AOSP package names, built from these pins and patches, or with an AOSP app they replace
still beside them.

## The design

Every app follows DSA's design: Material 3 (Expressive where the app's Material library has it)
with dynamic colour, rounded shapes, settings lists grouped into rounded cards, and a large bold
title that folds into the toolbar as the list scrolls. A large title is made for a phone in one
hand; a desktop window is often short, so a window lower than 600 dp opens with the title
already folded.

The shared part is in `design/`, copied into each app before its patches:

- `design/res/values/dsa_design.xml`: the corner sizes, the card and the large title styles.
- `design/java/.../DsaDesign.java`: the 600 dp rule for the large title.
- `design/java/.../CardGroups.java`: draws a list's rows as cards, one per run of rows between
  headers.
- `design/preference/.../PreferenceCards.java`: groups a settings list into a card per
  category. Only apps whose settings are a preference list get it.

Each app's own patch points its theme at these styles and calls the helpers from the screens
that have a title area. The changes stay in themes, layouts and a few lines of code; each app's
README says what its patches change and why.
