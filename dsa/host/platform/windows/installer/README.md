# installer

The Windows installer: `dsa.wxs` is the package (MSI), `bundle.wxs` wraps it in
`setup.exe` with this project's own setup screens (`setup-ui/`). They are built with WiX 5
and the .NET SDK, build tools that are not shipped with DSA.

```
dotnet tool install --global wix --version 5.0.2
pwsh dsa/host/platform/windows/installer/build.ps1 -Version 0.1.0 -Kernel bzImage `
    -GfxstreamBackend gfxstream_backend.dll `
    -ListUrl https://.../images.json -TrustedKey <public key> -AndroidMajor 17 `
    -ProgramListUrl https://.../program.json
```

The results land in `dsa/target/installer/`.

`-PlaceholderKernel` builds a test package instead, which only checks the installer: its
kernel and graphics library are empty stand-ins. It cannot be taken for the real one. Its
files are `dsa-test-<version>.msi` and `setup-test-<version>.exe`, its product name ends in
"(test package)", and its service writes a line saying so to its log each time it starts.
CI keeps both: `installer-windows-<version>` holds test packages, and
`installer-real-windows` the real one, built with the published kernel and update lists and
the graphics library of the newest graphics build.

A release ships setup.exe only, as `DSA-v<version>-win-x64-setup.exe`, with its SHA-256 in
`SHA256SUMS.txt`. The package (`dsa-<version>.msi`, and `image.cab` when it carries an
image) is inside it, and that MSI is what deployments with `msiexec` use.
WiX's `wix burn extract DSA-v<version>-win-x64-setup.exe -o <folder>` takes it out, under the names the
engine keeps inside: `a0` is the package and `a1`, when there is one, the image cabinet.
Rename them to `dsa-<version>.msi` and `image.cab` side by side. (`setup.exe /layout`
copies only `setup.exe` itself, because the package is embedded in it.)

## What the package does

- Installs `dsa-hostd` as the `DsaHost` service (SYSTEM, started automatically) and puts
  `dsa` on the system PATH.
- Writes the service's configuration to `C:\ProgramData\DSA\dsa-hostd.json`. A later
  install keeps an edited file.
- Lets the user choose both folders in `setup.exe` (Change, on its first screen): the program folder
  (`C:\Program Files\DSA` by default) and the folder for the configuration, logs and
  Android images (`C:\ProgramData\DSA` by default). The configuration names them as
  `{program}` and `{data}`, and the service fills them in when it reads it. With the MSI
  alone they are the `INSTALLFOLDER` and `DATAFOLDER` properties. The package records
  both under `HKLM\SOFTWARE\thisisthepy\DSA`, and a later version keeps them.
- With `-ProgramListUrl`, lets the service update DSA itself: it checks the signed
  program list, downloads a newer `setup.exe` into `C:\ProgramData\DSA\updates`, and runs
  it quietly once no user's Android is running (or at once on `dsa update`). Setup logs to
  `C:\ProgramData\DSA\logs\setup-<version>.log`. `"program": false` in the `update`
  section of the configuration turns it off.
- Keeps each user's Android disk in their own profile, as
  `%LOCALAPPDATA%\DSA\android.vhdx`, the way WSL keeps each user's distributions. The
  service creates it the first time that user starts Android, and gives the VM access to
  that one file when it starts.
- Puts the guest kernel in `C:\Program Files\DSA\vm\` and, when built with `-ImageDir`,
  the first Android image in the update store. `-ImageDir` names a folder made by
  `dsa/guest/image/make-image.py`, and the image's version comes from its `image.json`.
  Without an image the service downloads the newest supported one from `-ListUrl`.
- Installs DSA Settings, the settings app, in `C:\Program Files\DSA\settings\` with a
  Start menu shortcut named "DSA Settings". It is a self-contained WPF app with WPF UI's Windows 11 style controls
  (built from `dsa/host/platform/windows/settings/`), so it carries the .NET runtime and
  needs nothing installed first. It changes settings only through the service,
  which keeps them in `C:\ProgramData\DSA\settings` (`settings_dir` in the configuration).
- Installs the APK Installer (`DsaApkInstaller.exe`) beside it, in the same folder and on the
  same runtime, without a Start menu shortcut. `.apk` files open in it, and Open with lists it
  as "APK Installer".
- Puts the licence bundle and master notice in `C:\Program Files\DSA\licenses\` and
  `C:\Program Files\DSA\NOTICE.txt`.
- Keeps the image in `image.cab` beside the MSI, because an MSI holds at most 2 GB.
  `setup.exe` carries both.
- Lets Hyper-V's virtual machines (`S-1-5-83-0`) read the kernel and the images, which a VM
  needs to boot from them. `C:\ProgramData\DSA` is otherwise for SYSTEM and Administrators
  only.
- Replaces every program file on every installation, upgrade and repair, even one whose
  version is higher or that was changed after it was installed. Before that it ends every
  DSA program still running from them in every session (the window manager, the app
  windows and the settings app), and afterwards starts the window manager again for each
  signed-in user.
- Installs a rebuild of the version already installed (development and CI builds share a
  version) as an upgrade: double-click the newer MSI or `setup.exe`. Each build is its own
  product to Windows Installer, so it replaces the one before it rather than repairing it.
  `msiexec /i dsa-<version>.msi REINSTALL=ALL REINSTALLMODE=vamus` does the same for a newer
  build, and repairs the build that is installed when it is the same package.
- Removes the images, the settings and each user's files only on a real uninstall. A repair
  (`msiexec /fa`, or Repair in Installed apps), a reinstall, a change and an upgrade keep
  them.
- When the image the service last used has missing or broken files, the service moves to
  a usable installed image, or downloads one when there is none, rather than waiting on
  files that cannot boot.
- Stops with a message saying how to turn on Virtual Machine Platform when it is off. It
  never turns the feature on by itself.
- Uninstalling removes the program, the service, the downloaded images and setup files,
  and keeps each user's Android disk in their profile.

## Why WiX 5

WiX 6 and later ask organisations that earn revenue to pay a maintenance fee and accept a
licence to build with it. WiX 5 is under the Microsoft Reciprocal License only.

## The setup screens

`setup-ui/` is the program behind `setup.exe`'s window: a managed bootstrapper application
for WiX 5's engine, written in C# with WPF. The engine starts it as its own process
(`DsaSetup.exe`) and drives it through `WixToolset.BootstrapperApplicationApi`. It runs on
.NET Framework 4.7.2, which every supported Windows 10 and 11 already has, so setup
installs nothing before it.

It is one small window in the Windows 11 style: a 520 px dialog with Mica behind it (a
solid colour on Windows 10), rounded corners, its own caption bar, and the light or the
dark theme, whichever Windows uses for apps. Every screen shares the same top, the logo,
the product name, the publisher and three chips (the version, the newest Android the
release supports, and x64), and the same footer, with the buttons on the right.

- **Install**: what setup does, the install location with Change, and the licence. Change
  edits both folders, with Browse buttons that open the Windows folder picker.
- **Progress**: the percentage, a progress bar and what Windows Installer is doing.
- **Installed**: how to follow Android with `dsa status`.
- **Installed already**: Repair or Uninstall, then Repaired or Uninstalled.
- **Did not finish**: Windows Installer's message, if any, and a link to the log file.
- **Restart**: when Windows needs a restart to finish, the last screen says so and offers
  Restart.
- **Help**: `setup.exe /?` lists the command line.

The command line works without the screens: `/quiet` shows nothing and `/passive` shows
only progress; `/install`, `/repair`, `/uninstall` and `/layout [folder]` choose what to
do; `/norestart` never restarts; `/log file` writes a log; and `InstallFolder=<folder>
DataFolder=<folder>` choose the folders.

The logo (`setup-ui/assets/logo.ico` and `logo.png`) is rendered from the DSA symbol by
`dsa/brand/generate.py`. The window's text, colours and
layout are this project's own.

## Third-party parts and their licences

- WiX 5 (`wix`, the bootstrapper extension, and the engine inside `setup.exe`): Microsoft
  Reciprocal License.
- `WixToolset.BootstrapperApplicationApi` (shipped inside `setup.exe` as
  `WixToolset.BootstrapperApplicationApi.dll` and `mbanative.dll`): Microsoft Reciprocal
  License.
- `Microsoft.NETFramework.ReferenceAssemblies` (used only to build): MIT.

### The licence rule

`generate-licenses.py` writes the licence bundle and fails the build when a shipped
component breaks this rule:

- Rust crates come from `dsa/Cargo.lock` and must carry a licence from `dsa/deny.toml`.
- Every other component (the .NET packages, WiX, gfxstream and its dependencies, the guest
  image and DSA's guest programs) is listed in `dsa/third-party.toml`. A package that a
  shipped `.csproj` or the agent's `build.gradle.kts` references but that file does not
  list fails the check.
- A component linked into a DSA program must carry a permissive licence from the allow list
  there. A file-level reciprocal licence (MS-RL) is allowed only for a component shipped
  unmodified. A GPL licence is allowed only for a separate program whose corresponding
  source ships beside it: the guest kernel, which runs in its own virtual machine and is
  never linked with DSA code. Every release that ships it (the installer and each Android
  image) carries `kernel-source.tar.gz`, made by `dsa/kernel/source-bundle.sh`, and
  `licenses\kernel-source-offer.txt` says where that asset is.

The bundle also has `third-party-components.txt`, which lists those components with their
licences and sources. The settings app's About page opens the bundle's folder.
