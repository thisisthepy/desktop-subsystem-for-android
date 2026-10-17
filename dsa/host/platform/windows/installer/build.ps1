# Builds the DSA package (dsa-<version>.msi) and setup.exe (setup-<version>.exe) into
# dsa\target\installer. Needs Rust, and WiX 5:
#
#   dotnet tool install --global wix --version 5.0.2
#
# The bootstrapper extension is downloaded from NuGet into dsa\target\installer and passed
# to WiX by path. (In 5.0.2 its DLL is named WixToolset.BootstrapperApplications.wixext.dll,
# which WiX does not find under the package's name.)
#
# -ImageDir names an image folder made by dsa/guest/image/make-image.py; its version comes
# from its image.json. (-ImageVersion, -ImageMajor and -ImageBuild describe an older folder
# that holds only system.vhdx.) Without -ImageDir the package carries no Android image; the service then downloads the
# newest supported image from -ListUrl after installation. -PlaceholderKernel puts an empty
# kernel in the package, for testing the installer before a kernel is built.
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
    [string]$PipeSddl
)
$ErrorActionPreference = "Stop"

$installer = $PSScriptRoot
$dsa = Resolve-Path (Join-Path $installer "..\..\..\..")
$out = Join-Path $dsa "target\installer"
$stage = Join-Path $out "stage"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null

Push-Location $dsa
try {
    cargo build --release -p dsa-host-windows --bin dsa-hostd -p dsa-cli --bin dsa
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally { Pop-Location }
Copy-Item (Join-Path $dsa "target\release\dsa-hostd.exe") $stage
Copy-Item (Join-Path $dsa "target\release\dsa.exe") $stage

if ($Kernel) {
    Copy-Item $Kernel (Join-Path $stage "bzImage")
} elseif ($PlaceholderKernel) {
    Set-Content -Path (Join-Path $stage "bzImage") -Value "" -NoNewline
} else {
    throw "name the guest kernel with -Kernel (or -PlaceholderKernel for an installer test)"
}

if ($ImageDir) { $ImageDir = (Resolve-Path $ImageDir).Path }
$imageManifest = $ImageDir -and (Test-Path (Join-Path $ImageDir "image.json"))
if ($imageManifest) {
    $image = Get-Content -Raw (Join-Path $ImageDir "image.json") | ConvertFrom-Json
    foreach ($name in @($image.files.PSObject.Properties.Name)) {
        if ($name -notin @("disk.vhdx", "initrd.img", "bzImage")) {
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
    kernel        = "{program}\vm\bzImage"
    images_dir    = "{data}\images"
    first_image   = $first
    log_file      = "{data}\logs\dsa-hostd.log"
}
if ($PipeSddl) { $config.pipe_sddl = $PipeSddl }
if ($ListUrl) {
    if ($TrustedKey.Count -eq 0) { throw "-ListUrl needs at least one -TrustedKey" }
    $config.update = [ordered]@{ list_url = $ListUrl; trusted_keys = $TrustedKey }
}
$config | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8NoBOM (Join-Path $stage "dsa-hostd.json")

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

$msi = Join-Path $out "dsa-$Version.msi"
$setup = Join-Path $out "setup-$Version.exe"
$defines = @("-d", "Version=$Version", "-d", "SourceDir=$stage")
if ($ImageDir) { $defines += @("-d", "ImageDir=$ImageDir", "-d", "ImageVersion=$ImageVersion") }
if ($imageManifest) {
    $defines += @("-d", "ImageManifest=1")
    if ($image.files.PSObject.Properties.Name -contains "bzImage") { $defines += @("-d", "ImageKernel=1") }
}

Push-Location $installer
try {
    wix build -arch x64 @defines dsa.wxs -o $msi
    if ($LASTEXITCODE -ne 0) { throw "building the package failed" }
    wix build -arch x64 -ext $balDll -d "Version=$Version" -d "MsiPath=$msi" bundle.wxs -o $setup
    if ($LASTEXITCODE -ne 0) { throw "building setup.exe failed" }
} finally { Pop-Location }
Write-Host "built $msi and $setup"
