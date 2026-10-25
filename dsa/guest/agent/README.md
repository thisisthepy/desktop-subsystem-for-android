# guest/agent

The agent: a system app in the guest that tells the host about each app task, so the host
can show each task as a window of its own, and acts on the host's requests. It is a
persistent, platform-signed privileged app (`io.github.thisisthepy.dsa.agent`), installed
by the image in `/system_ext/priv-app/DsaAgent/`. It has no activity: the host is the shell.

## What it does

- **Freeform display.** At boot it turns freeform windows on (the
  `enable_freeform_support` and `force_resizable_activities` global settings) and sets the
  default display's windowing mode to freeform through the window manager. Every task
  then has bounds of its own on the one display. The window manager takes the setting
  through an observer, so the agent sets the mode and checks again until it holds.
- **Task reports.** It sends `TaskUpdated` (task ID, bounds, package, label, focus) when an
  app task appears or any of those change, and `TaskRemoved` when the task ends. Only app
  tasks are reported: not its own, not home or recents, and not a task with no package.
- **Host requests.** `LaunchApp` starts the package's launcher activity as a new task in a
  freeform window (or brings its task to the front). `CloseTask` ends the task, but only a
  task the host was told about.
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
nothing in the guest moves or resizes a task: its bounds are set when it starts, which the
listener reports, and later only at the host's request, which goes through the agent.

## Framework interfaces outside the SDK

A platform app may use framework interfaces the SDK does not publish. The agent uses:

| Interface | For |
|---|---|
| `TaskStackListener`, `ActivityTaskManager.getService()` | Task change callbacks, ending a task (`removeTask`) |
| `TaskInfo.configuration.windowConfiguration`, `TaskInfo.isFocused` | A task's bounds, activity type and focus (reflection) |
| `IWindowManager.get/setWindowingMode` | The display's windowing mode (reflection) |
| `ActivityOptions.setLaunchWindowingMode` | A freeform window for a launched app (reflection) |
| `ServiceManager.checkService` | The control daemon's service (reflection) |
| `Binder.markVintfStability` | Marking the listener the daemon calls as a stable interface (reflection) |

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
| `src/main/kotlin/.../agent/` | The agent: task reports, requests, the channel client, the freeform setup |
| `src/main/kotlin/.../agent/platform/` | The framework side: the application, task source, actions, windowing, binder channel |
| `src/test/` | The unit tests |
| `hidden-api/` | Compile-only declarations of framework classes outside the SDK |
| `privapp-permissions-dsa-agent.xml` | The agent's privileged permissions |
