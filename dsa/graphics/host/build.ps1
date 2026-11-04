# Builds gfxstream's host renderer (gfxstream_backend.dll) on Windows.
#
# -Platform is the machine the DLL is for: x64 or ARM64. Without it the DLL is for the
# machine that builds it. An ARM64 build on an ARM64 machine is native; on an x64 machine
# it is a cross build with Visual Studio's x64-to-ARM64 tools. On ARM64 the renderer uses
# the device's own Vulkan driver through the Vulkan loader, as on x64.
param(
    [string]$ScratchDir = "$PSScriptRoot/../../../../.scratch",
    [string]$OutDir = "",
    [string]$Config = "Release",
    [string]$Generator = "Ninja",
    [ValidateSet("x64", "ARM64")]
    [string]$Platform = "",
    [string]$Toolset = "ClangCL"
)

$ErrorActionPreference = "Stop"

# The machine's own processor: PROCESSOR_ARCHITEW6432 is set when this PowerShell is
# itself emulated.
$NativeArch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$HostIsArm64 = $NativeArch -eq "ARM64"
if (-not $Platform) {
    $Platform = if ($HostIsArm64) { "ARM64" } else { "x64" }
}
# The Visual Studio environment that compiles for $Platform on this machine.
$VcvarsName = if ($Platform -eq "ARM64") {
    if ($HostIsArm64) { "vcvarsarm64.bat" } else { "vcvarsamd64_arm64.bat" }
} else {
    if ($HostIsArm64) { "vcvarsarm64_amd64.bat" } else { "vcvars64.bat" }
}
Write-Host "Building the host graphics renderer for $Platform on a $NativeArch machine"

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

# Import Visual Studio environment if available
$Vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (Test-Path $Vswhere) {
    $VsPath = & $Vswhere -latest -property installationPath
    if ($VsPath) {
        $Vcvars = Join-Path $VsPath "VC\Auxiliary\Build\$VcvarsName"
        if (Test-Path $Vcvars) {
            Write-Host "Initializing Visual Studio environment from $Vcvars..."
            cmd.exe /c "call `"$Vcvars`" && set" | ForEach-Object {
                if ($_ -match '^(.*?)=(.*)$') {
                    Set-Content "env:$($matches[1])" $matches[2]
                }
            }
        }
    }
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

$HasNinja = [bool](Get-Command ninja.exe -ErrorAction SilentlyContinue)
$HasClangCl = [bool](Get-Command clang-cl.exe -ErrorAction SilentlyContinue)

# Optimised as before, plus a PDB beside the DLL: a crash in the renderer, or in a GPU driver
# it calls, can then be read with the debugger. The PDB is kept apart; nothing ships it.
$ConfigureArgs += @(
    "-DCMAKE_POLICY_DEFAULT_CMP0141=NEW",
    "-DCMAKE_MSVC_DEBUG_INFORMATION_FORMAT=ProgramDatabase",
    "-DCMAKE_SHARED_LINKER_FLAGS_RELEASE=/DEBUG /OPT:REF /OPT:ICF"
)

if ($Generator -eq "Ninja" -and $HasNinja) {
    $ConfigureArgs += @("-G", "Ninja", "-DCMAKE_BUILD_TYPE=$Config")
    if ($HasClangCl) {
        $ConfigureArgs += @("-DCMAKE_C_COMPILER=clang-cl", "-DCMAKE_CXX_COMPILER=clang-cl")
        # clang-cl compiles for the machine it runs on unless told otherwise, and Visual
        # Studio's clang-cl on an x64 machine is an x64 program.
        $ClangTarget = if ($Platform -eq "ARM64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" }
        $ConfigureArgs += @("-DCMAKE_C_COMPILER_TARGET=$ClangTarget", "-DCMAKE_CXX_COMPILER_TARGET=$ClangTarget")
    }
    if ($Platform -eq "ARM64" -and -not $HostIsArm64) {
        $ConfigureArgs += @("-DCMAKE_SYSTEM_NAME=Windows", "-DCMAKE_SYSTEM_PROCESSOR=ARM64")
    }
} elseif ($Generator -match "Visual Studio") {
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

$PdbOutput = Join-Path $DistDir "symbols/gfxstream_backend.pdb"
$Pdb = Get-ChildItem -Path $OutDir -Recurse -Filter "gfxstream_backend.pdb" -ErrorAction SilentlyContinue |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($Pdb) {
    New-Item -ItemType Directory -Path (Join-Path $DistDir "symbols") -Force | Out-Null
    Copy-Item $Pdb.FullName -Destination $PdbOutput -Force
    Write-Host "Kept its symbols: $PdbOutput"
} else {
    Write-Warning "no gfxstream_backend.pdb was produced; a renderer crash will show no function names"
}

Write-Host "Built host graphics renderer: $DllOutput"
