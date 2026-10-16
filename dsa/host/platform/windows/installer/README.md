# installer

The Windows installer: `dsa.wxs` is the package (MSI), `bundle.wxs` wraps it in
`setup.exe` with this project's own setup screens (`theme/`). They are built with WiX 5,
a build tool that is not shipped with DSA.

```
dotnet tool install --global wix --version 5.0.2
wix extension add -g WixToolset.Bal.wixext/5.0.2
pwsh dsa/host/platform/windows/installer/build.ps1 -Version 0.1.0 -Kernel bzImage `
    -ListUrl https://.../images.json -TrustedKey <public key>
```

The results land in `dsa/target/installer/`.

## What the package does

- Installs `dsa-hostd` as the `DsaHost` service (SYSTEM, started automatically) and puts
  `dsa` on the system PATH.
- Writes the service's configuration to `C:\ProgramData\DSA\dsa-hostd.json`. A later
  install keeps an edited file.
- Puts the guest kernel in `C:\Program Files\DSA\vm\` and, when built with `-ImageDir`,
  the first Android image in the update store. Without an image the service downloads the
  newest supported one from `-ListUrl`.
- Lets Hyper-V's virtual machines (`S-1-5-83-0`) read the kernel and the images, which a VM
  needs to boot from them. `C:\ProgramData\DSA` is otherwise for SYSTEM and Administrators
  only.
- Stops with a message saying how to turn on Virtual Machine Platform when it is off. It
  never turns the feature on by itself.
- Uninstalling removes the program, the service and the downloaded images, and keeps each
  user's Android data in `C:\ProgramData\DSA\users\`.

## Why WiX 5

WiX 6 and later ask organisations that earn revenue to pay a maintenance fee and accept a
licence to build with it. WiX 5 is under the Microsoft Reciprocal License only.

The setup screens are this project's own theme. Its control names are the ones WiX's
standard bootstrapper application expects; the layout, colours, text and logo
(`theme/make-logo.py`) are written here.
