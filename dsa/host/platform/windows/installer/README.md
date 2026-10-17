# installer

The Windows installer: `dsa.wxs` is the package (MSI), `bundle.wxs` wraps it in
`setup.exe` with this project's own setup screens (`theme/`). They are built with WiX 5,
a build tool that is not shipped with DSA.

```
dotnet tool install --global wix --version 5.0.2
pwsh dsa/host/platform/windows/installer/build.ps1 -Version 0.1.0 -Kernel bzImage `
    -ListUrl https://.../images.json -TrustedKey <public key>
```

The results land in `dsa/target/installer/`.

## What the package does

- Installs `dsa-hostd` as the `DsaHost` service (SYSTEM, started automatically) and puts
  `dsa` on the system PATH.
- Writes the service's configuration to `C:\ProgramData\DSA\dsa-hostd.json`. A later
  install keeps an edited file.
- Lets the user choose both folders in `setup.exe`'s Options: the program folder
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

The setup screens are this project's own theme. Its control names are the ones WiX's
standard bootstrapper application expects; the layout, colours, text and images are
written here. The look follows WinUI with Android's colours: a light backdrop, the pages on a
white card with rounded corners, and Android green for the button that goes ahead.
`theme/make-theme.py` draws the backdrop, the button images and the logo
(`uv run dsa/host/platform/windows/installer/theme/make-theme.py`); the logo uses Android's
colours but no Android mark.
