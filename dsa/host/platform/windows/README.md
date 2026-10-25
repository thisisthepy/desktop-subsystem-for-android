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
  `<data_dir>\<SID>\data.vhdx` and their VM `<vm_name>-<SID>`; later requests reach the
  same VM.
- A user's folder is readable only by SYSTEM, Administrators and Hyper-V's virtual
  machines, so other users cannot read someone's Android data.
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
- The windows are per-monitor DPI aware. While a window is resized, its last frame is
  stretched to fill it until Android draws one at the new size.
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
- Started by the user rather than the window manager, the program asks Android to start
  its app; a copy that runs already only asks again. It writes its messages to
  `%LOCALAPPDATA%\DSA\logs\dsa-app-window.log`.
- Uninstalling DSA leaves the copies in the user's profile, like the user's Android disk.
- The renderer runs on the GPU that drives the primary monitor, so frames never cross
  from one GPU to another and a laptop's discrete GPU stays asleep. The renderer uses one
  GPU, so a window on a monitor that another GPU drives is still rendered there, and
  Windows copies its frames across. When no Vulkan GPU matches (in a remote desktop
  session, for example), the renderer picks one itself, preferring a discrete GPU.

### Choosing the render GPU

`%LOCALAPPDATA%\DSA\compositor.json` holds the window manager's settings for each user. It
is optional; without it every setting has its default.

```json
{ "render_gpu": "discrete" }
```

`render_gpu` is one of:

- `display` (the default): the GPU that drives the primary monitor;
- `integrated`: the first integrated GPU;
- `discrete`: the first discrete GPU, for example for games;
- any other text: the first GPU whose name contains it, ignoring case, such as `"RTX"`
  or `"Arc"`.

The renderer chooses its GPU when it starts, so a change takes effect the next time
`dsa-wm` starts (sign out and back in). A setting that cannot be read is logged
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
