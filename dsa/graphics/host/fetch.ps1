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

# 1. Fetch gfxstream. A new checkout fetches the pinned commit alone: no build reads the
# history, and all of it took a minute or more to download on every CI run.
$GfxstreamDepth = @()
if (-not (Test-Path "$GfxstreamDir/.git")) {
    Write-Host "Fetching gfxstream..."
    git init -q $GfxstreamDir
    git -C $GfxstreamDir remote add origin https://android.googlesource.com/platform/hardware/google/gfxstream
    $GfxstreamDepth = @("--depth", "1")
}
git -C $GfxstreamDir fetch @GfxstreamDepth --no-tags origin $GfxstreamRevision
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
# its device again, and a bad command ends only that guest's stream. Without 0016 a fence or
# semaphore whose work finished but sat in a device's garbage for five seconds, because nothing
# used that device meanwhile, is leaked and logged as pending for a negative time. Without 0017
# every first use of a gralloc buffer is logged as an error, since each guest process opens a
# shared ColorBuffer before it knows whether another made it. Without 0018 an image release the
# GPU has not finished in three seconds hands its still pending fence back to the pool, the
# next release resets it and submits with it again while the GPU may still signal it, and
# nothing checks the renderer's device. Without 0019 a YUV gralloc buffer a guest CPU writes (a
# video decoder's pictures, a camera's frames) gets an RGBA ColorBuffer the guest cannot copy
# its planes into, so videos show their sound and a green picture. Without 0020 every read back
# of host-visible memory sends all of it, changed or not: the guest reads its mapped memory back
# at each fence, so every frame of every app waited for tens of megabytes to cross the socket.
# Without 0021 the renderer makes its lost device again while holding its lock, so every guest
# call that touches a ColorBuffer waits for the GPU to come back from its reset (13 s on an
# Intel Arc), and Android ends the app whose input went unanswered. Without 0022 a ColorBuffer
# read back or written whole is one GPU job however large the window, 2880x2048 and more for a
# window maximized on a 200% monitor. Without 0023 the renderer's memory count follows neither the
# device it makes again after a GPU reset nor any guest's device, so after a reset it reads 0.
# Without 0024 a fence or semaphore whose operation never looked finished (its fence reset by the
# guest before it was polled) is leaked, and a device destroyed with such garbage leaks it too.
# Without 0025 the images and memory of destroyed ColorBuffers wait for the next ColorBuffer to be
# made or destroyed, so those of the window that closed last stay.
# A patch that does not apply stops the fetch.
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
    @(Get-ChildItem -Path $PatchDir -Filter "0015-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0016-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0017-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0018-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0019-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0020-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0021-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0022-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0023-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0024-*.patch") +
    @(Get-ChildItem -Path $PatchDir -Filter "0025-*.patch")
foreach ($Patch in $Patches) {
    Write-Host "Applying patch: $($Patch.Name)"
    git -C $GfxstreamDir apply --recount $Patch.FullName
    if ($LASTEXITCODE -ne 0) {
        throw "patch $($Patch.Name) does not apply to gfxstream $GfxstreamRevision"
    }
}

# 2. Fetch aemu
$AemuDepth = @()
if (-not (Test-Path "$AemuDir/.git")) {
    Write-Host "Fetching aemu..."
    git init -q $AemuDir
    git -C $AemuDir remote add origin https://android.googlesource.com/platform/hardware/google/aemu
    $AemuDepth = @("--depth", "1")
}
git -C $AemuDir fetch @AemuDepth --no-tags origin $AemuRevision
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
