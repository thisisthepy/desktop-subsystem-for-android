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
  for a task the host was not told about. `SetTaskCaption` gives a task a top status bar and
  caption bar inset as high as the title bar its host window draws over it, through a window
  container transaction's inset sources, given again whenever the task's top edge moves.
- **Status bar tone.** Each `TaskUpdated` carries what the task's app told the system about
  its status bar (light or dark icons, and its color), read from the task description, so
  the host draws its caption buttons in a tone that stays readable over it.
- **Density and size limits in reports.** Each `TaskUpdated` also carries the density the
  task's app draws at and the display's size, which is the largest a task can be, and
  whether the task's top activity says it handles a change of density itself (its
  manifest's `configChanges` include `density`, read from the package manager's
  `ActivityInfo.configChanges`). Android relaunches any other activity whose density
  changes, so the host asks only such a task for another monitor's density and scales the
  window of any other.
- **Pointer input.** The host sends the mouse, touch and pen input of each task's window
  (`PointerEvent`, relative to the task). The agent injects it into the task at the
  place it last reported for the task (see below).
- **Key input.** The host sends the keys pressed in each task's window (`KeyEvent`). The
  agent gives the task the input focus if it does not hold it, and injects the key (see
  below).
- **Text input.** The host sends what its input method composes and commits in each task's
  window, and every character a key types (`ImeComposition`, `ImeCommit`). The agent's own
  input method, which shows nothing, puts it into the focused editor, and the agent reports
  where the editor's cursor is (`ImeCursor`) so the host's candidate list appears beside it
  (see below).
- **Screen and Doze.** The guest has no screen or battery, so Doze never starts by itself.
  When no task window has been shown on the host for a while, the host sends
  `ScreenState` off: the agent turns the display off (`PowerManager.goToSleep`) and forces
  deep idle (the `deviceidle force-idle deep` shell command). `ScreenState` on lets the
  device out (`deviceidle unforce`) and wakes it (`PowerManager.wakeUp`). The agent reports
  each change of the deep idle state (`DeviceIdle`), from which the host knows the guest
  is dozing. A new connection turns a screen left off back on, since the host takes the
  screen to be on.
- **Clipboard.** Text copied on the host arrives as `ClipboardText` and becomes the primary
  clip; text copied in the guest goes to the host the same way. The agent reads the
  clipboard as the guest's default input method, which Android 10 and later allow whichever
  app has the focus. It does not send back the clip it set, drops text longer than 256 KiB
  of UTF-8, and sends nothing while the host says sharing is off (`ClipboardSharing`).
- **Notifications.** The guest has no SystemUI, so the agent's notification listener reads
  the guest's notifications and sends each one a person would see to the host
  (`GuestNotification`): title, text, app name, a 96-pixel PNG icon, its importance, and
  whether it is high priority. It leaves out group summaries, the ongoing notices of
  foreground services, minimum-importance and empty notifications. A notification posted
  again replaces the host's, and a removed one is removed there (`NotificationRemoved`).
  A click on the host (`NotificationClicked`) sends the notification's content intent, which
  may start an activity from the background; a dismissal (`NotificationDismissed`) cancels
  it. The agent allows its own listener at start (`MANAGE_NOTIFICATION_LISTENERS`). The
  listener is called over binder in the agent's persistent process, so notifications still
  arrive while the device is in Doze.
- **Apps that run in the background.** An app that runs a foreground service (music, a
  download, a VPN) keeps running with no window. The agent reads each service's ongoing
  notification and tells the host which packages run one (`BackgroundApp`: the app's name
  and icon, the notification's title, text and action titles; one per package, from its
  latest service), and when a package runs none any more (`BackgroundAppGone`). The host
  shows each in its notification area. What the user does there comes back as
  `BackgroundAppAction`: open runs the notification's content intent, an action sends that
  action's intent, and stop force-stops the package (`ActivityManager.forceStopPackage`,
  with `FORCE_STOP_PACKAGES`), never the agent or the system. The agent also reads the
  active media sessions through `MediaSessionManager`, which a notification listener may,
  and sends each (`MediaSession`: title, artist, album, art of at most 256 pixels, playback
  state and the commands it takes) until it ends (`MediaSessionGone`); a host media key
  (`MediaControl`) goes to the session's transport controls.
- **Widgets.** The agent is the guest's widget host (`AppWidgetHost`, host ID `0x0D5A`).
  It lists the installed widgets for the host's gallery (`WidgetProviders`, in parts under
  256 KiB, with previews of at most 160 pixels and 48 KiB), and places one the host asks for
  (`AddWidget`): it allocates a widget ID, binds it without asking (`BIND_APPWIDGET`), and
  runs the widget's setup when its app has one, through `WidgetConfigureActivity`. That
  activity opens a freeform task of its own (an affinity of its own, `NEW_TASK` and
  `MULTIPLE_TASK`), never the agent's home task, which has no host window, and the setup runs
  above it; the agent reports that task as the app's, so the host shows the setup as the
  app's focused window. `RESULT_OK` places the widget, also from a setup that finishes at
  once; any other result, Back and a closed window cancel it, and a setup that cannot start
  is reported as such, not as a cancel. A widget whose app says it is reconfigurable and its
  setup optional is placed without one. A widget whose app says it is reconfigurable, and
  that has a setup, is described as such (`reconfigurable`), and the host's Widget settings
  (`ReconfigureWidget`) runs its setup again the same way for the same widget ID; the answer
  (`WidgetReconfigured`) says whether the user finished it, and a finished one describes and
  draws the widget anew. Each widget's view lives in a presentation on a private virtual
  display of the widget's size, which keeps its views running, and is drawn by the agent's
  own hardware renderer into PNG frames (`WidgetFrame`), only when it changes, at most ten
  times a second, and at most once a second while the guest dozes. That drawing does not use
  the guest's display, so it goes on while the screen is off. A clock is drawn again at each
  minute (or second) even when nothing asks. A size from the host (`ResizeWidget`) redraws
  the widget at that size and tells its app the size in dp through the widget's options
  (`OPTION_APPWIDGET_MIN`/`MAX_WIDTH`/`HEIGHT` and `OPTION_APPWIDGET_SIZES`), exactly the
  window's size: the widget's view has no padding, so the 8 dp a side that
  `AppWidgetHostView.updateAppWidgetSize` takes off for a launcher's padding is not taken. A
  widget is described with a default size in whole launcher cells, as a launcher places it:
  the cells its app asks for (`targetCellWidth`/`Height`), else the fewest that hold its
  minimum (n cells hold 70n - 30 dp), each 74 dp with 16 dp between them, at most 5. When a
  widget's views still reach past its size on any side, as when its text is larger than its
  app planned for (Android centres a widget's content, so it overflows at the top as much as
  at the bottom), the agent never sends an image that cuts them off: it draws the widget shrunk just
  enough to show whole, and describes it again with a default that holds its content (kept
  across restarts, and only ever grown), so a window the user has not sized grows to it. Pointer input (`WidgetPointer`) goes to the
  widget's view; a click opens its app on the guest's display. The widget IDs outlive the
  agent and the guest, and the size each was drawn at is kept in the agent's preferences.
  Each connection hears of every placed widget (`WidgetUpdated`, then `WidgetList`).
  Android hands a widget's updates only to a view its host made for that widget ID, so on
  every start the agent makes a view for each placed widget before anything else about its
  drawing, and tries again a second later when that fails. When the widget's window cannot
  open, the widget is drawn without one, in software, each time its app sends new views.
- **APKs from the host.** `InstallApk` opens a `PackageInstaller` session, each `ApkChunk`
  is written into it in order, and the session is committed once the APK is whole. After
  each chunk the agent reports how far it got (`InstallProgress`), which paces the host, and
  it answers with `InstallResult`: a stable reason (incompatible, conflicting signature,
  invalid APK, insufficient storage, downgrade, blocked, other) and the installer's own
  message. `INSTALL_PACKAGES` lets the session commit without asking the user; a
  `CancelInstall`, a broken transfer or a new connection abandons the session. An installed
  app reaches the host's Start menu through the launcher apps report. When `InstallApk` names
  an ABI (`x86_64` or `arm64-v8a`), the session takes it as its ABI override
  (`SessionParams.abiOverride`, a field the SDK hides that the platform-signed agent may set):
  the host asks for `arm64-v8a` only when the APK's x86_64 libraries leave out some of its ARM
  ones and a translator is in place.
- **Apps of several APKs.** When `InstallApk` lists `parts` (the base APK and the splits of a
  split-APK bundle the host chose for this device, then any expansion files), their bytes
  follow one another in the chunks. The agent writes every APK into the same session under
  its own name, and keeps each expansion (OBB) file aside in its cache. Once the installer
  has installed the app, the agent copies the expansion files into the app's folder under
  `Android/obb/<package>/` and says the app is installed; when they cannot go there, it says
  so with the reason. A failed or cancelled install drops them.
- **Dragging out of a window.** A one-pixel window of the agent's, above every app and
  taking no input, hears of every drag an app starts with `DRAG_FLAG_GLOBAL` (the window
  manager's private flag for intercepting global drags). When the pointer leaves a task's
  host window with a button held, the host sends `DragOut`. The window then covers the
  display, the agent lifts the pointer, and the drag drops on it. The agent takes the drop's
  read access for its own process, answers `DragOffer` with each item's name, size and type
  (or its text), and streams each file from the app's provider in answer to `DragRead`.
  Content URIs and paths stay in the guest. `DragEnd` closes the streams and gives the
  access back, whether the drag was dropped or cancelled on the host.
- **Share on PC.** "Share on PC" in Android's share sheet (`ShareActivity`, for `ACTION_SEND`
  and `ACTION_SEND_MULTIPLE` of any type) sends the host `ShareToHost` with the shared text,
  links and files. The host reads the files with `DragRead` under the share's ID, as a drag's,
  ends the share with `DragEnd`, and shows its own share UI (Windows' share window with Nearby
  sharing, the macOS share menu, the desktop portal on Linux). The activity shows nothing and
  stays, in a task of its own the host shows no window for, until the host has the files,
  since the app's read access lasts only while it does. A `ShareResult` that failed is shown
  to the user as a toast.
- **Network location and geocoding.** The image names the agent as the guest's network
  location provider, with or without Google Play, and as its geocode provider without Google
  Play; on a boot with the Google apps image an overlay names Google's geocoder instead. The
  network provider (`HostNetworkLocationService`) asks the host for its location with
  `LocationRequest` while Android wants network locations, and reports each `LocationFix` as a
  `network` location; a `LocationStatus` that is not allowed disables the provider. The control
  daemon tells the host what it and the GNSS HAL want together. The geocode provider
  (`HostGeocodeService`) sends each lookup as `GeocodeRequest` and turns the `GeocodeResult`
  into Android `Address`es, or into the `IOException` Android's geocoder reports when the
  service is not available. The host answers a position from administrative boundary data it
  keeps (country, province, city, district and neighbourhood, with no street or house number)
  and an address with no positions; nothing goes to a server.
- **The host's root certificates.** The control daemon adds the root certificates a user or
  administrator added to the host to Android's system certificates, and takes away those the
  host no longer names, before the agent hears of it (`HostCertificates`). An app that starts
  afterwards reads them as it reads Android's own, but a running process keeps every trust
  anchor it has looked up, so the agent (`network/HostCertificates.kt`) sends
  `KeyChain.ACTION_TRUST_STORE_CHANGED`. The activity manager then has every process reset its
  network security config and TrustManager, and a root the host took away stops being trusted
  at once. It is a protected broadcast, which the agent may send as a persistent app.
- **Host sleep.** When the host wakes from sleep it sends `HostSleep` with its wall clock, and
  the agent sets the guest's time to it with `AlarmManager.setTime` (the privileged
  `SET_TIME` permission), since the guest's clock stood still or drifted while the host slept.
  Before the host sleeps, the control daemon writes the guest's data to disk and answers.
- **Battery.** The host sends its power (`PowerState`): AC or battery, the charge, whether
  it charges, battery saver. The agent sets the battery service to it (the `battery set`
  shell command) and battery saver with it (`PowerManager.setPowerSaveModeEnabled`). A host
  without a battery is a full battery on AC power (`dumpsys battery`: `AC powered: true`,
  `present: true`, `status: 5`, `level: 100`), so jobs that wait for charging, such as
  background dexopt, run. Both shell commands go over binder to
  the service with the agent's `DEVICE_POWER` permission, which they check; they need no
  shell or debuggable build. The image's `sepolicy/agent.cil` lets system_server use the
  descriptors the call hands it.
- **Speech and spelling.** The agent is the guest's text-to-speech engine
  (`HostTextToSpeechService`), speech recognition service (`HostRecognitionService`) and spell
  checker (`HostSpellCheckerService`), and makes them the defaults at boot
  (`tts_default_synth`, `voice_recognition_service`, `selected_spell_checker`,
  `spell_checker_enabled`) unless the user picked another one that is still installed. None
  keeps an engine or a dictionary in the guest: the engine lists the host's voices
  (`ListSpeechVoices`) and speaks with them (`SpeakText`, the sound back as `SpeechAudio`
  pieces and `SpeechDone`), the recognizer asks the host to listen (`StartRecognition`, then
  `RecognitionEvent`s with partial and final results), and the spell checker sends each
  sentence an editor asks about (`CheckSpelling`, answered by `SpellingResult`). An app
  without the permission to record gets the recognizer's permission error before the host
  is asked; the host's own microphone privacy settings apply there. A host that cannot
  answer fails the request at once, so no app waits.
- **Sound volume.** The host's volume mixer is the only place the guest's sound is turned
  down: the audio module sends every output at full scale (see `dsa/guest/audio`). The agent
  registers an audio policy (`MODIFY_AUDIO_ROUTING`) and gives each app with a window an app
  output of its own, by attaching a mix whose rule matches the app's uid and which plays on
  that output's bus device (`dsa_app_0` to `dsa_app_7`). It tells the host which app each
  output plays and the app's volume (`AppAudio`, the whole list, on each connection and each
  change), and the host plays the output as that app in its mixer. An app's volume is what
  Android's media volume says while the app is the one it stands for: the app whose window
  has the focus, or the last one that had it. An app that sets its volume and the volume keys
  change it; when the focus moves, the media volume becomes the focused app's, so each app
  reads its own. A volume or mute the user sets in the host's mixer (`SetAppVolume`) becomes
  the app's, and the media volume while the app has the focus. Each app's volume is kept in
  the agent's preferences, and a new app starts at full volume. An app keeps its output after
  its windows close, so music it plays on keeps its name in the mixer; when all are taken, the
  app that had a window longest ago and plays nothing gives its output up. An app without one
  plays on the shared output, DSA's line in the mixer, which has a volume of its own.
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

The agent also registers a task organizer, but only to draw starting windows (see
"Splash screens"); task reports do not come from it. With no SystemUI and no window
decorations in the guest, only the agent moves or resizes a task: its bounds are set when it
starts, which the listener reports, and later only by the agent, which reads the tasks again
until the change lands (below).

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
the window's monitor (130 dpi at 100%, 262 at 200%: Android's desktop dp is 48 dp for
10.4 mm, 1/117.23 inch, and a host pixel at 100% 1/96 inch, so the density is 131 times the
monitor's scale, rounded to an even number). Android starts at the density of the user's main monitor, so a window that
opens there asks for nothing. A task whose top activity does not say it handles a change of
density is never asked for another one: its window shows it scaled instead, and its client
size is the task's size at that scale.

Text follows the same ratio as everything else, so body text of 16 sp is about 13 host
pixels at 100%, near the host's own body text. Android scales text by its font scale, apart
from everything else, and the host's `SystemSettings` carries a font scale of the host's text
size setting alone (1 at 100%, 1.5 at 150%), which the agent writes to
`Settings.System.FONT_SCALE`. A change of
font scale makes Android restart every activity that does not say it handles one (its
`configChanges` include `fontScale`), so while an app task is open whose top or root activity
does not, the agent holds the new scale, logs which apps it waits for, and applies it as soon
as none is open. The first scale after the agent connects applies before any app opens.

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

## Installed apps

The host's start menu lists the apps a launcher would list: those with an activity in the
launcher category. The agent sends each one's name and launcher icon (`InstalledApp`), then
every package in one list (`InstalledApps`). Each app goes in a frame of its own, so no frame
carries every icon. It does so when the channel connects, and again whenever the package
manager says a package was added, replaced, changed or removed, the language changed, or the
user's storage was unlocked (before that, only the apps that run that early are listed).
Only an app whose name, icon, version or size changed is described again; an app missing from
the list has been removed. The agent's own package is never listed.

Each `InstalledApp` also carries the app's version, when it was first installed and last
updated, whether it is part of the Android image, and its size (code, data and cache, from
`StorageStatsManager`). The host's `RefreshApps` makes the agent read them again and send the
list even when nothing changed, because the host waits for it.

### App shortcuts

The host shows each app's launcher shortcuts the desktop's way: the static and dynamic ones
as tasks of the app's jump list (Dock menu, desktop entry actions), and the pinned ones as
desktop shortcuts. The agent is the guest's default launcher, so `LauncherApps` lets it read
every app's shortcuts and start them without a permission of its own
(`platform/AndroidShortcuts.kt`). After each list of apps it sends `AppShortcuts` for every
app with shortcuts: the static and dynamic ones by rank, then the pinned ones, each with its
labels and its icon as a PNG of 256 pixels. It sends them again when Android says an app's
shortcuts changed (`LauncherApps.Callback.onShortcutsChanged`), and an empty list once an app
has none left. Turned-off shortcuts are left out.

`LaunchShortcut` starts a shortcut through `LauncherApps.startShortcut`, in a freeform window
like any app the host launches. A shortcut that is gone or turned off opens its app instead.

An app's `requestPinShortcut` reaches the default launcher's activity for
`CONFIRM_PIN_SHORTCUT`, which the agent declares (`platform/PinShortcutActivity.kt`), so
`isRequestPinShortcutSupported` is true for apps. The activity shows nothing and finishes at
once; it is in the agent's package, which the host never shows a window for. The agent sends
`PinShortcutRequest` with an ID, the app's name and the shortcut, and keeps Android's request.
`PinShortcutAnswer` with yes accepts it, which pins the shortcut; no lets it go. A request
with no answer lapses after five minutes, and a new connection drops the ones before it.

The host can uninstall an app (`UninstallApp`). The agent refuses an app that is part of the
image, itself, and a package that is not installed, each with its reason, and uninstalls any
other through `PackageInstaller`; `DELETE_PACKAGES` lets it go ahead without asking again in
the guest. It answers with `UninstallResult`. `OpenAppSettings` opens Settings' page of an app
(`ACTION_APPLICATION_DETAILS_SETTINGS`), or its list of apps when no package is named, as a
task in its own window.

Each app in the report also carries the ABI its native code runs as (the package manager's
primary CPU ABI) and the library names under `lib/<abi>/` of its base APK and splits
together, read from their central directories. The host's "Run as ARM" switch asks for an app
to be installed again with the other ABI (`ReinstallApp`): the agent writes the app's APKs on
the device into a new session with the ABI override, which replaces the app as an update does
and keeps its data. When Android refuses that replace in a way only an uninstall gets past
(`INSTALL_FAILED_UPDATE_INCOMPATIBLE`, `INSTALL_FAILED_SHARED_USER_INCOMPATIBLE`), nothing
changes and the answer says the data would be erased; with `erase_data` the agent copies the
APKs to its cache, uninstalls the app and installs the copies. It answers with
`ReinstallResult`, carrying the ABI Android chose.

The agent also follows the system's crash records (`ACTION_DROPBOX_ENTRY_ADDED`, with
`READ_LOGS` and `PACKAGE_USAGE_STATS`). When a user app ends because a native library did not
load (`data_app_crash` or `data_app_native_crash` naming `UnsatisfiedLinkError` or
`dlopen failed`), it sends `NativeLibraryCrash` with the package, the line that names the
library, and whether the guest's ABI list holds `arm64-v8a`; the host may then offer to run
the app as ARM. One app is reported at most once every five minutes.

## Pointer input

The host sends one `PointerEvent` per pointer as it happens, with the position relative to
the task. The agent adds the task's origin on the display, as it last reported it to the
host, and injects a `MotionEvent` with `InputManager.injectInputEvent`, without waiting
for the app (`input/PointerInput.kt`). This happens on a thread of its own, at the priority of
the framework's input threads, because the agent's other thread makes binder calls that can
take hundreds of milliseconds while the framework is busy. A move still waiting there is
replaced by a newer one of the same pointer, so a press never waits behind a queue of moves
(`input/PointerQueue.kt`). Input for a task the host was not told about is
dropped.

- The mouse is `SOURCE_MOUSE`. A press is `ACTION_DOWN`, then `ACTION_BUTTON_PRESS` for
  the button, so the right button is `BUTTON_SECONDARY`: a context click, not a long
  press. A second button while one is down is only a button press. Hovering begins with
  `ACTION_HOVER_ENTER` and ends before a press.
- The wheel is `ACTION_SCROLL` with `AXIS_VSCROLL` and `AXIS_HSCROLL`, in notches.
- Fingers are `SOURCE_TOUCHSCREEN` and a pen is `SOURCE_STYLUS` (its eraser end
  `TOOL_TYPE_ERASER`). Every finger down goes in each event of a touch: the second finger
  is `ACTION_POINTER_DOWN`, and lifting all but the last is `ACTION_POINTER_UP`.
- The fingers on every task window are one touch of the display, as on a phone's screen,
  and the framework splits it between the windows. Two touches from one device at once
  would make the second `ACTION_DOWN` cancel the first.
- Each finger takes the lowest pointer ID its touch does not use. The host's IDs can be
  any number, and the input dispatcher refuses a pointer ID above 31. A touch carries at
  most 16 fingers, as a `MotionEvent` does.
- A finger scrolls and flings by moving: it is never `ACTION_SCROLL`, which only the
  wheel sends.

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
  virtual keyboard. The host sends only keys that type no text (arrows, Enter, Backspace,
  shortcuts with Ctrl or Alt): a key that types a character comes as text instead.
- A held key keeps the time it went down; its repeats and its release are placed after it
  by the host's times. A release for a key the agent never saw go down is dropped. A
  release the host marks canceled (the window lost the focus) carries `FLAG_CANCELED`, so
  the app lets the key go without acting on it.

## Text input

The host is the shell, so text is typed with the host's input method, in the host's window,
and the guest has no keyboard app. Android puts text into an editor only through an input
method, so the agent declares one (`platform/HostInputMethod.kt`) that is no keyboard: it has
no view, refuses to be shown, and is never fullscreen, so no soft keyboard appears. At boot
the agent makes it the only enabled and the selected input method, and turns off showing a
soft keyboard beside a hardware one, through the secure settings the input method manager
watches (`platform/AndroidTextInput.kt`). Its `input-method` metadata marks it the default,
so the manager selects it on its own too.

- A composition (`ImeComposition`) becomes the focused editor's composing text, which the
  editor shows underlined, with the cursor where the host's input method has it; each one
  replaces the last, and an empty one removes it. A commit (`ImeCommit`) replaces the
  composition, which ends (`input/TextInput.kt`).
- Before text goes to a task that does not hold the input focus, the agent focuses it, as
  for a key.
- Where no editor takes text (a game, or a view that reads keys itself), committed text is
  injected as the virtual keyboard's keys for each character, so typing still works there.
  A composition there has nowhere to go and is dropped.
- The input method asks every editor it is bound to for cursor updates, and the agent tells
  the host where the cursor is in the focused task (`ImeCursor`), once for each place it
  moves to.

## Web links

While the agent is connected, web links go to the host, which opens them in the user's
default browser. The agent makes itself the guest's browser through the role manager
(`MANAGE_ROLE_HOLDERS`), so Android hands it every http and https link an app opens. Its
`LinkActivity` shows nothing: it passes the link to the agent and finishes at once, and the
agent sends it to the host as `OpenLink`. Anything but an http or https link with a host is
dropped, because the host opens links with its shell.

A link that an installed app has verified as its own (an Android App Link) still opens in that
app: Android sends verified links straight to their app and never asks the browser.

The host's `LinkHandling` says whether the user keeps links in Android instead. Then the agent
gives the browser role back to a browser app in the guest and opens any link it still gets
there. A link that cannot reach the host, because the agent is not connected, opens in the
guest too, so a tap is never lost. The image keeps its stock browser for this, and because an
app may start it by name.

## Files from the host

The host sends a file the user opens or shares from Windows as `OpenFile` and then
`FileChunk` messages of 128 KiB. The agent stores each transfer in a folder of its own under
`files/host-files/` (`<transfer>-<time>/<name>`) and answers each chunk with `FileProgress`,
which is what lets the host send the next ones. Once the file is whole it starts the system
chooser with `ACTION_VIEW` (open) or `ACTION_SEND` (share) and the MIME type of the file's
extension, so the user picks the app, which then shows as a task window. The app reads the
file through `HostFileProvider`, which is not exported and serves only with the read
permission granted on that intent.

A name with a folder in it, a file over 64 MiB, a chunk out of order or past the announced
size end the transfer with the reason in `FileProgress`, and its folder is removed. A transfer
the last connection left unfinished is dropped when the agent connects again, and the folders
of transfers older than an hour are removed whenever a new one begins.

## Accessibility

A screen reader on the host, or any other UI Automation client, reads a task window's
contents through the agent (`accessibility/AccessibilityExport.kt`).

- **Off unless asked.** The agent's accessibility service
  (`platform/HostAccessibilityService.kt`) is off while the host watches no task. Android then
  tells no app that accessibility is on, so no app builds accessibility events and nothing is
  read: a user who uses no accessibility client pays nothing for it. At boot the agent turns
  the service off, in case an agent that died left it on.
- **Turned on without the user.** The first `AccessibilityWatch` with `on` adds the service to
  the `enabled_accessibility_services` secure setting (the agent holds
  `WRITE_SECURE_SETTINGS`), keeping any other service there, and sets `accessibility_enabled`.
  The last watch turned off, or a new connection to the host, removes it again, and
  `accessibility_enabled` is cleared only when no other service is left. The agent's input
  method and home activity are other components and are not touched.
- **Only the watched task.** The service asks for every interactive window, and the agent
  takes the windows whose task is the watched one (`AccessibilityWindowInfo.getTaskId()`,
  hidden in the SDK and called by reflection, which a platform app may do). Bounds are sent
  relative to the task, which are the pixels of its host window.
- **A full tree, then changes.** Each node gets an ID that lasts for the tree's epoch. A watch
  starts a new epoch with a full tree; after that a change carries only the nodes that are new
  or changed, and the IDs of those that are gone. A change waits 100 ms for the rest of its
  burst, and two trees of a task are never less than 250 ms apart, so at most four go out a
  second, and a host client still hears of a change well within a second.
- **Bounded.** A tree holds at most 2000 nodes and about 400 KB, nearest the top first,
  because one frame reaches the daemon in a binder call of at most 1 MiB. Text is cut at 2000
  characters, and a password's characters never leave the guest, only how many there are.
- **Actions.** `AccessibilityAction` names a node by its ID and epoch and asks for a click,
  long click, new text, a scroll forward or backward, the input focus, the accessibility focus
  or a selection. The agent performs it as the node's accessibility action, which does not
  move the pointer, and answers with `AccessibilityActionResult`. An action for an earlier
  epoch is refused: its ID may name another node now.

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

## Splash screens

Since Android 12 the window manager does not draw an app's starting window (its splash
screen) itself. It asks the registered task organizer, which on a phone is SystemUI's window
manager shell, and with no organizer it draws none: the task's window stayed black until the
app drew its first frame. The guest has no SystemUI, so the agent registers the organizer
(`platform/AndroidStartingWindows.kt`).

- **Which window.** For each launch the window manager describes (a new task, a cold
  process, a task brought back), the agent picks the style the framework's shell would
  (`splash/StartingWindowPolicy.kt`): the splash screen, its background alone, the app's
  window background for an app on the pre-splash theme, or nothing, for home and for a task
  that is already drawn. Task snapshots are never drawn: such a task still has its surface.
- **What is in it.** The background is the theme's `windowSplashScreenBackground`, else its
  `windowBackground` color. The icon is `windowSplashScreenAnimatedIcon`, else the
  activity's icon, centered: an adaptive icon at 160 dp, any other splash icon at 288 dp
  (240 dp on a `windowSplashScreenIconBackgroundColor` circle), so its safe zone is the
  visible circle. `windowSplashScreenBrandingImage` sits at the bottom. An animated icon
  plays.
- **How.** The window is a `TYPE_APPLICATION_STARTING` window on the activity's token,
  added from the agent's process, as the shell does. The window manager asks for its
  removal when the app has drawn, and the agent removes it at once, without the shell's
  reveal animation. An app that asks to animate the splash screen away itself is told it
  gets no copy of it, and goes on.
- **What registering changes.** The window manager hands every task to the organizer, and
  then leaves each task's surface position and crop to it. The agent puts each task's
  surface at the task's position, and moves it when the task moves; the crop is removed,
  because it would keep the task's size at the moment it was organized
  (`splash/StartingWindows.kt`). Visibility, bounds, focus and input stay with the window
  manager. Only the last organizer registered receives starting window requests, and the
  guest has no other. If the agent's process dies, the window manager takes the tasks back
  and places their surfaces again.

All of it runs on a thread of its own (`dsa-splash`), not on the main thread of the home
activity. If the organizer cannot be registered, apps start without a splash screen and
everything else works as before.

## App data backups

The agent is the guest's backup transport (`platform/HostBackupTransport.kt`, a
`BackupTransport` its service `HostBackupTransportService` returns to the backup manager).
Android's backup manager keeps each app's data with it, and the host keeps that data in the
user's DSA Backup folder, encrypted with a key the guest never holds. A data reset replaces only
the guest's disk, so the folder survives it, and so does a move to another computer once its
recovery key is entered there.

- **Only when the user turns it on.** Backup keeps a second copy of the apps' data on the
  computer, so it is off until the user turns it on on the host (the settings app, or
  `dsa backup on`). The host says which (`BackupSetting`) each time the agent connects and
  whenever the user changes it. Until it says on, the agent keeps Android's backup manager off,
  automatic restore too, while the host's transport, AOSP's local one or none is selected, so
  nothing is backed up and nothing is scheduled; a transport of another app the user chose
  (Google's) is left to that app. The host's `BackupRun` and `BackupRestore` are refused then.
- **Which transport.** The backup manager binds a transport only of a privileged app that a
  sysconfig file lists. The image installs `sysconfig-dsa-agent.xml` in
  `system_ext/etc/sysconfig/`, and the service has the `android.backup.TRANSPORT_HOST` action. It
  is not exported: only the system binds it. While backup is on, at each change and every five
  minutes after, the agent turns backup and automatic restore on and selects its transport
  (`backup/BackupRuns.kt`, through `IBackupManager`), so a transport another app selects (Google
  Play services, if the user installs it) does not take the user's data elsewhere.
- **Backing up.** A key-value backup is read from the app's `BackupDataInput` and sent as
  records (a big-endian key length, the key, a value length, -1 for a deleted key, the value);
  an incremental one only changes what the host holds. A full backup's tar stream goes as
  Android writes it. Every piece is at most 256 KiB, and the next goes only once the host has
  answered the last (`BackupUpload`, then `BackupCommit`). When the host has no APKs of the app's
  version, it says so in its answer, and the agent sends `base.apk` and the split APKs, except of
  an app the image carries: a new disk has those already. The quotas are the host's: 64 MiB of
  key-value data and 4 GiB of full backup per app.
- **Restoring.** The host offers one restore set, its backup folder (`BackupQuery`). Android
  restores the package manager's metadata (`@pm@`) first and then each app in the order it asks,
  as key-value records written to `BackupDataOutput` or as the full backup's tar stream, which
  the agent reads from the host piece by piece as Android consumes it (`BackupFetch`). After a
  data reset the host installs the apps it holds APKs of and sends `BackupRestore`; the agent
  starts a restore session and answers `BackupRunResult` with how many apps came back. The
  host's `BackupRun` backs up every app now, as `bmgr backupnow --all` does.
- **Waiting.** The backup manager calls the transport on its own threads and expects the answer
  from the call, so each call blocks until the host answers (`channel/HostCalls.kt`), at most a
  minute or two. Without the host every call fails and the backup manager tries again later.
  The control daemon passes backup messages only from the agent.

## Passkeys

The agent is a credential provider (`platform/HostCredentialProvider.kt`) offering passkeys
only: "This computer" in Android's credential selector. Picking it opens `PasskeyActivity`,
which shows nothing, sends the request to the host (`passkeys/HostPasskeys.kt`, `PasskeyRequest`)
and returns the result: a `CreateCredentialResponse` with the registration JSON, a
`GetCredentialResponse` with the authentication JSON, or the exception type an app built with
androidx.credentials expects (canceled, `NotAllowedError`, no credential, unknown). The host
checks the app against the relying party's Digital Asset Links and runs its own
authenticator's prompt (Windows Hello, macOS passkeys). Its answer can take minutes, so the
request waits on a thread of its own.

The trust boundary is the caller's identity, because the host decides from it which relying
party a request may name. It comes only from the credential manager's `CallingAppInfo`, never
from the app: its package, the SHA-256 of each certificate it is signed with now (every signer
of an app with several, otherwise the newest of its signing history), and an origin only as the
credential manager reports it. Any app may hold `CREDENTIAL_MANAGER_SET_ORIGIN` (a normal
permission) and set an origin, so an origin, and a client data hash with it, is passed on only
for a browser on the list of privileged browsers credential providers use
(`passkeys/PrivilegedBrowsers.kt`, by package and certificate) or a browser the image carries.
From any other app the request is refused before it reaches the host. An origin written into the
request JSON is never read.

The provider is a system provider: the agent holds `PROVIDE_DEFAULT_ENABLED_CREDENTIAL_SERVICE`
and its service has the system provider action, so the credential manager lists it whatever
other providers write. The agent also keeps it in the `credential_service` setting, and makes it
the primary provider only while none is (`AndroidCredentialSetup`), so a provider the user
chose stays first.

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
| `ActivityManager.forceStopPackage` | Stopping an app the user stops on the host (reflection) |
| `Binder.markVintfStability` | Marking the listener the daemon calls as a stable interface (reflection) |
| `BackupTransport`, `RestoreSet`, `RestoreDescription`, `BackupDataInput(FileDescriptor)`, `BackupDataOutput(FileDescriptor)` | The backup transport (reflection for the two constructors) |
| `IBackupManager`, `IRestoreSession`, `IRestoreObserver`, `IBackupObserver`, `ServiceManager.getService` | Selecting the transport, backing up every app and restoring |
| `InputManager.injectInputEvent`, `MotionEvent.setActionButton`, `InputEvent.setDisplayId` | Injecting the host's pointer and key input (reflection) |
| `IActivityTaskManager.setFocusedTask` | Giving a task the input focus before its keys |
| `TaskOrganizer`, `StartingWindowInfo`, `TaskInfo.positionInParent`, `IActivityTaskManager.onSplashScreenViewCopyFinished` | Drawing starting windows and placing organized tasks' surfaces (reflection for the fields and the copy answer) |

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
The runtime permissions are the exception: `default-permissions-dsa-agent.xml`, which the
image installs in `system_ext/etc/default-permissions/`, grants them from the first boot.
The storage permissions are there so that Android lets the agent write the folders under
`Android/obb/`.

## Building and testing

Needs a JDK (17 or later) and the Android SDK (`ANDROID_HOME`, or `sdk.dir` in
`local.properties`). From this folder:

```
./kotlin test
./kotlin do assembleRelease
```

The project uses the Kotlin Toolchain (`project.yaml`, `module.yaml`) for dependency
resolution, Kotlin/Java compilation and JVM unit testing, and a local build plugin
(`build-plugin/`) that invokes the Android SDK tools (`aidl`, `aapt2`, `d8`, `zipalign`)
directly to generate binder stubs and package the APK without Gradle.

The unit tests run on the JVM against fakes of the framework, the channel and the
scheduler, and check the frames with the generated codec. Everything that touches the
framework sits behind a small interface in `platform/`.

The release APK, `build/outputs/apk/release/dsa-agent-release-unsigned.apk`, is unsigned.
`make-image.py --agent` adds it to the image and signs it with the platform key.

| Path | What it is |
|---|---|
| `src/main/kotlin/.../agent/` | The agent: task reports, requests, pointer, key and text input, the channel client, the freeform setup, task placement |
| `src/main/kotlin/.../agent/power/` | The screen, Doze and the battery, as the host asks |
| `src/main/kotlin/.../agent/clipboard/` | The clipboard shared with the host |
| `src/main/kotlin/.../agent/notifications/` | The notifications shown on the host |
| `src/main/kotlin/.../agent/background/` | The apps that run in the background, their media sessions, and stopping them |
| `src/main/kotlin/.../agent/widgets/` | The widgets placed on the host's desktop |
| `src/main/kotlin/.../agent/install/` | Installing the APKs the host sends |
| `src/main/kotlin/.../agent/backup/` | App data backups kept by the host, and restoring them |
| `src/main/kotlin/.../agent/passkeys/` | Passkeys made and used by the host's authenticator |
| `src/main/kotlin/.../agent/platform/` | The framework side: the application, task source, actions, windowing, input injection, the input method, binder channel, power controls |
| `src/test/` | The unit tests |
| `hidden-api/` | Compile-only declarations of framework classes outside the SDK |
| `privapp-permissions-dsa-agent.xml` | The agent's privileged permissions |
| `default-permissions-dsa-agent.xml` | The runtime permissions the image grants the agent |
| `sysconfig-dsa-agent.xml` | The sysconfig that lets the backup manager use the agent's transport |
