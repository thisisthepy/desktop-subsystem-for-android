# Builds gfxstream's host renderer (gfxstream_backend.dll) on Windows.
param(
    [string]$ScratchDir = "$PSScriptRoot/../../../../.scratch",
    [string]$OutDir = "",
    [string]$Config = "Release",
    [string]$Generator = "Visual Studio 17 2022",
    [string]$Platform = "x64",
    [string]$Toolset = "ClangCL"
)

$ErrorActionPreference = "Stop"

$ScratchPath = (Resolve-Path -Path $ScratchDir -ErrorAction SilentlyContinue)
if (-not $ScratchPath) {
    New-Item -ItemType Directory -Path $ScratchDir -Force | Out-Null
    $ScratchPath = (Resolve-Path -Path $ScratchDir).Path
} else {
    $ScratchPath = $ScratchPath.Path
}

if (-not $OutDir) {
    $OutDir = Join-Path $ScratchPath "build-host-gfxstream"
}

# Ensure source dependencies exist
$GfxstreamDir = Join-Path $ScratchPath "gfxstream"
$AemuDir = Join-Path $ScratchPath "aemu"
$AngleDir = Join-Path $ScratchPath "angle"

if ((-not (Test-Path "$GfxstreamDir/CMakeLists.txt")) -or
    (-not (Test-Path "$AemuDir/CMakeLists.txt")) -or
    (-not (Test-Path "$AngleDir/include"))) {
    Write-Host "Fetching host graphics dependencies into $ScratchPath..."
    & "$PSScriptRoot/fetch.ps1" -ScratchDir $ScratchPath
}

$CMakeBin = "cmake"
if (-not (Get-Command $CMakeBin -ErrorAction SilentlyContinue)) {
    # Check Android SDK cmake candidates
    $Candidates = Get-ChildItem "$HOME/AppData/Local/Android/Sdk/cmake" -Recurse -Filter "cmake.exe" -ErrorAction SilentlyContinue
    if ($Candidates) {
        $CMakeBin = $Candidates[0].FullName
    } else {
        throw "cmake executable not found in PATH or Android SDK."
    }
}

$DistDir = Join-Path $OutDir "dist"
$ConfigureArgs = @(
    "-B", $OutDir,
    "-S", $PSScriptRoot,
    "-DSCRATCH_DIR=$ScratchPath",
    "-DDIST_DIR=$DistDir"
)

if ($Generator -match "Visual Studio") {
    $ConfigureArgs += @("-G", $Generator, "-A", $Platform)
    if ($Toolset) {
        $ConfigureArgs += @("-T", $Toolset)
    }
} elseif ($Generator) {
    $ConfigureArgs += @("-G", $Generator)
}

Write-Host "Configuring host graphics renderer with CMake..."
& $CMakeBin @ConfigureArgs
if ($LASTEXITCODE -ne 0) {
    throw "CMake configuration failed with exit code $LASTEXITCODE."
}

Write-Host "Building gfxstream_backend target ($Config)..."
& $CMakeBin --build $OutDir --config $Config --target dist_host_gfxstream
if ($LASTEXITCODE -ne 0) {
    throw "CMake build failed with exit code $LASTEXITCODE."
}

$DllOutput = Join-Path $DistDir "bin/gfxstream_backend.dll"
if (-not (Test-Path $DllOutput)) {
    # Check under Release or OutDir directly
    $Fallbacks = Get-ChildItem -Path $OutDir -Recurse -Filter "gfxstream_backend.dll" -ErrorAction SilentlyContinue
    if ($Fallbacks) {
        New-Item -ItemType Directory -Path (Join-Path $DistDir "bin") -Force | Out-Null
        Copy-Item $Fallbacks[0].FullName -Destination $DllOutput -Force
    } else {
        throw "Build finished but gfxstream_backend.dll was not found."
    }
}

Write-Host "Built host graphics renderer: $DllOutput"
