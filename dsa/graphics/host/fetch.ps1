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

# The repository root: this script is in dsa/graphics/host.
$RepoRoot = (Resolve-Path "$PSScriptRoot/../../..").Path
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

# Apply the checked-in gfxstream patches. 0002, 0003 and 0008 patch Mesa for the guest and do not
# belong to this tree. The host renderer cannot build without 0004, and without 0005 it
# aborts when two guest processes create one shared ColorBuffer and refuses more than
# eleven task windows. Without 0006 a Vulkan call naming an image, buffer or memory the
# host does not have reaches the GPU driver or aborts, and takes the renderer down with
# every app on it. Without 0007 the renderer cannot be asked for the GPU that drives the
# display. Without 0009 every ColorBuffer the guest destroys waits, under the renderer's
# locks, for all GPU work on the queue to finish, and the guest's buffer allocations queue
# up behind it for seconds while a window is resized. Without 0011 the renderer's
# spin-wait has no processor hint on ARM64, and the lavapipe test driver is looked for under
# its x86_64 name. Without 0012 a host with no usable Vulkan driver crashes the renderer
# while it falls back from Vulkan. Without 0013 the renderer on macOS never enables
# Vulkan portability, so the loader hides MoltenVK and Vulkan does not start. Without 0014
# a renderer that asks for Vulkan only and cannot start it crashes instead of failing. Without 0015
# a GPU reset, or one guest's command the renderer cannot carry out, ends the window manager and
# every window with it; with it the guest's calls get VK_ERROR_DEVICE_LOST, the renderer makes
# its device again, and a bad command ends only that guest's stream. A patch that does not apply
# stops the fetch.
$PatchDir = Join-Path $RepoRoot "dsa/graphics/patches"
$Patches = @(Get-ChildItem -Path $PatchDir -Filter "0001-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0004-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0005-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0006-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0007-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0009-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0010-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0011-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0012-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0013-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0014-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0015-*.patch")
foreach ($Patch in $Patches) {
    Write-Host "Applying patch: $($Patch.Name)"
    git -C $GfxstreamDir apply --recount $Patch.FullName
    if ($LASTEXITCODE -ne 0) {
        throw "patch $($Patch.Name) does not apply to gfxstream $GfxstreamRevision"
    }
}

# 2. Fetch aemu
if (-not (Test-Path "$AemuDir/.git")) {
    Write-Host "Cloning aemu..."
    git clone https://android.googlesource.com/platform/hardware/google/aemu $AemuDir
}
git -C $AemuDir fetch --no-tags origin $AemuRevision
git -C $AemuDir checkout --detach $AemuRevision
git -C $AemuDir reset --hard $AemuRevision
git -C $AemuDir clean -fdx

# Apply the checked-in aemu patches (patches/aemu). Without 0001 aemu does not compile for
# ARM64 Windows: it calls the x86-only _mm_pause there. Without 0002 every fatal error ends the
# process; with it one on a thread that serves a guest ends only what that thread serves.
foreach ($Patch in @(Get-ChildItem -Path (Join-Path $PatchDir "aemu") -Filter "*.patch" | Sort-Object Name)) {
    Write-Host "Applying aemu patch: $($Patch.Name)"
    git -C $AemuDir apply --recount $Patch.FullName
    if ($LASTEXITCODE -ne 0) {
        throw "patch aemu/$($Patch.Name) does not apply to aemu $AemuRevision"
    }
}

# 3. Fetch angle (sparse include only)
if (-not (Test-Path "$AngleDir/.git")) {
    Write-Host "Cloning angle (include only)..."
    git clone --depth=1 --filter=blob:none --sparse https://android.googlesource.com/platform/external/angle $AngleDir
}
git -C $AngleDir sparse-checkout set include

Write-Host "All host dependencies fetched successfully in $ScratchPath."
