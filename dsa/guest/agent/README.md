# guest/agent

The agent: a system app in the guest that tells the host about each app task, so the host
can show each task as a window of its own, and acts on the host's requests. It is a
persistent, platform-signed privileged app (`io.github.thisisthepy.dsa.agent`), installed
by the image in `/system_ext/priv-app/DsaAgent/`. The host is the shell, so the agent shows
nothing. Its one activity is the guest's home, which draws nothing (see "The home activity").

## What it does

- **Freeform display.** At boot it turns freeform windows on (the
  `enable_freeform_support` and `force_resizable_activities` global settings) and sets the
  default display's windowing mode to freeform through the window manager. Every task
  then has bounds of its own on the one display. The window manager takes the setting
  through an observer, so the agent sets the mode and checks again until it holds.
- **Task placement.** No two app tasks overlap on the display. Android's freeform launch
  puts every new task at the same default place, and on one shared display a covered task
  shows in no window, so the agent moves a task that overlaps another (see below).
- **Task reports.** It sends `TaskUpdated` (task ID, bounds, package, label, focus) when an
  app task appears or any of those change, and `TaskRemoved` when the task ends. Only app
  tasks are reported: not its own, not home or recents, and not a task with no package.
- **Host requests.** `LaunchApp` starts the package's launcher activity as a new task in a
  freeform window (or brings its task to the front). `CloseTask` ends the task, but only a
  task the host was told about. `ResizeTask` gives a task its host window's client size,
  holding the app's configuration while the user drags the window's edge, and
  `SetTaskDensity` the density of the window's monitor (see below); both are refused
  for a task the host was not told about.
- **Density and size limits in reports.** Each `TaskUpdated` also carries the density the
  task's app draws at and the display's size, which is the largest a task can be.
- **Pointer input.** The host sends the mouse, touch and pen input of each task's window
  (`PointerEvent`, relative to the task). The agent injects it into the task at the
  place it last reported for the task (see below).
- **Key input.** The host sends the keys pressed in each task's window (`KeyEvent`). The
  agent gives the task the input focus if it does not hold it, and injects the key (see
  below).
- **The channel.** It reaches the host through the control daemon's binder service,
  `io.github.thisisthepy.dsa.channel.IChannel/default` (`dsa/guest/aidl/`). It looks for
  the service every second until the daemon is up, subscribes, and sends every app task
  again on each new connection. When the daemon dies it waits for it to come back.

A frame that does not decode is logged and dropped. An exception in the agent's work is
logged and the agent carries on. Its log tag is `DsaAgent`.

## How it sees tasks

A task stack listener tells it when a task is created, removed, moved to the front,
focused, or given a new description, and on any change to the task stack. The agent then
reads every running task (`getRunningTasks`, which shows a privileged app every task) and
sends only what changed. A burst of changes is read once.

A task organizer would report bounds directly, but registering one hands the organizer
every task's surface and its transitions, which the guest's composer must keep, and only
one organizer is active at a time. With no SystemUI and no window decorations in the guest,
only the agent moves or resizes a task: its bounds are set when it starts, which the
listener reports, and later only by the agent, which reads the tasks again until the
change lands (below).

## Where tasks sit on the display

The hwcomposer gives each task the layers inside its bounds, and SurfaceFlinger does not
draw a window that another one covers. Two tasks at the same place would show one app in
both host windows, or nothing in one of them. Where a task sits on the guest display is
never seen, because the host places each window itself, so any packing works.

Each time the agent reads the tasks, it goes through the app tasks by task ID, oldest
first. A task that lies inside the display and overlaps no older task stays where it is.
Any other task moves to the highest free place, then the leftmost, among the older ones
(`windowing/TaskPlacement.kt`). A task only ever moves: its size is the one Android gave
it. So when an older task grows into a newer one, the newer one moves.

The move goes through `IActivityTaskManager.resizeTask`, the call behind
`cmd activity task resize`. It works for any task in a freeform window, whichever way the
task appeared: launched by the host, started by another app, or restored. The agent does
not give launch bounds to `LaunchApp` instead. The window manager picks a new task's size
from the app's layout and the display, so the agent cannot know it before the launch, and
launch bounds would replace that size. Launch bounds also do nothing for a task that is
already running, or for one an app starts itself.

The window manager may apply a move later, and a move does not always call the task stack
listener. So a task that is being moved is reported at the place it is going to, and the
agent reads the tasks again every 100 ms until the move lands. After 2 seconds it gives
up, logs it, reports the task where it is, and leaves the task there until its bounds
change.

## Size and density from the host

The host shows each task 1:1 in its window, so a window's client size in host pixels is the
task's size in guest pixels, and the task's density keeps the app the size it should be on
the window's monitor (160 dpi at 100%, 320 at 200%).

`ResizeTask` asks for a size. The task keeps its top left corner and takes the size, cut
down to the display and moved back inside it if it would cross an edge. If the task then
overlaps another, the placement above moves the newer of the two. Only the latest size of
each task counts. While one resize is under way, the next waits until it lands: the agent
reads the tasks every 16 ms while a resize is pending, so the next size goes out within a
frame of the last one landing, and the window manager never works through sizes the user
has already dragged past. A resized task is reported at its new bounds at once, as a moved
one is. A resize that has not landed after 2 seconds is given up, logged, and the task
reported where it is.

`SetTaskDensity` asks for a density. A density and a size that arrive together are applied
in one window container transaction (`WindowContainerTransaction.setBounds` and
`setDensityDpi`, through `IActivityTaskManager.getWindowOrganizerController().
applyTransaction`), so the app relays out once. This is how the framework's own desktop
windowing gives a task a density of its own. The density is part of the task's
configuration, so tasks on the one display each draw at their own density. On a framework
without these calls the task still gets its size through `resizeTask`, draws at the
display's density, and the agent logs it once.

**While the user drags.** A `ResizeTask` with `live` set is a step of a drag that has not
ended. A new size changes the task's screen size in dp, and with it the orientation, the
screen layout and the smallest width, so the window manager relaunches the app's
activities, which takes far longer than a frame. On an Android 16 image, Clock and
Settings showed a stretched frame for 156 ms (median) at each step. So at the first live
step the agent takes the screen size the task's configuration has then
(`screenWidthDp`, `screenHeightDp` and `smallestScreenWidthDp`, read with the running
tasks), and every step gives the task its new bounds while its configuration keeps that
screen size, in one transaction (`setBounds`, `setScreenSizeDp` and
`setSmallestScreenWidthDp`). Only the task's window configuration changes, which does not
relaunch anything, so the app lays itself out again in the new bounds within a frame.
Split screen holds a task's configuration the same way while its divider is dragged.

The first `ResizeTask` without `live` ends the drag, even when its size is the one the task
already has. The agent gives the task that size and clears the held screen size (a screen
size of 0, `Configuration.SCREEN_WIDTH_DP_UNDEFINED` and the others), with the density the
host last asked for, in one transaction, so the app is reconfigured once, at its final
size. The end of a drag waits behind a step that has not landed, as any size does, and a
live size that arrives after it does not replace it. A density that arrives during a drag
ends the hold too, because a screen size held at the old density would be wrong at the new
one. A `ResizeTask` without `live` outside a drag is the plain resize above. On a framework
without these calls each step is a plain resize, and the agent logs it once.

**Capacity.** A phone app's default freeform size on the default 1920x1080 display is
548x974, so three such tasks fit side by side. A fourth goes where it shares the fewest
pixels with the others (beside three such tasks, the bottom right corner, over a strip of
the third) and the agent logs a warning. Tasks that only partly overlap still get frames
of their own, because neither holds the other's surface, but a touch in the shared part
goes to the upper task. A bigger guest display holds more tasks
(`androidboot.dsa.display_width` and `display_height`, in `dsa/guest/hwcomposer/`).

## Pointer input

The host sends one `PointerEvent` per pointer as it happens, with the position relative to
the task. The agent adds the task's origin on the display, as it last reported it to the
host, and injects a `MotionEvent` with `InputManager.injectInputEvent`, without waiting
for the app (`input/PointerInput.kt`). Input for a task the host was not told about is
dropped.

- The mouse is `SOURCE_MOUSE`. A press is `ACTION_DOWN`, then `ACTION_BUTTON_PRESS` for
  the button, so the right button is `BUTTON_SECONDARY`: a context click, not a long
  press. A second button while one is down is only a button press. Hovering begins with
  `ACTION_HOVER_ENTER` and ends before a press.
- The wheel is `ACTION_SCROLL` with `AXIS_VSCROLL` and `AXIS_HSCROLL`, in notches.
- Fingers are `SOURCE_TOUCHSCREEN` and a pen is `SOURCE_STYLUS` (its eraser end
  `TOOL_TYPE_ERASER`). Every finger down goes in each event of a touch: the second finger
  is `ACTION_POINTER_DOWN`, and lifting all but the last is `ACTION_POINTER_UP`.

Injection rather than a uinput device: a uinput device needs `/dev/uinput` opened by a
process the SELinux policy allows, which an app is not, and a mouse there reports movement,
not places. Injection takes each event's place, source, tool and buttons as they are.

## Key input

The host sends one `KeyEvent` per key press, repeat and release, with Android's key code,
the repeat count, the meta state (Shift, Ctrl, Alt and Meta with their sides, and the
locks) and the time on the host's clock (`input/KeyInput.kt`).

- Android sends a key to the focused window, so before the first press in a task that does
  not hold the input focus, the agent focuses it (`IActivityTaskManager.setFocusedTask`).
  Moving between host windows with the keyboard alone moves the guest's focus too.
- The key is injected on the default display as `SOURCE_KEYBOARD` from the framework's
  virtual keyboard, whose key character map is a full US keyboard, so a letter, digit or
  punctuation key with its meta state types its character into a text field.
- A held key keeps the time it went down; its repeats and its release are placed after it
  by the host's times. A release for a key the agent never saw go down is dropped. A
  release the host marks canceled (the window lost the focus) carries `FLAG_CANCELED`, so
  the app lets the key go without acting on it.

## The home activity

Android always keeps a home activity at the bottom of the display, and gives it input focus
whenever no app task has focus. The guest has no launcher, so the agent declares the home
activity itself (`platform/HomeActivity.kt`). It is not a launcher: it has no content, its
theme is transparent, it is not in recents, and it is not in the launcher category, so it
is in no app list. Its task is a home task, which the agent never reports, so the host shows
no window for it, and the hwcomposer sends no layer of it, because no app task's bounds
contain a display-wide layer.

Without it, Android used Settings' fallback home, `FallbackHome`, which runs in the Settings
process. A window must answer each focus change within 5 seconds. While the Settings
process was busy, for example starting cold or creating the Settings activity the host had
launched, `FallbackHome` answered too late. With no dialog to show, the activity manager
then killed the whole process, and the user's Settings task ended with it. Each restart of
the process started cold again, so it could repeat.

The agent's home runs in the agent's own process. That process is persistent, so it is
always running and never starts cold, and its main thread does nothing else, because the
agent's work runs on a thread of its own. So the home answers at once, and no app's process
holds the home task.

## Framework interfaces outside the SDK

A platform app may use framework interfaces the SDK does not publish. The agent uses:

| Interface | For |
|---|---|
| `TaskStackListener`, `ActivityTaskManager.getService()` | Task change callbacks, ending a task (`removeTask`), moving a task (`resizeTask`) |
| `TaskInfo.configuration`, its `windowConfiguration`, `TaskInfo.isFocused` | A task's density, bounds, activity type and focus (reflection) |
| `WindowContainerTransaction`, `TaskInfo.token`, `IActivityTaskManager.getWindowOrganizerController` | A task's bounds and density in one change (reflection) |
| `IWindowManager.get/setWindowingMode` | The display's windowing mode (reflection) |
| `ActivityOptions.setLaunchWindowingMode` | A freeform window for a launched app (reflection) |
| `ServiceManager.checkService` | The control daemon's service (reflection) |
| `Binder.markVintfStability` | Marking the listener the daemon calls as a stable interface (reflection) |
| `InputManager.injectInputEvent`, `MotionEvent.setActionButton`, `InputEvent.setDisplayId` | Injecting the host's pointer and key input (reflection) |
| `IActivityTaskManager.setFocusedTask` | Giving a task the input focus before its keys |

`hidden-api/` declares the classes the agent links against. It is compile-only: at run time
the classes are the guest's own.

The channel's binder classes are generated at build time from `dsa/guest/aidl/` as a stable
(VINTF) interface. The generated stubs call `Binder.markVintfStability()`, which the SDK
lacks; the build points that call at `VintfStability.mark()`, which reaches the same method.
The protocol messages come from the generated Java codec in `dsa/protocol/java/`.

## Permissions

Every permission in `src/main/AndroidManifest.xml` is listed in
`privapp-permissions-dsa-agent.xml`, which the image installs next to the agent. The
framework refuses to boot when a privileged app requests a privileged permission its list
does not grant, so the two must change together; `test_make_image.py` checks that they do.

## Building and testing

Needs a JDK (17 or later) and the Android SDK (`ANDROID_HOME`, or `sdk.dir` in
`local.properties`). From this folder:

```
./gradlew testDebugUnitTest assembleRelease
```

The unit tests run on the JVM against fakes of the framework, the channel and the
scheduler, and check the frames with the generated codec. Everything that touches the
framework sits behind a small interface in `platform/`.

The release APK, `build/outputs/apk/release/dsa-agent-release-unsigned.apk`, is unsigned.
`make-image.py --agent` adds it to the image and signs it with the platform key.

| Path | What it is |
|---|---|
| `src/main/kotlin/.../agent/` | The agent: task reports, requests, pointer input, the channel client, the freeform setup, task placement |
| `src/main/kotlin/.../agent/platform/` | The framework side: the application, task source, actions, windowing, input injection, binder channel |
| `src/test/` | The unit tests |
| `hidden-api/` | Compile-only declarations of framework classes outside the SDK |
| `privapp-permissions-dsa-agent.xml` | The agent's privileged permissions |
