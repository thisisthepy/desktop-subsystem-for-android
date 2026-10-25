# guest/hwcomposer

`dsa-hwcomposer` is the guest's composer HAL (`android.hardware.graphics.composer3`,
version 4). SurfaceFlinger composes one freeform display through it, as on any device.
The host never receives that display. The hwcomposer cuts each frame into one frame per
task and sends each to its own host display, so each host window shows its own app and
nothing else.

## How a frame travels

1. The agent reports every task's ID and bounds (`TaskUpdated`, `TaskRemoved`) on the
   control channel. The hwcomposer subscribes to `dsa-channeld`'s binder service
   (`io.github.thisisthepy.dsa.channel.IChannel/default`) and keeps the latest bounds.
2. At each `presentDisplay`, it splits the display's layers among the tasks
   (`src/task_layout.cpp`, rules below).
3. Each task gets a host display the first time it has a layer. The hwcomposer creates it
   with renderControl (`rcCreateDisplayById`), sized to the task's bounds, and sends
   `TaskFrames {task_id, display_id}` before the first frame. A task keeps its display until
   `TaskRemoved`, when the display and its ColorBuffers are released.
4. The task's layers are composed on the host GPU with `rcComposeWithoutPost`
   (`ComposeDevice_v2`) into a ColorBuffer the size of the task's bounds. Layer frames are
   moved so the task's top left corner is the origin. The request names display 0, not
   the task's: given a display, the host would make the target that display's frame
   before composing into it, and the window would show each frame one late. Once the
   compose has finished, the hwcomposer makes the ColorBuffer the task display's current
   one (`rcSetDisplayColorBuffer`), which is when the host copies it into the window. Two
   ColorBuffers per task alternate, so the host can show one while the next is composed.

Composition finishes before `presentDisplay` returns, so the hwcomposer hands back no
release fences and declares `PRESENT_FENCE_IS_NOT_RELIABLE`.

## Which layers belong to which task

A layer belongs to the task whose bounds contain its display frame, clipped to the
display. When several tasks contain it (one window over another), the layer goes to:

1. the task whose bounds equal the frame exactly, if only one does (the task's own surface);
2. otherwise the task of the nearest layer below it that one of those tasks owns, since a
   task's layers are adjacent in z order;
3. otherwise the smallest of those tasks, then the lowest task ID.

A layer no task contains is never sent: the wallpaper, display-wide backgrounds and dim
layers, and anything that straddles a window's edge. Sending it would put pixels that
belong to no window, or to another one, into a host window. A popup that reaches outside
its window is left out for the same reason.

SurfaceFlinger composes some layers itself (client composition). For a task with such
layers, the part of the client target their frames cover is composed where the task's
lowest client layer sits.

## What the host does with it

- `TaskFrames {task_id, display_id}` tells the host that `display_id` carries the task's
  frames. Display IDs start at 1 (display 0 is the host renderer's own) and are reused
  after a task ends.
- Each frame is an `rcSetDisplayColorBuffer(display_id, colorBuffer)` with a ColorBuffer
  that is already composed. Its size is the task's bounds. The hwcomposer never sets a
  ColorBuffer on display 0.
- The hwcomposer sends `TaskFrames` again for every pairing whenever it subscribes to
  the channel anew, and after reconnecting to a restarted host renderer.

The emulator's host renderer allows eleven displays in all.
`dsa/graphics/patches/0005-*.patch` raises that to 64 for displays created by ID, which
gives 63 task windows; past that a task gets no frames and the hwcomposer logs it.

## Buffers shared between processes

An app's buffer has to reach the same host ColorBuffer in the app, in SurfaceFlinger and
in the hwcomposer. Without virtio-gpu, a gralloc buffer carries no host resource, so every
process derives the ColorBuffer handle from the buffer ID minigbm writes into the handle
(`dsa/graphics/include/dsa_shared_color_buffer.h`). The first process to use the buffer
creates the ColorBuffer with that handle; the others open it. The guest Vulkan driver does
the same (`dsa/graphics/patches/0002-*.patch`). The hwcomposer closes its reference when
SurfaceFlinger can no longer show the buffer, and the host frees whatever a process still
holds when the process dies.

## Display

The display's size and density come from the kernel command line, which
`make-image.py` writes into `image.json`:

| Property | Default |
|---|---|
| `androidboot.dsa.display_width` | 1920 |
| `androidboot.dsa.display_height` | 1080 |
| `androidboot.dsa.display_dpi` | 160 |
| `androidboot.dsa.display_refresh_rate` | 60 |

A window cannot be larger than this display.

## Build and test

```
dsa/guest/hwcomposer/build.sh .scratch .scratch/build-hwcomposer
dsa/guest/hwcomposer/test.sh
```

`build.sh` fetches the pinned sources (`fetch.sh`): the frozen composer3 AIDL definitions
from `hardware/interfaces`, the binder NDK headers from `frameworks/native`, protobuf 3.21.12,
and the gfxstream guest tree with DSA's patches (through `dsa/graphics/fetch.sh`). It
generates the binder code with the Android SDK's `aidl` and builds with the NDK. The
binary links only `libc`, `libdl`, `liblog` and `libbinder_ndk`; C++ is linked in, because
the NDK's libc++ is not the platform's. `dsa/graphics/check-guest-driver.py --executable`
checks every import resolves against the image.

`test.sh` runs the task rules (`tests/task_layout_test.cpp`) and the channel messages
against the protocol vectors (`tests/channel_messages_test.cpp`) on the build machine. It
needs a C++17 compiler and protobuf-lite 3.21 (Debian: `libprotobuf-dev`).

The composer is DSA's own code, written after ranchu's hwc3
(`device/generic/goldfish-opengl`, `system/hwc3`). Ranchu's builds only inside AOSP: it
needs libui, the HIDL composer resources and libdrm, none of which the NDK has, and its
composition paths (DRM, guest composition) do not apply to a VM without a display device.

## Files

| File | What it is |
|---|---|
| `src/task_layout.*` | The rules: which layer belongs to which task, task bounds, display pairings |
| `src/compose_request.*` | The `ComposeDevice_v2` request renderControl's `rcCompose` takes |
| `src/channel_messages.*` | `TaskUpdated`, `TaskRemoved` and `TaskFrames`, through the generated codec |
| `src/task_composer.*` | Per-task composition and posting, display lifetimes, shared ColorBuffers |
| `src/host_renderer.*`, `src/host_stream.*` | renderControl over vsock to the host renderer |
| `src/channel_client.*` | The binder client of `dsa-channeld` |
| `src/composer*.*`, `src/display.*`, `src/vsync_thread.*`, `src/main.cpp` | The composer HAL SurfaceFlinger talks to |
| `dsa-hwcomposer.rc`, `dsa-hwcomposer.xml` | Its init service and VINTF declaration, installed by `make-image.py` |
| `binder_ndk_platform_stub.c` | Link-time stand-ins for the platform's binder calls the NDK leaves out |
