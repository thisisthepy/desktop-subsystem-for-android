# host/platform/windows

The Windows host: the Android VM, its control channel over Hyper-V sockets,
`dsa-hostd`, which runs both, `dsa-wm`, the DSA Window Manager, and `dsa-app-window`, the
program each Android app's windows belong to.

## dsa-hostd

```
cargo build -p dsa-host-windows --bin dsa-hostd --release
dsa-hostd dsa-hostd.example.json
```

It creates the Android VM through the Host Compute System, waits for the guest's channel
daemon to connect on vsock port `control_port` and pass the protocol handshake, and prints
every state change. Type `start`, `stop`, `status` or `quit`; end of input quits, and
quitting stops the VM first.

- It needs administrator rights, because only administrators may create VMs through
  HCS. Run it from an elevated prompt. In the installed product the DSA service does this.
- It needs the Windows feature "Virtual Machine Platform". Without it, starting reports
  that the Host Compute System is unavailable and names the feature.
- `kernel` is the guest kernel from `dsa/kernel/` ("Kernel test" uploads it), and
  `system_image` and `data_disk` are the Android images, which are not built yet.

The settings are described in `src/host_config.rs`; unknown keys are an error. When
`user_sid` is absent, the user running `dsa-hostd` may use the VM's Hyper-V sockets.

### Serving users

```
dsa-hostd serve dsa-hostd.example.json
dsa-hostd service C:\ProgramData\DSA\dsa-hostd.json
```

`serve` answers the local API on the named pipe `\\.\pipe\<pipe_name>` (default
`dsa-host`) in the foreground and stops on Ctrl+C. `service` does the same under the
Service Control Manager; the installer registers it, and it writes its messages to
`log_file`.

- Each caller is identified by the SID Windows reports for the pipe connection, never by
  anything the caller sends. The first request of a user creates their data disk at
  `%LOCALAPPDATA%\DSA\android.vhdx` in their own profile and their VM `<vm_name>-<SID>`;
  later requests reach the same VM. `data_dir` (`<data_dir>\<SID>\data.vhdx`) is for tests
  and development only; an installation never sets it.
- A user's profile admits only them, SYSTEM and Administrators, and a `data_dir` folder
  only SYSTEM, Administrators and Hyper-V's virtual machines, so other users cannot read
  someone's Android data.

### Where files go

Everything DSA makes at run time is in two folders, and `src/paths.rs` is the one place
that builds their paths. The settings app and the installer follow the same layout.

| Machine (`%ProgramData%\DSA`, or the data folder chosen at install) | |
|---|---|
| `dsa-hostd.json` | the service's configuration |
| `images\` | the Android image store |
| `updates\` | downloaded setup files and the update state |
| `settings\` | the settings users change in DSA Settings, and widget placements |
| `logs\` | `dsa-hostd.log` and each user's Android console, `android-<SID>.log` |
| `tmp\` | each boot's initrd while the VM starts; SYSTEM and Administrators only |

| Each user (`%LOCALAPPDATA%\DSA`) | |
|---|---|
| `android.vhdx`, `android.image-version` | the user's Android disk and the image it holds |
| `apps\<package>\` | each app's window program |
| `notifications\` | the icons notifications show |
| `logs\` | `dsa-wm.log`, `dsa-app-window.log` |
| `compositor.json` | the render GPU chosen before DSA Settings kept it |
| `monitors.txt` | the user's monitors, for sizing the guest display |
| `start-menu.json` | the Start menu shortcuts the window manager made |
| `tmp\` | temporary files of the user's processes, when one cannot be avoided |

Outside them DSA writes only the Start menu shortcuts, its key `HKLM\SOFTWARE\thisisthepy\DSA`,
the `Run` value that starts `dsa-wm`, the `.apk` association and context menu entries, the
Hyper-V socket service registrations, and in each user's classes the notification protocol
and each app's AppUserModelID.

- No program uses the system's or the user's temporary folder. The service clears what
  earlier runs left in the machine's `tmp` when it starts.
- A disk the first releases kept in `%ProgramData%\DSA\users\<SID>\data.vhdx` moves into the
  user's profile when their Android is first set up. A disk already there is never replaced:
  the old one then stays, and the service log says so, as it does when the move fails.
- Uninstalling removes everything above, for every user, signed in or not, except each
  user's Android disk (`dsa-hostd remove-machine-data` and `dsa-hostd remove-user-data`).
- The pipe admits SYSTEM and interactively signed-in users unless `pipe_sddl` says
  otherwise. Remote clients are always refused.
- Stopping the service stops every user's VM first.

### Updating the Android image

With `images_dir`, `first_image` and `update` in the configuration, the service keeps
the Android image up to date:

```json
"images_dir": "C:\\ProgramData\\DSA\\images",
"first_image": {"version": "16.0.1", "android_major": 16, "build": 1},
"update": {
  "list_url": "https://.../images.json",
  "trusted_keys": ["<public key from dsa-release keygen>"]
}
```

- A minute after it starts and then every `check_interval_hours` (default 24), it reads
  the release list and its signature (`images.json.sig`). A list not signed by one of
  `trusted_keys` changes nothing.
- It downloads the newest image the rules allow: a supported Android version, never a
  lower major version, never one that failed here before. Every file is checked against
  the list's SHA-256 before the image becomes current.
- The next start of a user's Android boots the new image on trial. If it has not finished
  booting within `boot_timeout_secs` (default 300), the service stops it, goes back to the
  previous image and starts that.
- Users see what happened in `dsa status`: an update waiting for the next start, an update
  applied, or one undone and why.


### Updating DSA itself

With `program_list_url` and `program_dir` in `update`, the service also keeps DSA itself
up to date, from a second signed list that the same `trusted_keys` sign:

```json
"update": {
  "list_url": "https://.../images.json",
  "program_list_url": "https://.../program.json",
  "program_dir": "C:\\ProgramData\\DSA\\updates",
  "trusted_keys": ["<public key from dsa-release keygen>"]
}
```

- A minute after it starts and then every `check_interval_hours`, it reads the program
  list and its signature (`program.json.sig`). The list names the newest version and its
  `setup.exe`, with the file's size and SHA-256 (`dsa-release program` writes it).
- Only a newer version is installed; versions compare as numbers (0.1.10 is newer than
  0.1.9). The setup file is downloaded into `program_dir`, a folder only SYSTEM and
  Administrators can write to, and checked against the list when it arrives and again
  just before it runs.
- Setup runs once no user's Android is running, and no Android starts while it runs.
  `dsa update` runs it at once and stops every user's Android first, since the service
  restarts. Setup runs quietly (`/quiet /norestart`) as a process apart from the service,
  because it stops and replaces the service, and logs to `setup-<version>.log` beside
  the service's log.
- Users see what happened in `dsa status`: a version waiting to be installed, an
  installation running, and after the restart that DSA was updated, or that updating
  failed (with setup's log) and the old version is still installed. A version that failed
  is not installed again on its own; `dsa update` tries it again.
- `"program": false` in `update` turns it off. The program list is then never fetched.
  A path instead of a URL reads the lists from a folder, such as a mirror on a network
  drive.

## dsa-wm

`dsa-wm.exe`, the DSA Window Manager, runs in each signed-in user's session; the
installer starts it at sign-in. One copy runs per session, and it ends at sign-out. It
writes its messages to `%LOCALAPPDATA%\DSA\logs\dsa-wm.log`. Earlier releases called it
`dsa-compositor`.

Android cannot finish booting without the renderer in `dsa-wm.exe`, so the service makes sure it
runs. When a user's Android starts, and about once a second while it runs, the service looks
for the window manager's pipe in the user's sessions. When it is not there the service starts
`dsa-wm.exe` in the user's session, as the user, with the limited token Windows gives it for that
session (`WTSQueryUserToken` and `CreateProcessAsUserW`, which SYSTEM may call). A window manager
that keeps exiting is retried more and more slowly. Without a session to start it in (the user
signed out, or only reached the computer over the network), a stopped Android waits without
starting its VM, so it cannot crash-loop, and `dsa status` says why; a running Android is stopped
after two minutes. `"keep_window_manager": false` turns this off.

- It carries the text clipboard between the user and their Android, since the service runs
  in session 0 and cannot reach it. A message-only window hears each change to the user's
  clipboard and passes new text to the service, and the text Android copies goes on the
  user's clipboard. Text it set itself is not passed back. `dsa settings set clipboard off`,
  or the switch in the settings app, stops the sharing in both directions.
- It shows the notifications of the user's Android as toasts, each under the AppUserModelID
  of the app that posted it, so it carries the app's name and icon and groups with its
  taskbar button. A new version of a notification replaces its toast, and a notification
  removed in Android removes its toast. Clicking a toast launches the
  `dsa-notification:` protocol, which the window manager registers for the user to run
  `dsa-wm.exe --notification <uri>`: that passes the click to the service and ends, and the
  app opens in Android, which leaves Doze first. Dismissing a toast dismisses the
  notification in Android too, unless the app keeps it.
- It shows the widgets the user placed on the desktop, each in a borderless layered window
  of its own. The window is a tool window owned by the shell's desktop window (`Progman`),
  so it is neither in the taskbar nor in Alt+Tab, stays when the user shows the desktop, and
  is kept below every other window; it never takes the focus. It shows the widget's PNG
  frames, decoded with WIC, with what the widget leaves clear see-through. Dragging the band
  along its top moves it and dragging its edges resizes it; when the user lets go, the
  service remembers the placement (per user, beside the user's settings, as
  `users/<user>.widgets.json`) and tells Android the window's size, which the widget draws
  at. Presses, moves and releases inside the widget go to Android. The widgets come from the
  settings app's Widgets page or `dsa widgets list|add <provider>|remove <id>`. When the
  window manager starts at sign-in and the service remembers widgets, it starts Android so
  they come back.
- It owns no window. Each Android app task shows in a window of the app's own window
  program (below). The window manager watches the user's tasks through the service's
  local API, starts each app's window program, and tells it to open a window for each
  task, titled by the task's label and sized by the task's size in Android, and to close
  it when the task ends. Closing a window asks Android to end its task.
- It accepts the guest's graphics streams on Hyper-V sockets, ports 5566 and 5567, and
  hands them to `gfxstream_backend.dll`, which is installed beside it. The renderer
  copies each task's frames on the GPU into a Direct3D 11 texture the window manager
  shares with the app's window program, which shows it through DirectComposition; no
  frame passes through the CPU.
- Frames reach a window only for a display that Android named as a task's. Frames for any
  other display, such as Android's whole screen, are dropped, so no window shows more
  than one app.
- One Android pixel is one pixel of the window, so frames are shown 1:1 and stay sharp
  at any monitor scale. Each task draws at the density of its window's monitor: 160 dpi at
  100%, 240 at 150%, 320 at 200%. A window that opens on, or moves to, a monitor of
  another scale asks Android for that density, and then for the size that keeps the app
  the same size to the eye. Density is set per task, so windows on monitors of different
  scales are each sharp.
- Resizing a window asks Android to give its task the window's new client size. During a
  live resize only the latest size is sent, not every size the edge passed through. The
  window shows its last frame stretched over the new size until a frame of that size
  arrives, then shows that frame 1:1. A window cannot grow past what Android's display
  can hold, because a larger window could only show its frames stretched. That display is
  chosen when Android starts: a square as long as the longest monitor, from 1920 to 3840
  pixels on a side, so a window fills any monitor in either orientation. The service cannot
  see the user's monitors from session 0, so the window manager and the app windows record
  them in `%LOCALAPPDATA%\DSA\monitors.txt`. A monitor longer than the display, or one
  attached after Android started, caps its windows at the display's size, and a maximised
  window is centred on the screen, until the next start of Android. If Android gives the
  task another size than the window asked for, the window takes the task's size within 2
  seconds.
- Every size is first given to the task live: the app lays itself out again in the new
  size without being recreated, which takes a frame or two rather than the half second or
  more that recreating an app can take. The task is then reconfigured for its new size
  once, at a size it already has: when the user lets go of the window's edge, or, for a
  maximise, a restore or a snap, as soon as Android has given the task the size.
- The app window program reports each window's client size, its monitor's scale and the
  start and end of a drag to the window manager over the session's pipe. The window
  manager turns them into the task's size and density and sends those to the service.
  It tells the window the task's size, which pointer input is measured in, and the largest
  size the window may have.
- Each window measures how long it waits for a frame of its new size after a resize, and
  the log gets a summary (median, 95th percentile, longest, and how many took 100 ms or
  more) every 20 resizes and when the window closes. A window still waiting after 2
  seconds is logged as showing a stretched frame.
- Mouse, touch and pen input in a window goes to its task. A point in the window becomes
  the same point of the task in Android's pixels, whatever the window's size and the
  monitor's scale, and a point inside the window always lands inside the task. The right
  button is Android's secondary button (a context click, not a long press), the wheel
  scrolls, a pen brings its pressure, eraser and barrel button, and each finger of a touch
  is its own pointer. The window holds the mouse while a button is down, so a drag that
  leaves it still ends in the task. Input goes to the service on a connection of its own,
  which answers none of it, so a fast pointer is never held back.
- The keys pressed in the window that has the keyboard focus go to its task on the same
  connection, as Android key codes with their repeats and with Shift, Ctrl, Alt and the
  Windows key (each with its side) and the locks: the keys that type nothing or a control
  character (arrows, Enter, Backspace, Delete, Tab, Esc, function keys, Ctrl and Alt
  shortcuts). Alt+F4 still closes the window. When the window loses the focus, every key
  still held is released in Android, so none stays down there.
- Text goes to the task as text, on the same connection: what the Windows input method
  composes (the composing text with its cursor, which the Android editor shows underlined)
  and commits, and every character a key types without it, so any keyboard layout types what
  its keys show. A key that types a character is never also sent as a key. The window takes
  the input method through IMM32, which the text services framework serves every input
  method to, the Microsoft Korean IME included. It hides the input method's own composition
  window, since the Android editor shows the composition, and places the candidate list
  below the cursor of the focused Android editor, which the agent reports.
- Without a GPU device, the windows still open and close with their tasks but stay
  empty, and the log says why.

### Each app's window program

Task Manager and the taskbar group windows by program, and take the program's name and
icon from its file. So each Android app gets its own copy of `dsa-app-window.exe`, which is
installed beside the window manager:

- The copy is `%LOCALAPPDATA%\DSA\apps\<package>.exe`, for example
  `com.android.settings.exe`. Its version information names the app and its icon is the
  app's launcher icon, both as Android gives them, so Settings shows as Settings, with its
  own icon and its own taskbar button. `<package>.json` beside it records what it was made
  from; a copy is made again when DSA is updated or the app's name or icon changes, once
  its windows are closed.
- The window manager starts it when the app's first task appears. It connects to the
  window manager over the pipe `\\.\pipe\dsa-wm-<session>`, which only the signed-in user
  may open, and checks the window manager runs as the same user before it says anything.
  It ends when the app's last window closes. Ending it, from Task Manager say, closes the
  app's windows and Android ends their tasks.
- The window manager places each task's frame texture in the program's process by its NT
  handle. For each frame it tells the program, which copies the frame into the window's
  swapchain on the GPU and says when it is done; the renderer writes the texture again only
  then.
- Started by the user rather than the window manager (from the app's Start menu shortcut,
  a pinned taskbar button or Windows search), the program first asks the service to start
  Android when it is stopped and waits until it runs, showing a small window under the
  app's name and icon meanwhile, so the taskbar shows the app at once. It starts the window
  manager when its pipe is not there, from the path the installer's sign-in entry gives.
  Then it becomes the app's window host and asks Android to start the app; when the app
  has a window host already, that one gets the app and this program ends, so an app never
  has two. If Android does not start, a message box says why. It writes its messages to
  `%LOCALAPPDATA%\DSA\logs\dsa-app-window.log`.
- Every copy takes the app's own AppUserModelID, `DSA.<package>`, the one its Start menu
  shortcut carries, so the taskbar groups each app's windows on a button of its own with
  the app's icon, and pinning that button pins the shortcut.
- Uninstalling DSA leaves the copies in the user's profile, like the user's Android disk,
  but removes the Start menu shortcuts that open them (see below), in every profile.
- The renderer runs on the GPU that drives the primary monitor, so frames never cross
  from one GPU to another and a laptop's discrete GPU stays asleep. The renderer uses one
  GPU, so a window on a monitor that another GPU drives is still rendered there, and
  Windows copies its frames across. When no Vulkan GPU matches (in a remote desktop
  session, for example), the renderer picks one itself, preferring a discrete GPU.

### The Start menu

Each app that a launcher lists in the user's Android gets a shortcut in the user's Start
menu, so Windows search finds it by name and opens it, even while Android is stopped.

- The agent tells the host which apps it has when its channel connects and whenever one is
  installed, updated or removed. The service keeps the list per user, also while Android is
  stopped, and the window manager makes each app's window program and its shortcut from it.
- The shortcut is `%APPDATA%\Microsoft\Windows\Start Menu\Programs\<Label>.lnk`, named and
  drawn as the app. It runs the app's window program with `--package <package>` and carries
  the app's AppUserModelID.
- The shortcuts go straight into `Programs`, not a folder of their own. Windows search finds
  both the same way, but the list of all apps would show the apps inside one folder, apart
  from every other program; directly in `Programs` each app is listed under its own name.
- A name another program's shortcut already has gets the package after it
  (`Clock (com.example.clock).lnk`). The window manager records the shortcuts it made in
  `%LOCALAPPDATA%\DSA\start-menu.json` and only ever changes or removes those, so a shortcut
  it did not make is never touched.
- When an app is removed from Android, its shortcut goes, and so does its window program
  once it no longer runs.
- Uninstalling DSA removes the shortcuts the note names in each profile's default Start
  menu folder (`dsa-hostd remove-user-data`). A Start menu moved elsewhere by folder
  redirection keeps its shortcuts, because the uninstaller cannot read another user's
  redirection.

### Choosing the render GPU

The render GPU is chosen in DSA Settings (Display & GPU) or with
`dsa settings set render-gpu <value>`. The service keeps it for each user, and `dsa-wm`
asks the service for it when it starts.

`%LOCALAPPDATA%\DSA\compositor.json`, where the choice was made before, still counts: the
service starts a user who has never saved settings from it, and `dsa-wm` reads it when it
cannot reach the service. It is optional; without it every setting has its default.

```json
{ "render_gpu": "discrete" }
```

The value is one of:

- `display` (the default): the GPU that drives the primary monitor;
- `integrated`: the first integrated GPU;
- `discrete`: the first discrete GPU, for example for games;
- any other text: the first GPU whose name contains it, ignoring case, such as `"RTX"`
  or `"Arc"`.

The renderer chooses its GPU when it starts, so a change takes effect the next time
`dsa-wm` starts (sign out and back in, or restart it from DSA Settings). A setting that cannot be read is logged
and the default is used. Unknown keys are an error, so a misspelt setting is reported.

The log says what was asked for and what the renderer chose:

```text
render GPU setting display: asking the renderer for Intel(R) Arc(TM) Graphics (LUID 00000000:00019bec), which drives the primary monitor; task windows on other monitors are rendered there too
rendering on Intel(R) Arc(TM) Graphics (LUID 00000000:00019bec), the GPU asked for
```

When nothing matches, the second line says that the renderer ranked the GPUs itself, and
why.

`dsa launch <package>` starts an app in a window of its own, and `dsa tasks` lists the
tasks that have one.

### Web links

A web link an app in Android opens goes to the user's default browser in Windows. The service
receives it from Android and hands it to the window manager, which watches the user's links
over the local API (`WatchLinks`) and opens each with `ShellExecuteW` and the `open` verb. It
opens only http and https links it has checked itself (a scheme of `http` or `https`, a host,
no spaces or control characters, at most 32000 characters) and logs and drops anything else,
so an app in Android can never make Windows run a program. More than five links in ten
seconds are dropped as well.

A link that an installed app has verified as its own (an Android App Link) opens in that app.
The user can keep every link in Android instead: `dsa settings set links-in-android on`, or
"Open web links in Android apps" in DSA Settings. The change reaches a running Android at once.

### Opening Windows files in Android apps

`dsa open <file>` and `dsa share <file>` send a file to the user's Android, which asks which app
opens it (or takes it, to share); that app then shows in a window of its own. The installer
also adds "Open in an Android app" and "Share with an Android app" to the context menu of every
file in Explorer (on Windows 11, under "Show more options"). They run the installed
`dsa-app-window.exe` with `--open-file` or `--share-file`, which, like opening an app from the
Start menu, starts the window manager and Android first when they are not running, and shows
a message when the file cannot be sent. The entries are the machine's
(`HKLM\SOFTWARE\Classes\*\shell`), so uninstalling removes them for every user; entries in each
user's profile would outlive DSA there.

The file goes to the service over the local API in 1 MiB pieces, and from the service to
Android in 128 KiB chunks, at most two ahead of what Android says it has stored, because each
chunk reaches the agent in a one-way binder call, and those share half of a 1 MiB buffer.
Files up to 64 MiB can be sent: the service holds the whole file in memory while it sends it,
and the limit keeps that bounded. A larger file is refused with a message before anything is
sent.

## Installing APKs

An APK file installs in Android three ways, which all end in the same local API request:

- Opening it in Explorer. The installer associates `.apk` files with `dsa-app-window.exe
  --install-apk "%1"`, which starts the window manager and Android when they are not running,
  installs the APK, and says how it went in a message box.
- `dsa install <file.apk>`, which prints the result and exits with code 1 when Android refused
  the APK. Its line then starts with `not installed (<reason>):`.
- Dropping APK files on the settings app's window. The bar at the top of the window says how
  each install went, with the reason in the user's language.

The APK streams to the service and on to the agent in chunks, without being held whole in
memory, and the agent installs it through a `PackageInstaller` session. APKs up to 2 GiB are
taken. One install runs at a time; a second waits for the first. A refusal carries a stable
reason (`incompatible`, `conflicting-signature`, `invalid-apk`, `insufficient-storage`,
`downgrade`, `blocked` or `other`) and Android's own message. An installed app appears in the
Start menu like every other app Android reports.
### Accessibility

Each task window has a UI Automation tree made of the Android app's accessibility nodes
(`src/app_window/accessibility.rs`), so Narrator and other screen readers read the app, and
inspection tools see it.

- A client that reads a window sends it `WM_GETOBJECT`. The window answers with a fragment
  root whose children are the task's nodes, and the first request starts a thread in the
  window program that watches the task's tree through the service's local API
  (`WatchAccessibility`). The service asks the agent in Android for it. Before any client asks,
  nothing runs.
- Each node has the control type of its role (a Settings row is a list item, a switch is a
  button with the Toggle pattern), the name Android gives it (a row without one takes the text
  inside it), and its bounds on the screen: the window's client area plus the node's place in
  the task, which is 1:1 with the window's pixels. Nodes support Invoke, Value, Toggle,
  SelectionItem and Scroll as they can (`src/ui_automation.rs`). Each pattern call becomes the
  node's accessibility action in Android, so Invoke presses a row without moving the pointer.
- As changes arrive the window raises structure changed events, property changed events (name,
  value, toggle state, selection, enabled, bounds) and focus changed events, while a client
  listens for them.
- Once no client has asked for the tree for 30 seconds and none listens for events
  (`UiaClientsAreListening`), the thread ends, the connection closes, and Android stops reading
  the task and turns its accessibility service off when no other window is read.
- A tree with more than 4000 nodes, or one that is not a tree, is dropped and logged in
  `dsa-app-window.log`, and the watch starts over.

To check it:

1. Open Settings in Android.
2. Run `inspect.exe` from the Windows SDK, or Accessibility Insights for Windows, and point it
   at the Settings window. Its tree shows list items named as the rows read, and each one's
   bounding rectangle sits on the row.
3. Invoke a list item (Inspect: Action, Invoke). Settings opens that screen, the pointer does
   not move, and the tree shows the new screen within a second.
4. Close the tool, wait 30 seconds, and run `adb shell settings get secure
   enabled_accessibility_services` from a debug build: the agent's service is no longer listed.
5. Start Narrator (Ctrl+Win+Enter), focus the Settings window, and move through its items
   (Caps Lock+Right). Narrator reads each list item.

## Sleep

When every Android window is closed or minimized for `doze_delay_secs` (30 seconds by
default, in `dsa-hostd.json`), Android's screen goes off and Android goes into Doze, as a
phone does in a pocket: notifications, alarms and widgets keep working in Doze's short
maintenance windows, and the CPU is idle in between. `dsa status` then says `Dozing`.
Showing any Android window turns the screen on again at once.

Android's battery follows the computer's: on AC power or on battery, the charge, and
battery saver.

Freezing stops Android's processes altogether, which also stops its notifications,
alarms and widget updates, so it is never done by default. `dsa freeze` freezes Android
now. Turning on "freeze when idle" (Advanced in DSA Settings, or `dsa settings set freeze
on`) freezes it after 30 minutes in Doze, or after `freeze_after_minutes` from
`dsa-hostd.json`. Showing a window, or input in one, resumes it.

## Memory

`dsa memory` shows how much memory the user's Android takes. On the host: the VM's `vmmem`
process (told apart from other VMs by its account, `NT VIRTUAL MACHINE\<VM ID>`), `dsa-wm`
and each app window, as resident memory (what Task Manager shows) and private memory. Inside
Android, from the report the agent sends: memory in use, page cache, app memory, zram swap,
and the processes with the largest proportional set size. The service asks Android for a
fresh report first, so the command takes a moment.

```text
Host: 820 MiB in all
  vm              pid 4       600 MiB resident,    300 MiB private
  window manager  pid 10      120 MiB resident,     60 MiB private
  ...
Android (0 s ago): 2048 of 4096 MiB in use, 3000 MiB available
  page cache 900 MiB (shared memory 20), buffers 10, reclaimable kernel caches 80
  ...
```

The service also logs one line for each running Android every five minutes:

```text
dsa-hostd 0.1.0: S-1-5-21-...: memory: host 820 MiB (vm 600, window manager 120, 2 app windows 100); guest 2048/4096 MiB used, cache 910, apps 700, swap 100 (zram 30), pss 650 MiB, 42 s ago
```

What keeps the number small:

- The VM's memory is backed by the pagefile and committed on first touch, and the pages the
  guest frees and reports go back to the host (`vm.rs`). On the Windows builds WSL does this
  on (26100 and later, and earlier ones from a set update revision), the VM has small backing
  pages and faults 64 KiB at a time, and the guest reports free memory in 128 KiB pieces.
  Elsewhere it reports 2 MiB pieces (`page_reporting_order`, 5 or 9).
- Inside Android, the control daemon has cold pages moved to compressed memory (zram) and the
  clean caches dropped when no app has been in use for a minute, so that memory is free to
  report. The agent ends cached apps when the guest goes to sleep with no task open.
- A window host empties its working set when its window is minimized, and the window manager
  does when no window has been shown for 30 seconds.
- `dsa-wm` empties its working set 30 seconds after the last task window closes, and logs
  how much it held. Memory it touches again comes back by itself.

## DSA Settings

`settings/` is the settings app, a WPF app in C# with WPF UI (MIT), an open source
library of Windows 11 style controls: a navigation view with the pages
General (start and stop Android, its state and last error), Memory (the VM, the window
manager and the app windows on the host, and Android's own breakdown), Display & GPU (the
render GPU), Updates, Advanced (freeze when idle, off by default, opening web links in
Android apps instead of the default browser, off by default, and the log folders) and
About, built from cards, with Mica behind the window where Windows has it. It follows the
system's theme, accent colour and language (English and Korean). The Windows App SDK and
WinUI 3 are not used, because their runtime's licence is not open source.

It changes nothing itself. It runs `dsa` with `--json` (`dsa status`, `dsa settings`,
`dsa settings set`, `dsa memory`, `dsa gpus`) and shows what the service answers, so every
setting goes through the service's local API, and the local API stays generated for Rust
only.

| Path | What it is |
|---|---|
| `settings/DsaSettings/` | The app: its window, pages and texts (`Strings.resx`, `Strings.ko.resx`) |
| `settings/DsaSettings.Core/` | Reading `dsa`'s JSON, and the view models the pages show |
| `settings/DsaSettings.Core.Tests/` | The view model tests, against a fake `dsa` |

From `dsa/host/platform/windows/settings`, with the .NET 8 SDK:

```
dotnet test DsaSettings.Core.Tests/DsaSettings.Core.Tests.csproj
dotnet publish DsaSettings/DsaSettings.csproj -c Release -r win-x64
```

The installer publishes it self-contained, with the .NET runtime in its folder, into `C:\Program Files\DSA\settings\`, and adds the Start menu shortcut
"DSA Settings".

## Measuring on a PC

`tools/measure.ps1` measures the signed-in user's Android on a real PC and prints the
numbers with the environment they were taken in: the memory of a dozing Android (guest PSS,
the VM, the window processes and the service, against 800 MB), its idle CPU as a share of
one logical core, how long an app's window takes to show from Dozing (and, with `-Cold`,
from Stopped), and how long the VM's memory takes to go back to the host after a stop.

```
pwsh -File tools\measure.ps1 -Part all -Package com.android.settings -Cold
```

Run it from an elevated prompt: only an administrator may read the VM process's CPU time
and the service's memory. Without that, those numbers print as `null` and the figure is
marked incomplete. `-Json` prints one JSON object.
