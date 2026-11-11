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

### Android's log

Android's log stays in memory in the guest and is never written to a file. The guest's
channel daemon reads it only while someone does (`dsa logcat`, the Android log page of DSA
Settings) and sends it on vsock port `log_port` (5001), apart from the control channel, so a
busy log never delays input or windows. The service keeps the last 1 MiB of what came in
memory, for a reader that joins later, and reads Android's log while it boots, to explain a
boot that stalls. A connection on the log port passes the same handshake as the control
channel, so only the guest's daemon gets in.

```
dsa logcat                        # follow the log; Ctrl+C ends it
dsa logcat -d -b all *:W          # what Android's log holds now, warnings and above
dsa logs collect -o diag.zip      # everything a problem report needs, as one zip
```

The diagnostics archive holds Android's whole log, its tombstones, ANR traces and dropbox,
pstore, a summary of the system services' state, the console of the user's Android, the
logs of the processes in their session, and the service's log without the lines about other
users. `summary.txt` in the archive says what could not be gathered.

The serial console carries only the kernel's early boot and a panic, and the service writes
at most 2 MiB of it a boot to `android-<SID>.log`.

### Where files go

Everything DSA makes at run time is in two folders, and `src/paths.rs` is the one place
that builds their paths. The settings app and the installer follow the same layout.

| Machine (`%ProgramData%\DSA`, or the data folder chosen at install) | |
|---|---|
| `dsa-hostd.json` | the service's configuration |
| `images\` | the Android image store |
| `updates\` | downloaded setup files and the update state |
| `settings\` | the settings users change in DSA Settings, and widget placements |
| `logs\` | `dsa-hostd.log` and each user's Android console, `android-<SID>.log` (the kernel's early boot and a panic, at most 2 MiB a boot) |
| `tmp\` | each boot's initrd while the VM starts; SYSTEM and Administrators only |

| Each user (`%LOCALAPPDATA%\DSA`) | |
|---|---|
| `android.vhdx`, `android.image-version` | the user's Android disk and the image it holds |
| `pstore.vhd` | a fixed 4 MiB disk where the guest kernel keeps its last log across a crash; uninstalling removes it |
| `addons.vhd` | the add-ons the user installed (the Google apps, an ARM translation package), a fixed disk the VM attaches read only so the guest reads each image once; a change waits in `addons.vhd.next` for the next start of Android; uninstalling removes it |
| `apps\<package>\` | each app's window program |
| `notifications\` | the icons notifications show |
| `logs\` | `dsa-wm.log`, `dsa-app-window.log` |
| `compositor.json` | the render GPU chosen before DSA Settings kept it |
| `monitors.txt` | the user's monitors, for sizing the guest display |
| `monitor-density.txt` | the main monitor's DPI, for the density Android starts at |
| `start-menu.json` | the Start menu shortcuts the window manager made |
| `tmp\` | temporary files of the user's processes, when one cannot be avoided |

Outside them DSA writes only the Start menu shortcuts, its key `HKLM\SOFTWARE\thisisthepy\DSA`,
the `Run` value that starts `dsa-wm`, the `.apk` association and context menu entries, the
Hyper-V socket service registrations, in each user's classes the notification protocol
and each app's AppUserModelID, and each app's entry in the user's list of installed apps.

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
  "program_list_url": "https://.../program-stable.json",
  "prerelease_program_list_url": "https://.../program-prerelease.json",
  "channel": "prerelease",
  "program_dir": "C:\\ProgramData\\DSA\\updates",
  "trusted_keys": ["<public key from dsa-release keygen>"]
}
```

- A minute after it starts and then every `check_interval_hours`, it reads the program
  list and its signature (`<list>.sig`). The list names the newest version and its
  `setup.exe`, with the file's size and SHA-256 (`dsa-release program` writes it).
- There is one list per release channel. `program_list_url` is the stable list and
  `prerelease_program_list_url` the pre-release list. A computer on the stable channel reads
  the stable list only; one that takes pre-releases reads both and installs the higher
  version. The channel is the machine setting `prerelease-updates` (`dsa settings`, or
  Updates in DSA Settings, as an administrator). A computer starts on `channel`, the
  channel its installer was published on (below 1.0 every release is a pre-release), and
  upgrades keep the channel it has. Moving back to the stable channel never installs a
  lower version: DSA waits until the stable list offers a newer one.
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

Android cannot finish booting without the renderer `dsa-wm.exe` starts, so the service makes sure
it runs. When a user's Android starts, and about once a second while it runs, the service looks
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
  `io.github.thisisthepy.dsa-notification:` protocol, which the window manager registers for the user to run
  `dsa-wm.exe --notification <uri>`: that passes the click to the service and ends, and the
  app opens in Android, which leaves Doze first. Dismissing a toast dismisses the
  notification in Android too, unless the app keeps it.
- It shows each app that keeps running in Android with no window, because it runs a
  foreground service, as an icon in the notification area: the app's icon, with the
  service's notification as its tooltip. Each icon has a GUID made from the app's package,
  so Windows remembers per app whether it shows on the taskbar. A click opens the app; the
  right click menu has Open, the notification's actions and Stop, which asks first (with
  "Do not ask again", the `stop-without-asking` setting) and then has Android force-stop
  the app. The icon goes when the service ends. An app's media session gets Windows' media
  controls (SystemMediaTransportControls) through a hidden window that carries the app's
  AppUserModelID, so the media keys and the media flyout reach it. The service's ongoing
  notification is never a toast.
- It shows the widgets the user placed on the desktop, each in a borderless layered window
  of its own. The window is a tool window owned by the shell's desktop window (`Progman`),
  so it is neither in the taskbar nor in Alt+Tab, stays when the user shows the desktop, and
  is kept below every other window; it never takes the focus. It shows the widget's PNG
  frames, decoded with WIC, with what the widget leaves clear see-through. Until the first
  frame comes, or when a frame has nothing in it, it shows a mostly opaque dark card with
  rounded corners, so a widget is never an invisible hole on the desktop. While the pointer
  is over a widget, a small round "More" button fades in at its top right corner, inside
  its rounded corners. Clicking it, or right-clicking the widget, opens the widget's menu:
  Widget settings, for a widget whose app lets it be set up again, Resize, with the sizes
  the widget allows, and Remove (in Korean when that is the display language). The menu is
  a Windows 11 menu drawn by the window manager, with rounded corners, the acrylic backdrop
  (a solid fill on Windows 10 or with transparency effects off), the light or dark theme,
  icons and full keyboard use, and UI Automation reads it as a menu. Widget settings opens
  the widget's setup again as the app's window, and the widget shows what the user chose
  once they finish. A long press edits the widget as Android's home screen does: a finger
  or a pen held on it, or the left mouse button held for half a second without moving past
  the drag threshold. Android's press is cancelled then, so a long press never also clicks.
  The widget lifts a little over a soft shadow, with an accent border and a handle on each
  edge it may be resized along; dragging moves it, letting go opens its menu under it, the
  handles resize it within the widget's minimum and maximum, and Esc, a click anywhere else
  or the end of a move or a resize ends the edit. A press that moves sooner stays the
  widget's own, so its lists scroll and its buttons work. Dragging the More button also
  moves the widget, and dragging its edges resizes it. When the user lets go, the service
  remembers the placement (per user, beside the user's settings, as
  `users/<user>.widgets.json`) and tells Android the window's size, which the widget draws
  at. Presses, moves and releases inside the widget go to Android. The widgets come from the
  settings app's Widgets page, which shows changes made elsewhere through
  `dsa widgets watch`, or `dsa widgets list|add <provider>|settings <id>|remove <id>`. The
  window manager starts at sign-in and starts Android then, so the first app opened after
  the computer starts does not wait for a boot and remembered widgets come back.
- It owns no window. Each Android app task shows in a window of the app's own window
  program (below). The window manager watches the user's tasks through the service's
  local API, starts each app's window program, and tells it to open a window for each
  task, titled by the task's label and sized by the task's size in Android, and to close
  it when the task ends. Closing a window asks Android to end its task.
- The graphics renderer runs in a process of its own, `dsa-gfx.exe`, which Task Manager
  shows as "Graphics renderer for Android" ("Android용 그래픽 렌더러" on a Korean desktop).
  `dsa-wm` starts it from beside itself and tells it which VM to take streams from. The
  renderer accepts the guest's graphics streams on Hyper-V sockets, ports 5566 and 5567, and
  hands them to `gfxstream_backend.dll`, which is installed beside it. It copies each task's
  frames on the GPU into a Direct3D 11 texture the window manager makes and shares with the
  app's window program, which shows it through DirectComposition. The window manager
  duplicates each texture's NT handle into the renderer once; no frame passes through the
  CPU.
- When the renderer ends (a crash in it or in the GPU driver, or End task in Task Manager),
  the window manager and the app windows stay, and each window keeps its last frame.
  `dsa-wm` starts the renderer again at once, then waiting longer after each quick failure,
  up to 30 seconds, and logs why it ended in `dsa-wm.log`. The guest sees its graphics
  streams drop and connects again, and the windows draw again with its next frames. The
  renderer ends with its window manager. `dsa memory` lists it as the graphics renderer,
  and its memory is logged on a line of its own.
- Frames reach a window only for a display that Android named as a task's. Frames for any
  other display, such as Android's whole screen, are dropped, so no window shows more
  than one app.
- One Android pixel is one pixel of the window, so frames are shown 1:1 and stay sharp
  at any monitor scale. Each task draws at the density of its window's monitor: 96 dpi at
  100%, 144 at 150%, 192 at 200%, since an Android dp is 1/160 inch and a Windows pixel
  at 100% 1/96 inch. Android scales text on its own, so its font scale is 160/96 times
  the Windows text size (Accessibility > Text size), and text keeps the size of Windows'
  text. A change to the text size reaches Android at once. Android restarts an app that
  does not say it handles a change of font scale, so while such an app is open the new
  scale waits, and it applies as soon as none is. A window that opens on, or moves to, a
  monitor of another scale asks Android for that density, and then for the size that
  keeps the app the same size to the eye. Density is set per task, so windows on monitors of different
  scales are each sharp.
- Resizing a window asks Android to give its task the window's new client size. During a
  live resize only the latest size is sent, and the next one waits until the app has drawn
  the last (at most 150 ms), so an app slower than the drag always works on the latest
  size. The window changes size only together with a frame of that size: it keeps the size
  of the frame it shows until the app has drawn a new one, then takes that frame's size and
  shows the frame in the same composition commit. It follows the mouse as fast as the app
  draws, and a frame is never stretched, cropped or shown with a margin. The size a drag
  ends at is reached exactly. A window cannot grow past what Android's display can hold.
  That display is chosen when Android starts: a square as long as the longest monitor,
  from 1920 to 3840 pixels on a side, so a window fills any monitor in either orientation. The service cannot
  see the user's monitors from session 0, so the window manager and the app windows record
  them in `%LOCALAPPDATA%\DSA\monitors.txt`, and the main monitor's DPI beside it in
  `monitor-density.txt`: Android starts at the density a window on the main monitor gives its
  task (96 dpi at 100%, 144 at 150%), so an app that opens there is not relaunched for a
  change of density right after it starts. A monitor longer than the display, or one
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
- A task window has no title bar above the app. Its client area reaches the window's top
  edge, the app draws there, and the window draws only its caption buttons (minimize,
  maximize or restore, close) over the app's top right corner. The band as high as a
  standard caption at the window's DPI belongs to Windows: dragging it moves the window, a
  double click maximizes it, a right click or Alt+Space opens the system menu, resting on
  the maximize button shows the Windows 11 snap layouts, and the top edge resizes the window.
  The window reports the band's height, and Android gives the task a top status bar and
  caption bar inset of that height, so the app's toolbar sits below the buttons and its
  status bar color runs behind them. The glyphs are dark over a light status bar and light
  over a dark one, as the app tells Android; close turns red under the pointer. Under a high
  contrast theme the buttons take the theme's colors. The title and icon are not drawn in
  the window; the taskbar, Alt+Tab and the system menu show them.
- Each window measures how long it waits for a frame of its new size after a resize, and
  the log gets a summary (median, 95th percentile, longest, and how many took 100 ms or
  more) every 20 resizes and when the window closes. A window still waiting after 2
  seconds is logged as keeping its size.
- Mouse, touch and pen input in a window goes to its task. A point in the window becomes
  the same point of the task in Android's pixels, whatever the window's size and the
  monitor's scale, and a point inside the window always lands inside the task. The right
  button is Android's secondary button (a context click, not a long press), the wheel
  scrolls, a pen brings its pressure, eraser and barrel button, and each finger of a touch
  is its own pointer. The window holds the mouse while a button is down, so a drag that
  leaves it still ends in the task. Input goes to the service on a connection of its own,
  which answers none of it, so a fast pointer is never held back.
- Windows 11 keeps three or more fingers from every app while its three- and four-finger
  touch gestures are on, and no public API turns them off for one app. So while a task
  window is the active window, the window manager turns them off for the user (the
  `touch-gestures-off` setting, on by default) and puts the user's own value back as soon
  as another window is. The value is kept in `touch-gestures-original.txt` in the user's DSA
  folder while the gestures are off, so a window manager that crashed puts it back when it
  next starts. If Windows refuses the change (the action is one Settings uses but does not
  document), the window manager leaves the gestures alone and the settings app says why.
  Windows 10 and Windows Server 2022 have no such gestures and are left alone.
- The keys pressed in the window that has the keyboard focus go to its task on the same
  connection, as Android key codes with their repeats and with Shift, Ctrl, Alt and the
  Windows key (each with its side) and the locks: the keys that type nothing or a control
  character (arrows, Enter, Backspace, Delete, Tab, Esc, function keys, Ctrl and Alt
  shortcuts). Alt+F4 still closes the window. When the window loses the focus, every key
  still held is released in Android, so none stays down there.
- Text goes to the task as text, on the same connection: what the Windows input method
  composes (the composing text with its cursor, which the Android editor shows underlined)
  and commits, and every character a key types without it, so any keyboard layout types what
  its keys show. A key that types a character is never also sent as a key. The window is a
  text services framework client: its document is the text of the focused Android editor
  around the selection, which the agent describes with the kind of text the editor takes.
  The input method reads that text to predict and to reconvert what was typed, and every
  edit it makes there (a composition, a commit, a new selection, a word made the composition
  again) reaches the Android editor. The Microsoft Korean IME composes there, the touch
  keyboard takes its layout from the editor's input scope (password, number, PIN, phone,
  email, URL, name, date, time, search), and dictation (Win+H), handwriting and the emoji
  panel type into it. A password, and a view that takes no text (a game's), take keys
  straight, with no input method composing. The Android editor shows the composition, so no
  composition window is drawn, and the candidate list opens beside the cursor of the focused
  Android editor, which the agent reports.
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
- Every copy takes the app's own AppUserModelID, `Thisisthepy.DSA.<package>`, the one its Start menu
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

### Installed apps in Windows Settings

Each app also shows in Windows Settings under Apps > Installed apps (and in Programs and
Features), as it did with WSA, so it can be found and uninstalled there.

- Together with the Start menu, the window manager writes one key for each app,
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Thisisthepy.DSA.<package>`, with
  `DisplayName`, `DisplayIcon` (the app's window program), `DisplayVersion`, `Publisher`,
  `InstallDate`, `EstimatedSize`, `NoModify`, `NoRepair` and `UninstallString`, which runs
  `settings\DsaSettings.exe --uninstall-app <package>`. Windows has already asked the user, so
  that opens a small window, laid out like the APK install window, that shows the app's icon,
  name and version with the progress, runs `dsa apps uninstall <package>` hidden at once, and
  shows the result: it closes itself after a success and says why otherwise. The settings app
  is a windowed program, so no console window flashes up as it would if Windows ran `dsa`
  itself.
- Keys whose names start with `Thisisthepy.DSA.` are DSA's; no other key is ever changed or
  removed. A key of an app no longer installed goes.
- Uninstalling DSA removes them in every profile (`dsa-hostd remove-user-data`), loading the
  user's `NTUSER.DAT` when the user is not signed in.

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

The window manager reads the setting when it starts and the renderer runs on that GPU from
then on, also after a restart of the renderer, so a change takes effect the next time
`dsa-wm` starts (sign out and back in, or restart it from DSA Settings). A setting that cannot be read is logged
and the default is used. Unknown keys are an error, so a misspelt setting is reported.

The log says what was asked for and what the renderer chose:

```text
render GPU setting display: asking the renderer for Intel(R) Arc(TM) Graphics (LUID 00000000:00019bec), which drives the primary monitor; task windows on other monitors are rendered there too
rendering on Intel(R) Arc(TM) Graphics (LUID 00000000:00019bec), the GPU asked for
```

When nothing matches, the second line says that the renderer ranked the GPUs itself, and
why.

### Paravirtualized GPU (experimental, off)

`"gpu_paravirtualization": true` in the service's configuration file gives each user's Android
VM Hyper-V's paravirtualized GPU, the one WSL gives its VM (the guest kernel's `/dev/dxg`).
It is off by default and no installation turns it on. It is the start of a second graphics
path, in which drivers inside Android talk to the host GPU directly instead of through the
renderer; that path is not usable yet, because no open source Direct3D 12 runtime for
Android exists to put under them.

With the setting on, the service:

- makes the VM's high MMIO window 16 GiB, where the guest maps the host GPU memory it
  writes with the CPU;
- puts `androidboot.dsa.gpu=dxg androidboot.dsa.vulkan=dxg` on the kernel command line;
- asks HCS, once the VM runs, to give it every GPU of the computer (`Mirror`, as WSL does).
  A display driver without GPU paravirtualization (WDDM 2.9 or later) refuses, the service
  logs why, and Android runs as before.

An image built without the dxg drivers ignores the boot arguments, so Android keeps rendering
through `dsa-wm`.

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

A file dragged from Explorer onto a task window goes to that window's app: each task window is
an OLE drop target (`src/app_window/drop_target.rs`), and the file is sent the same way, named
for the app, which the agent hands it to with `ACTION_SEND`. An APK dropped on a window goes to
the APK install window instead, as opening it would. Folders are not sent.

### Sharing folders with Android

The user's Documents, Downloads, Pictures, Music, Videos and Desktop folders show in Android's
shared storage under `Host/<folder>`, all of them unless the user chooses fewer, and none
while file sharing is off (`dsa settings set file-sharing on|off`,
`dsa settings set shared-folders Documents,Pictures`, or the Advanced page of DSA Settings).
The service serves each folder to the guest with a 9P2000.L file server on a Hyper-V socket
(vsock port 5590); the guest mounts it with the kernel's `9p` file system. Each connection
starts with a proof of the boot's secret, so no app in the guest reaches the server, and every
request is served impersonating the signed-in user (`src/file_share.rs`), so Windows applies the
user's own rights. Names that leave a folder (`..`, separators, device names) and links that
point outside it are refused.

The other way, Android's shared storage opens in Explorer from the "Android files" button in
DSA Settings or `dsa files open`. It has no Start menu entry, so Windows search does not find
it. The guest serves its storage as WebDAV on vsock port 5591, the service bridges a loopback port to it, and Explorer
opens it through Windows' WebDAV redirector as `\\127.0.0.1@<port>\<token>`
(`src/android_files.rs`). The token changes with each session. The redirector is the WebClient
service, which Windows starts the first time such a path is opened.

Android's storage also has its own entry, "Android", in Explorer's navigation pane, directly
below WSL's "Linux" (`SortOrderIndex` 120 to its 119). It is a shell folder shortcut whose target is the service's files door,
`\\127.0.0.1@47591\<key>`: one loopback port the service opens when it starts, so the address
holds while Android is stopped and across restarts. Opening it starts Android and waits for its
storage. The key is the user's own, made by the window manager the first time it runs and kept
beside the entry in the user's classes (`HKCU\Software\Classes\CLSID\{6F1D4E2A-...}`), which
only that user and the service can read; the door answers a key nobody has with 403. The
installer registers the entry for the machine and uninstalling removes it, with each user's own
copy.

The Start menu entry opens the same place, and checks first that it answers: given a path that
does not answer, Explorer opens the user's Documents instead and says nothing, so DSA shows an
error dialog with the reason.

The guest's folder for the host's folders was `Windows/` before (`Linux/` and `Mac/` on those
hosts). It is `Host/` on every host now; the guest takes the old mounts away and removes the
old folders once they are empty, so files a user put there stay.

## Installing APKs

An APK file installs in Android three ways, which all end in the same local API request:

- Opening it in Explorer. The installer associates `.apk` files with `settings\DsaApkInstaller.exe
  --install-apk "%1"`, the APK Installer (`settings/DsaApkInstaller/`). It is its own program,
  not part of the settings app: Explorer, Open with and the taskbar show it as "APK Installer"
  ("APK 설치 관리자" in Korean) with its own icon, and it sets its own AppUserModelID
  (`Thisisthepy.DSA.ApkInstaller`), so its windows are their own group in the taskbar. It is a
  WPF app with WPF UI, like the settings app, and shares the settings app's core
  (`DsaSettings.Core`), strings and folder, so both use one copy of the .NET runtime. Its window
  is laid out as App Installer lays out an app package, with Mica and the Windows type ramp:
  the app's name large at the top left with its package, publisher (the signing certificate,
  since an APK names no publisher) and version under it, its icon at the top right, then the
  permissions it asks for in plain words under "Capabilities" and the rest of what the file
  says under "Details" (version code, the Android versions it is made for, size, processor
  types, the SHA-256 of its signing certificate). All of it is read on the host by
  `dsa apk-info` before Android is started. An APK with native code for none of this PC's
  processor types carries a warning. When Android runs, the window also says whether the APK
  is a new install, an update (with the installed and the new version) or a downgrade or an
  APK signed with another key, which it refuses with the reason, as Android would. The footer
  has "Launch when ready" (on by default) on the left and Install, Update or Reinstall on the
  right. Install starts Android first when it is not running, checks again, then shows the
  progress bar and what is happening in place of the checkbox. It ends by opening the app and
  closing, when "Launch when ready" is on, or with Launch and Close, or with the reason it
  failed. `DsaSettings.exe --install-apk` and `dsa-app-window.exe --install-apk`, which
  earlier releases associated, hand the APK to the APK Installer.
- `dsa install <file>`, which prints the result and exits with code 1 when Android refused
  the APK. Its line then starts with `not installed (<reason>):`. With `--progress` it prints
  each step first (`stage starting`, `stage sending`, `progress <percent>`,
  `stage installing`), which the install window shows.
- Dropping APK files on the settings app's window, or on the APK Installer's window. Each APK
  opens in its own APK Installer window.

Split-APK bundles (`.xapk` from APKPure, `.apks` from bundletool and SAI, `.apkm` from
APKMirror) install the same three ways. The installer gives them a type of their own,
`Dsa.ApkBundle` ("Split APK bundle"), which opens in the APK Installer; `.aab` is only under
Open with, so the tools that build App Bundles keep opening them. What a file is comes from
what is in it (`dsa/host/core/src/apk_bundle.rs`), so a renamed bundle installs too. The window
shows the app as its base APK describes it, with the number of APKs that go (the base and the
splits) and their size together. Of a bundle's splits, the install takes those this PC's
Android needs: one ABI (x86_64 first, `arm64-v8a` only when the x86_64 libraries are missing or
leave some out and ARM translation is on), the density split nearest the main screen's
density, the splits of the display languages and English, and every feature split. They go to
Android as one install, the APKs in one `PackageInstaller` session, and the bundle's expansion
(OBB) files go to `Android/obb/<package>/` once the app is installed. A bundle whose splits are
signed with other keys, an encrypted bundle (some older `.apkm` files) and an App Bundle are
refused with their reasons before Android starts.

`dsa apk-info <file.apk>` reads an APK without Android: the binary manifest, the resource table
(for the name in the default language and the icon in the highest density, or an adaptive
icon's colour and image layers) and the APK Signature Scheme block (v3.1, v3, v2, else the JAR
signature) are parsed in `dsa/host/core/src/apk_info/`, with no third-party library. When
Android runs, it asks the agent what the package manager has of the package (`GetPackage`).

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

Android also follows the computer's sleep. The service registers for Windows' suspend and
resume notifications (`PowerRegisterSuspendResumeNotification`). Before the computer sleeps or
hibernates, each running Android's screen goes off and its control daemon writes its pending
data to disk; the service waits at most 2 seconds for that. When the computer wakes, each
Android's clock is set to the computer's, and its screen comes back on if it was on, so an
Android that was `Active` is `Active` again and one in Doze stays there. Time asleep does not
count towards freezing.

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
  pages and faults 64 KiB at a time, and the guest reports every free 4 KiB page, at once.
  Elsewhere it reports 2 MiB pieces (`page_reporting_order`, 0 or 9).
- Inside Android, the control daemon has cold pages moved to compressed memory (zram) and the
  clean caches dropped on its first look (every 5 seconds) after the last app closes, and
  page cache beyond the open apps' working set trimmed on the next look that finds it, so
  that memory is free to report at once. The agent ends cached apps when the guest goes to sleep with no task open.
- A window host empties its working set when its window is minimized, and the window manager
  does when no window has been shown for 30 seconds.
- `dsa-wm` empties its working set 30 seconds after the last task window closes, and logs
  how much it held. Memory it touches again comes back by itself.

## DSA Settings

`settings/` is the settings app, a WPF app in C# with WPF UI (MIT), an open source
library of Windows 11 style controls: a navigation view with the pages
General (start, stop and freeze Android, its state and last error), Memory (the memory
limit, automatic by default; the VM's size and what Android uses of it; the VM, the window
manager and the app windows on the host; Android's own breakdown and its kernel's), Display
& GPU (the render GPU), Updates, Advanced (freeze when idle, off by default, opening web
links in Android apps instead of the default browser, off by default, developer mode and
its adb address, the input and resize trace switches, which only an administrator changes,
and the log folders) and
About, built from cards, with Mica behind the window where Windows has it. It follows the
system's theme, accent colour and language (English and Korean). The Windows App SDK and
WinUI 3 are not used, because their runtime's licence is not open source.

It changes nothing itself. It runs `dsa` with `--json` (`dsa status`, `dsa settings`,
`dsa settings set`, `dsa memory`, `dsa gpus`, `dsa freeze`) and shows what the service answers, so every
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
one logical core, how long an app's new window takes to present its first frame and to have
it shown from Dozing (and, with `-Cold`, from Stopped), and how long the VM's memory takes to
go back to the host after a stop. The window times come from the window itself, through
`dsa launch <package> --wait-first-frame`.

```
pwsh -File tools\measure.ps1 -Part all -Package com.android.settings -Cold
```

`-Part click` measures clicks: it presses at `-TapX`, `-TapY` in the app's task `-Repeat`
times with `dsa tap`, which sends the press through the local API, so the mouse never moves
and no other window gets input. It then reads the input trace with
`scripts/input-trace.py --json` and reports the time from each press to the first app frame,
to the first visible change around the press point, and to the next screen. The input trace
must be on (`dsa settings set input-trace on`, as an administrator), and `-Relaunch` opens
the app again before each press.

```
pwsh -File tools\measure.ps1 -Part click -Package com.android.settings -TapX 300 -TapY 400 -Repeat 10 -Relaunch
```

Run it from an elevated prompt: only an administrator may read the VM process's CPU time
and the service's memory. Without that, those numbers print as `null` and the figure is
marked incomplete. `-Json` prints one JSON object.
