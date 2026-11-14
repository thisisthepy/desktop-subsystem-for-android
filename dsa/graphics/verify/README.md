# Graphics checks and measurements

`verify.py` checks the graphics path of an installed DSA and measures it. Every app's
OpenGL ES driver must be ANGLE, Vulkan must run on the host through the remoting driver, a
fixed Vulkan CTS subset must pass, one OpenGL ES app and one Vulkan app must draw in their
task windows, and three workloads are measured.

## What CI does and what needs a PC

The "Graphics test" workflow checks what a hosted runner can without a guest:

- every pattern in `vk-cts-subset.txt` still names cases of the pinned Android CTS
  (`verify.py check-subset`), and the expanded case list is kept;
- the two test apps are built from Android's NDK samples (Apache-2.0, fetched at a pinned
  commit by `build-test-apps.sh`) and kept as the `graphics-test-apps` artifact;
- the host renderer runs on software Vulkan (Lavapipe) and serves a gfxstream client.

The "Image test" workflow boots the image it made under the Linux host, with the renderer on
Lavapipe, and on that one boot (`dsa/host/platform/linux/tests/graphics.rs`):

- installs hello-gl2 (OpenGL ES, drawn through ANGLE) and hello-vulkan through the local API,
  opens each in a task window, and waits until the host places the task's frames in a window;
- reads each window through adb (`screencap`, or a `screenrecord` frame when the capture is
  empty) and checks it shows the app's triangle (green for hello-gl2, dark red for
  hello-vulkan), point up, in the middle of a grey background, and that the background has
  faded on 3 s later, since both apps change it on every frame;
- runs the CTS subset with `verify.py cts --software-vulkan`, and keeps each case's result
  (`vk-cts-results.json` and `.txt`), the dEQP log and the guest log in the
  `graphics-check-results` artifact, beside the window pictures.

Lavapipe is not a GPU. A case that cannot pass on it for a reason Lavapipe gives is listed in
`vk-cts-subset.txt` as `lavapipe: <pattern>  # <the reason>`: it still runs in CI and its
result is kept, but its failure fails only a run without `--software-vulkan`. None is listed
yet.

The measurements need a real GPU, so they run on a PC, as does the CTS subset on that GPU.

## On a PC

Turn developer mode on (`dsa settings set developer-mode on`, or the Developer section of
the settings app), start Android, and run from the repository root. The commands connect adb
to `127.0.0.1:58526` themselves; `--serial` names another address, such as the one
`dsa settings` shows when the `adb-port` setting picks another port:

```text
uv run dsa/graphics/verify/verify.py fetch
uv run dsa/graphics/verify/verify.py cts
uv run dsa/graphics/verify/verify.py apps
uv run dsa/graphics/verify/verify.py measure --seconds 30
```

- `fetch` downloads the Vulkan CTS APK (only that member of the pinned CTS archive, by range
  requests) and the test apps from the latest successful "Graphics test" run (needs `gh`).
  The run is looked up in the repository this checkout's `origin` remote names, so a fork's
  clone takes the fork's own builds; `--repo owner/name` names another one, and `--run` one
  run. Everything goes to `.scratch/graphics-verify/`.
- `fetch-cts` downloads only the Vulkan CTS APK and writes the expanded case list, for a
  machine that has the test apps already.
- `cts` checks that the guest reports ANGLE and the remoting Vulkan driver, runs the subset,
  and prints each case that did not pass. It exits non-zero on any failure. A case that ends
  the CTS process is a crash and fails the subset, and the cases after it run in a new
  process, so one crash does not hide the rest. A run is over only when the CTS process has
  been gone for two polls in a row: Android recreates the CTS activity when its window changes
  size just after it opens, and the CTS then starts again in a new process. `--results DIR`
  keeps each case's result, the dEQP log and the guest log in `DIR`; `--software-vulkan`
  excuses the `lavapipe:` cases, for a renderer on Lavapipe. `DSA_ADB` names the adb to use.
- `apps` starts hello-gl2 and hello-vulkan, checks that each presents frames, and asks
  whether each window shows what it should (`--yes` skips the question).
- `measure` scrolls Settings, then runs hello-gl2 and hello-vulkan as continuously rendering
  loads, and prints frame time (median, p90, p99, max, janky frames), guest CPU, guest memory
  in use, and the CPU and resident memory of `vmmem`, `dsa-wm` and the app windows. Reading
  `vmmem` needs an administrator prompt. The report is saved as JSON beside the downloads.

Record the printed numbers with the build and the machine (CPU, GPU, driver, display).
