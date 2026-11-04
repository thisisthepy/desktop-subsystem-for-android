# Graphics checks and measurements

`verify.py` checks the graphics path of an installed DSA and measures it. Every app's
OpenGL ES driver must be ANGLE, Vulkan must run on the host through the remoting driver, a
fixed Vulkan CTS subset must pass, one OpenGL ES app and one Vulkan app must draw in their
task windows, and three workloads are measured.

## What CI does and what needs a PC

The "Graphics test" workflow checks what a hosted runner can:

- every pattern in `vk-cts-subset.txt` still names cases of the pinned Android CTS
  (`verify.py check-subset`), and the expanded case list is kept;
- the two test apps are built from Android's NDK samples (Apache-2.0, fetched at a pinned
  commit by `build-test-apps.sh`) and kept as the `graphics-test-apps` artifact;
- the host renderer runs on software Vulkan (Lavapipe) and serves a gfxstream client.

Running the CTS cases, the apps and the measurements needs a booted guest. That needs
Windows with Hyper-V and a GPU, which hosted runners do not have, so these run on a PC.

## On a PC

Install a debug image (a user build has no adb), connect adb to the guest, and run from the
repository root:

```text
uv run dsa/graphics/verify/verify.py fetch
uv run dsa/graphics/verify/verify.py cts
uv run dsa/graphics/verify/verify.py apps
uv run dsa/graphics/verify/verify.py measure --seconds 30
```

- `fetch` downloads the Vulkan CTS APK (only that member of the pinned CTS archive, by range
  requests) and the test apps from the latest successful "Graphics test" run (needs `gh`).
  Everything goes to `.scratch/graphics-verify/`.
- `cts` checks that the guest reports ANGLE and the remoting Vulkan driver, runs the subset,
  and prints each case that did not pass. It exits non-zero on any failure.
- `apps` starts hello-gl2 and hello-vulkan, checks that each presents frames, and asks
  whether each window shows what it should (`--yes` skips the question).
- `measure` scrolls Settings, then runs hello-gl2 and hello-vulkan as continuously rendering
  loads, and prints frame time (median, p90, p99, max, janky frames), guest CPU, guest memory
  in use, and the CPU and resident memory of `vmmem`, `dsa-wm` and the app windows. Reading
  `vmmem` needs an administrator prompt. The report is saved as JSON beside the downloads.

Record the printed numbers with the build and the machine (CPU, GPU, driver, display).
