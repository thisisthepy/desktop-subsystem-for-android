# Builds the DSA package (dsa-<version>.msi) and setup.exe (setup-<version>.exe) into
# dsa\target\installer. Needs Rust, and WiX 5:
#
#   dotnet tool install --global wix --version 5.0.2
#   wix extension add -g WixToolset.Bal.wixext/5.0.2
#
# Without -ImageDir the package carries no Android image; the service then downloads the
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

$data = "C:\ProgramData\DSA"
if ($ImageDir) {
    if (-not $ImageVersion) { throw "-ImageDir needs -ImageVersion, -ImageMajor and -ImageBuild" }
    $first = [ordered]@{ version = $ImageVersion; android_major = $ImageMajor; build = $ImageBuild }
} else {
    # No image yet: any supported image is newer than this, so the first check downloads one.
    $first = [ordered]@{ version = "none"; android_major = 0; build = 0 }
}
$config = [ordered]@{
    kernel        = "C:\Program Files\DSA\vm\bzImage"
    images_dir    = "$data\images"
    first_image   = $first
    data_dir      = "$data\users"
    log_file      = "$data\logs\dsa-hostd.log"
}
if ($PipeSddl) { $config.pipe_sddl = $PipeSddl }
if ($ListUrl) {
    if ($TrustedKey.Count -eq 0) { throw "-ListUrl needs at least one -TrustedKey" }
    $config.update = [ordered]@{ list_url = $ListUrl; trusted_keys = $TrustedKey }
}
$config | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8NoBOM (Join-Path $stage "dsa-hostd.json")

$msi = Join-Path $out "dsa-$Version.msi"
$setup = Join-Path $out "setup-$Version.exe"
$defines = @("-d", "Version=$Version", "-d", "SourceDir=$stage")
if ($ImageDir) { $defines += @("-d", "ImageDir=$ImageDir", "-d", "ImageVersion=$ImageVersion") }

Push-Location $installer
try {
    wix build -arch x64 @defines dsa.wxs -o $msi
    if ($LASTEXITCODE -ne 0) { throw "building the package failed" }
    wix build -arch x64 -ext WixToolset.Bal.wixext -d "Version=$Version" -d "MsiPath=$msi" bundle.wxs -o $setup
    if ($LASTEXITCODE -ne 0) { throw "building setup.exe failed" }
} finally { Pop-Location }
Write-Host "built $msi and $setup"
