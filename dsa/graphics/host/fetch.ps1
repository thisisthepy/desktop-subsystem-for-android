# Fetches the pinned gfxstream source and host build dependencies on Windows.
param(
    [string]$ScratchDir = "$PSScriptRoot/../../../../.scratch"
)

$ErrorActionPreference = "Stop"

$ScratchPath = (Resolve-Path -Path $ScratchDir -ErrorAction SilentlyContinue)
if (-not $ScratchPath) {
    New-Item -ItemType Directory -Path $ScratchDir -Force | Out-Null
    $ScratchPath = (Resolve-Path -Path $ScratchDir).Path
} else {
    $ScratchPath = $ScratchPath.Path
}

$RepoRoot = (Resolve-Path "$PSScriptRoot/../../../..").Path
$GfxstreamDir = Join-Path $ScratchPath "gfxstream"
$AemuDir = Join-Path $ScratchPath "aemu"
$AngleDir = Join-Path $ScratchPath "angle"

$GfxstreamRevision = "d047a57228332d995d36600792fa9ccc26cf8ae6"
$AemuRevision = "3ff20aed307f55bc4ae865373d07bf76c9efa43c"

# 1. Fetch gfxstream
if (-not (Test-Path "$GfxstreamDir/.git")) {
    Write-Host "Cloning gfxstream..."
    git clone https://android.googlesource.com/platform/hardware/google/gfxstream $GfxstreamDir
}
git -C $GfxstreamDir fetch --no-tags origin $GfxstreamRevision
git -C $GfxstreamDir checkout --detach $GfxstreamRevision
git -C $GfxstreamDir reset --hard $GfxstreamRevision
git -C $GfxstreamDir clean -fdx

# Apply checked-in patches if present
$PatchDir = Join-Path $RepoRoot "dsa/graphics/patches"
$Patches = Get-ChildItem -Path $PatchDir -Filter "*.patch" -ErrorAction SilentlyContinue
foreach ($Patch in $Patches) {
    Write-Host "Applying patch: $($Patch.Name)"
    git -C $GfxstreamDir apply --recount $Patch.FullName
}

# 2. Fetch aemu
if (-not (Test-Path "$AemuDir/.git")) {
    Write-Host "Cloning aemu..."
    git clone https://android.googlesource.com/platform/hardware/google/aemu $AemuDir
}
git -C $AemuDir fetch --no-tags origin $AemuRevision
git -C $AemuDir checkout --detach $AemuRevision
git -C $AemuDir reset --hard $AemuRevision

# 3. Fetch angle (sparse include only)
if (-not (Test-Path "$AngleDir/.git")) {
    Write-Host "Cloning angle (include only)..."
    git clone --depth=1 --filter=blob:none --sparse https://android.googlesource.com/platform/external/angle $AngleDir
}
git -C $AngleDir sparse-checkout set include

Write-Host "All host dependencies fetched successfully in $ScratchPath."
