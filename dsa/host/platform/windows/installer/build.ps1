# Builds the DSA package (dsa-<version>.msi) and setup.exe (setup-<version>.exe) into
# dsa\target\installer. Needs Rust, the .NET 8 SDK (for the setup screens in setup-ui\ and
# the settings app in ..\settings\), and WiX 5:
#
#   dotnet tool install --global wix --version 5.0.2
#
# The bootstrapper extension is downloaded from NuGet into dsa\target\installer and passed
# to WiX by path. (In 5.0.2 its DLL is named WixToolset.BootstrapperApplications.wixext.dll,
# which WiX does not find under the package's name.) setup.exe needs it for the variables
# that the command line may set.
#
# -AndroidMajor is the newest Android major the release supports; the first screen of
# setup.exe shows it. Without it that screen leaves it out.
#
# -ImageDir names an image folder made by dsa/guest/image/make-image.py; its version comes
# from its image.json. (-ImageVersion, -ImageMajor and -ImageBuild describe an older folder
# that holds only system.vhdx.) Without -ImageDir the package carries no Android image; the service then downloads the
# newest supported image from -ListUrl after installation. -PlaceholderKernel puts an empty
# kernel in the package, for testing the installer before a kernel is built.
#
# -PlaceholderKernel makes a test package, which must never be taken for the real one: its
# files are dsa-test-<version>.msi and setup-test-<version>.exe, its product name ends in
# "(test package)", and its service says it is a test package each time it starts. A real
# package needs -Kernel and -GfxstreamBackend (or the library a local graphics build left).
#
# -Arch is the platform the package is for: x64 or arm64. Without it the package is for the
# machine that builds it. The programs are built for that platform's Rust target
# (x86_64-pc-windows-msvc or aarch64-pc-windows-msvc) and the settings app for its runtime
# (win-x64 or win-arm64). The arm64 package installs only on ARM64 Windows; its kernel is the
# arm64 build's Image rather than bzImage. The setup screens stay x64 in both: they are a
# .NET Framework 4.7.2 program that the engine starts as its own process, and ARM64 Windows
# 11 runs it under emulation.
#
# -ProgramListUrl names the signed program list the service updates DSA itself from (it
# needs -ListUrl, and the same -TrustedKey signs both lists). Without it the installed DSA
# is updated by hand.
#
# -ReleaseTag names the GitHub release this build is published as (v<version>); the licence
# bundle points at the kernel source asset of that release.
param(
    [string]$Version = "0.1.0",
    [string]$Kernel,
    [switch]$PlaceholderKernel,
    [string]$ImageDir,
    [string]$ImageVersion,
    [int]$ImageMajor,
    [long]$ImageBuild,
    [string]$ListUrl,
    [string[]]$TrustedKey = @(),
    [string]$ProgramListUrl,
    [string]$PipeSddl,
    [int]$AndroidMajor,
    [string]$GfxstreamBackend,
    [string]$ReleaseTag,
    [ValidateSet("x64", "arm64")]
    [string]$Arch
)
$ErrorActionPreference = "Stop"

if (-not $Arch) {
    # The machine's own processor: PROCESSOR_ARCHITEW6432 is set when this PowerShell is
    # itself emulated.
    $native = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    $Arch = if ($native -eq "ARM64") { "arm64" } else { "x64" }
}
$rustTarget = if ($Arch -eq "arm64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
$kernelName = if ($Arch -eq "arm64") { "Image" } else { "bzImage" }

$localGraphics = if ($Arch -eq "arm64") { "build-host-arm64" } else { "build-host" }

$testPackage = [bool]$PlaceholderKernel
$productName = if ($testPackage) { "Desktop Subsystem for Android (test package)" } else { "Desktop Subsystem for Android" }
$fileTag = if ($testPackage) { "test-" } else { "" }

$installer = $PSScriptRoot
$dsa = Resolve-Path (Join-Path $installer "..\..\..\..")
$out = Join-Path $dsa "target\installer"
$stage = Join-Path $out "stage"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null

# The programs report this release's version, not their crates' own.
$env:DSA_VERSION = $Version
Push-Location $dsa
try {
    cargo build --release --target $rustTarget -p dsa-host-windows --bin dsa-hostd --bin dsa-wm --bin dsa-app-window -p dsa-cli --bin dsa
    if ($LASTEXITCODE -ne 0) { throw "cargo build for $rustTarget failed (rustup target add $rustTarget adds the target)" }
} finally { Pop-Location }
$built = Join-Path $dsa "target\$rustTarget\release"
foreach ($program in @("dsa-hostd.exe", "dsa-wm.exe", "dsa-app-window.exe", "dsa.exe")) {
    Copy-Item (Join-Path $built $program) $stage
}

if ($GfxstreamBackend) {
    Copy-Item $GfxstreamBackend (Join-Path $stage "gfxstream_backend.dll")
} elseif (Test-Path (Join-Path $stage "gfxstream_backend.dll")) {
    # Already staged.
} elseif (Test-Path (Join-Path $dsa "..\..\..\.scratch\$localGraphics\dist\bin\gfxstream_backend.dll")) {
    Copy-Item (Join-Path $dsa "..\..\..\.scratch\$localGraphics\dist\bin\gfxstream_backend.dll") (Join-Path $stage "gfxstream_backend.dll")
} elseif ($PlaceholderKernel) {
    Set-Content -Path (Join-Path $stage "gfxstream_backend.dll") -Value "" -NoNewline
} else {
    throw "name the gfxstream backend library with -GfxstreamBackend (or -PlaceholderKernel for an installer test)"
}

# Generate the licence bundle for all shipped components and dependencies.
$python = if (Get-Command python3 -ErrorAction SilentlyContinue) { "python3" } else { "python" }
$licensesDir = Join-Path $stage "licenses"
$masterNotice = Join-Path $stage "NOTICE.txt"
$generateScript = Join-Path $installer "generate-licenses.py"
$releaseTagArgs = if ($ReleaseTag) { @("--release-tag", $ReleaseTag) } else { @() }
& $python $generateScript --workspace $dsa --out-dir $licensesDir --master-notice $masterNotice @releaseTagArgs
if ($LASTEXITCODE -ne 0) { throw "generating the licence bundle failed" }

# Reject Microsoft WSA binaries or components in the payload.
$checkNoWsaScript = Join-Path $installer "check-no-wsa.py"
& $python $checkNoWsaScript --manifests (Join-Path $installer "dsa.wxs") (Join-Path $installer "bundle.wxs") `
    --stage-dir $stage --image-inputs (Join-Path $dsa "guest\image\inputs.json")
if ($LASTEXITCODE -ne 0) { throw "WSA binary check failed" }

if ($Kernel) {
    Copy-Item $Kernel (Join-Path $stage $kernelName)
} elseif ($PlaceholderKernel) {
    Set-Content -Path (Join-Path $stage $kernelName) -Value "" -NoNewline
} else {
    throw "name the guest kernel with -Kernel (or -PlaceholderKernel for an installer test)"
}

if ($ImageDir) { $ImageDir = (Resolve-Path $ImageDir).Path }
$imageManifest = $ImageDir -and (Test-Path (Join-Path $ImageDir "image.json"))
if ($imageManifest) {
    $image = Get-Content -Raw (Join-Path $ImageDir "image.json") | ConvertFrom-Json
    foreach ($name in @($image.files.PSObject.Properties.Name)) {
        if ($name -notin @("disk.vhdx", "system.vhdx", "data.vhdx", "initrd.img", $kernelName, "NOTICE.html.gz", "NOTICE.xml.gz", "NOTICE.txt")) {
            throw "image.json names $name, which this package does not know how to install"
        }
        if (-not (Test-Path (Join-Path $ImageDir $name))) { throw "$ImageDir has no $name" }
    }
    $ImageVersion = $image.version
    $ImageMajor = $image.android_major
    $ImageBuild = $image.build
}
if ($ImageDir) {
    if (-not $ImageVersion) { throw "-ImageDir needs an image.json, or -ImageVersion, -ImageMajor and -ImageBuild" }
    $first = [ordered]@{ version = $ImageVersion; android_major = $ImageMajor; build = $ImageBuild }
} else {
    # No image yet: any supported image is newer than this, so the first check downloads one.
    $first = [ordered]@{ version = "none"; android_major = 0; build = 0 }
}
# The user chooses both folders while installing, so the service fills in {program} (its
# own folder) and {data} (this file's folder) when it reads the file. Each user's Android
# disk is in their own profile, so the file names no data_dir.
$config = [ordered]@{
    kernel        = "{program}\vm\$kernelName"
    images_dir    = "{data}\images"
    first_image   = $first
    log_file      = "{data}\logs\dsa-hostd.log"
    settings_dir  = "{data}\settings"
}
if ($testPackage) { $config.test_package = $true }
if ($PipeSddl) { $config.pipe_sddl = $PipeSddl }
if ($ListUrl) {
    if ($TrustedKey.Count -eq 0) { throw "-ListUrl needs at least one -TrustedKey" }
    $config.update = [ordered]@{ list_url = $ListUrl; trusted_keys = $TrustedKey }
}
if ($ProgramListUrl) {
    if (-not $ListUrl) { throw "-ProgramListUrl needs -ListUrl and -TrustedKey" }
    # Downloaded setup files go to the service's own folder, which only SYSTEM and
    # Administrators can write to, since the service runs them.
    $config.update.program_list_url = $ProgramListUrl
    $config.update.program_dir = "{data}\updates"
}
$config | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8NoBOM (Join-Path $stage "dsa-hostd.json")
# An upgrade keeps dsa-hostd.json as the user left it, so it goes on naming the first
# installation's image and release lists. This file is replaced by every installation and
# tells the service what this package carries: the image it bundled (booted when newer than
# the current one) and the lists it was built for.
$package = [ordered]@{ first_image = $first }
if ($testPackage) { $package.test_package = $true }
if ($config.Contains("update")) { $package.update = $config.update }
$package | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8NoBOM (Join-Path $stage "installed-package.json")

# The setup screens: a .NET Framework 4.7.2 program that setup.exe carries and starts.
$setupUi = Join-Path $out "setup-ui"
dotnet build (Join-Path $installer "setup-ui\DsaSetup.csproj") -c Release -o $setupUi -p:Version=$Version
if ($LASTEXITCODE -ne 0) { throw "building the setup screens failed" }

# The settings app: a self-contained WPF app (the .NET runtime in its own folder), installed
# in the program folder's settings\ folder beside dsa.exe.
$settingsApp = Join-Path $out "settings-app"
Remove-Item -Recurse -Force $settingsApp -ErrorAction SilentlyContinue
dotnet publish (Join-Path $installer "..\settings\DsaSettings\DsaSettings.csproj") -c Release -r win-$Arch `
    -p:Version=$Version -o $settingsApp
if ($LASTEXITCODE -ne 0) { throw "building the settings app failed" }
if (-not (Test-Path (Join-Path $settingsApp "DsaSettings.exe"))) { throw "the settings app has no DsaSettings.exe" }
# The APK Installer, the program .apk files open in: published into the same folder, where it
# shares the settings app's copy of the .NET runtime and WPF UI.
dotnet publish (Join-Path $installer "..\settings\DsaApkInstaller\DsaApkInstaller.csproj") -c Release -r win-$Arch `
    -p:Version=$Version -o $settingsApp
if ($LASTEXITCODE -ne 0) { throw "building the APK Installer failed" }
if (-not (Test-Path (Join-Path $settingsApp "DsaApkInstaller.exe"))) { throw "the settings folder has no DsaApkInstaller.exe" }

$balVersion = "5.0.2"
$balDir = Join-Path $out "ext\bal-$balVersion"
$balDll = Join-Path $balDir "wixext5\WixToolset.BootstrapperApplications.wixext.dll"
if (-not (Test-Path $balDll)) {
    New-Item -ItemType Directory -Force $balDir | Out-Null
    $nupkg = Join-Path $balDir "bal.zip"
    Invoke-WebRequest -Uri "https://api.nuget.org/v3-flatcontainer/wixtoolset.bal.wixext/$balVersion/wixtoolset.bal.wixext.$balVersion.nupkg" -OutFile $nupkg
    Expand-Archive -Path $nupkg -DestinationPath $balDir -Force
    if (-not (Test-Path $balDll)) { throw "the bootstrapper extension has no $balDll" }
}

$utilVersion = "5.0.2"
$utilDir = Join-Path $out "ext\util-$utilVersion"
$utilDll = Join-Path $utilDir "wixext5\WixToolset.Util.wixext.dll"
if (-not (Test-Path $utilDll)) {
    New-Item -ItemType Directory -Force $utilDir | Out-Null
    $nupkg = Join-Path $utilDir "util.zip"
    Invoke-WebRequest -Uri "https://api.nuget.org/v3-flatcontainer/wixtoolset.util.wixext/$utilVersion/wixtoolset.util.wixext.$utilVersion.nupkg" -OutFile $nupkg
    Expand-Archive -Path $nupkg -DestinationPath $utilDir -Force
    if (-not (Test-Path $utilDll)) { throw "the util extension has no $utilDll" }
}

$msi = Join-Path $out "dsa-$fileTag$Version.msi"
$setup = Join-Path $out "setup-$fileTag$Version.exe"
$defines = @("-d", "Arch=$Arch", "-d", "KernelName=$kernelName", "-d", "Version=$Version", "-d", "ProductName=$productName", "-d", "SourceDir=$stage", "-d", "SettingsDir=$settingsApp")
if ($ImageDir) { $defines += @("-d", "ImageDir=$ImageDir", "-d", "ImageVersion=$ImageVersion") }
if ($imageManifest) {
    $defines += @("-d", "ImageManifest=1")
    # An image of one disk (disk.vhdx), or of a system image and a data disk template.
    if ($image.files.PSObject.Properties.Name -contains "system.vhdx") { $defines += @("-d", "ImageSplitDisks=1") }
    if ($image.files.PSObject.Properties.Name -contains $kernelName) { $defines += @("-d", "ImageKernel=$kernelName") }
    foreach ($noticeCandidate in @("NOTICE.html.gz", "NOTICE.xml.gz", "NOTICE.txt")) {
        if (Test-Path (Join-Path $ImageDir $noticeCandidate)) {
            $defines += @("-d", "ImageNotice=$noticeCandidate")
            break
        }
    }
}

Push-Location $installer
try {
    $msiExt = if (Test-Path $utilDll) { @("-ext", $utilDll) } else { @("-ext", "WixToolset.Util.wixext") }
    # The package's own tables are English (the machine's package, one language); every word
    # Windows shows each user afterwards is an indirect string into dsa-app-window.exe, in
    # that user's display language. dsa.ko-kr.wxl holds the same strings for -culture ko-KR.
    $loc = @("-culture", "en-US", "-loc", "dsa.en-us.wxl", "-loc", "dsa.ko-kr.wxl")
    wix build -arch $Arch @msiExt @defines @loc dsa.wxs -o $msi
    if ($LASTEXITCODE -ne 0) { throw "building the package failed" }
    $bundleDefines = @("-d", "Arch=$Arch", "-d", "Version=$Version", "-d", "ProductName=$productName", "-d", "MsiPath=$msi", "-d", "SetupUiDir=$setupUi")
    if ($AndroidMajor -gt 0) { $bundleDefines += @("-d", "AndroidMajor=$AndroidMajor") }
    if ($ImageDir) { $bundleDefines += @("-d", "BundledImage=1") }
    wix build -arch $Arch -ext $balDll @bundleDefines bundle.wxs -o $setup
    if ($LASTEXITCODE -ne 0) { throw "building setup.exe failed" }
} finally { Pop-Location }
Write-Host "built $msi and $setup"
