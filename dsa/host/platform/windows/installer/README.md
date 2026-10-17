# installer

The Windows installer: `dsa.wxs` is the package (MSI), `bundle.wxs` wraps it in
`setup.exe` with this project's own setup screens (`setup-ui/`). They are built with WiX 5
and the .NET SDK, build tools that are not shipped with DSA.

```
dotnet tool install --global wix --version 5.0.2
pwsh dsa/host/platform/windows/installer/build.ps1 -Version 0.1.0 -Kernel bzImage `
    -ListUrl https://.../images.json -TrustedKey <public key> -AndroidMajor 17
```

The results land in `dsa/target/installer/`.

A release ships `setup.exe` only. The package (`dsa-<version>.msi`, and `image.cab` when
it carries an image) is inside it, and that MSI is what deployments with `msiexec` use.
WiX's `wix burn extract setup-<version>.exe -o <folder>` takes it out. (`setup.exe /layout`
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
  alone they are the `INSTALLFOLDER` and `DATAFOLDER` properties.
- Keeps each user's Android disk in their own profile, as
  `%LOCALAPPDATA%\DSA\android.vhdx`, the way WSL keeps each user's distributions. The
  service creates it the first time that user starts Android, and gives the VM access to
  that one file when it starts.
- Puts the guest kernel in `C:\Program Files\DSA\vm\` and, when built with `-ImageDir`,
  the first Android image in the update store. `-ImageDir` names a folder made by
  `dsa/guest/image/make-image.py`, and the image's version comes from its `image.json`.
  Without an image the service downloads the newest supported one from `-ListUrl`.
- Keeps the image in `image.cab` beside the MSI, because an MSI holds at most 2 GB.
  `setup.exe` carries both.
- Lets Hyper-V's virtual machines (`S-1-5-83-0`) read the kernel and the images, which a VM
  needs to boot from them. `C:\ProgramData\DSA` is otherwise for SYSTEM and Administrators
  only.
- Stops with a message saying how to turn on Virtual Machine Platform when it is off. It
  never turns the feature on by itself.
- Uninstalling removes the program, the service and the downloaded images, and keeps each
  user's Android disk in their profile.

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

The logo (`setup-ui/assets/make-logo.py`) is drawn here. The window's text, colours and
layout are this project's own.

## Third-party parts and their licences

- WiX 5 (`wix`, the bootstrapper extension, and the engine inside `setup.exe`): Microsoft
  Reciprocal License.
- `WixToolset.BootstrapperApplicationApi` (shipped inside `setup.exe` as
  `WixToolset.BootstrapperApplicationApi.dll` and `mbanative.dll`): Microsoft Reciprocal
  License.
- `Microsoft.NETFramework.ReferenceAssemblies` (used only to build): MIT.
