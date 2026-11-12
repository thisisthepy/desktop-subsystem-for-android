<#
.SYNOPSIS
Installs a Development build of DSA on this computer: the installer, the Android image, or
both, whichever the run made.

.DESCRIPTION
The "Development build" workflow (.github/workflows/dev-build.yml) builds DSA for Windows x64
without testing or publishing it, and keeps what it made as workflow artifacts for three
days. This script installs them here:

  1. Finds the run: -Run, or the newest successful Development build on develop that still
     has its artifacts. It checks that the run is this repository's Development build of
     develop and that it passed.
  2. Downloads its artifacts into .scratch\dev-build\<run>\ in this repository.
  3. Checks them before anything is installed. The setup file against SHA256SUMS.txt and
     against the signed program.json beside it, with the release keys the installed service
     trusts (dsa-hostd check-setup, the service's own check of a downloaded update; a DSA
     from before the Development build has no such command, and then the script says so and
     goes on with the SHA-256 alone). The image's files against the sizes and SHA-256 its
     images.json names.
  4. Runs setup silently, when the run made an installer. Setup stops every user's Android
     while it replaces the program, as an update does.
  5. Installs the image, when the run made one, with "dsa android install-image": the
     installed service checks the signed images.json against the keys it trusts, applies the
     update rules and checks every file's hash again, then the image boots on trial at the
     next start of Android (a running Android restarts into it once no app window is open),
     and the service goes back to the image before it if it does not finish booting.

What the run did not make stays as it is installed. Signature checks for normal updates are
not changed: the image is only taken through the same checks.

Installing needs an administrator. Run from a PowerShell started as administrator; otherwise
the script downloads and checks first, then starts itself again as administrator (Windows
asks for consent) for the installation, and shows what it printed.

The image's version ends in -dev.<run number> and the installer's version is
<next release>-dev.<run number>, so neither is taken for a release. The release that comes
next is newer than both, so the computer takes it as an ordinary update.

.PARAMETER Run
The run ID of a Development build (gh run list --workflow dev-build.yml). Without it, the
newest successful one that still has its artifacts.

.PARAMETER Repo
The GitHub repository the run is in.

.PARAMETER Release
The release the "how to go back" lines name.

.EXAMPLE
pwsh scripts/install-dev-build.ps1

.EXAMPLE
pwsh scripts/install-dev-build.ps1 -Run 43120000000
#>
param(
    [long]$Run,
    [string]$Repo = "thisisthepy/desktop-subsystem-for-android",
    [string]$Release = "0.2.3",
    # Set when the script has started itself again as administrator: install only, from the
    # folder the first run downloaded and checked, and write what it prints to this file.
    [string]$ElevatedLog
)
$ErrorActionPreference = "Stop"

$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$artifacts = @("development-installer", "development-image", "development-image-list")

function Test-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    ([Security.Principal.WindowsPrincipal]$identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Invoke-Gh {
    $output = & gh @args
    if ($LASTEXITCODE -ne 0) { throw "gh $($args -join ' ') failed" }
    $output
}

# The names of the run's artifacts that have not expired.
function Get-RunArtifacts([long]$id) {
    $names = Invoke-Gh api "repos/$Repo/actions/runs/$id/artifacts?per_page=100" --jq '[.artifacts[] | select(.expired | not) | .name] | join(",")'
    @(($names -join "") -split "," | Where-Object { $_ -in $artifacts })
}

# The newest successful Development build on develop that still has an artifact.
function Find-Run {
    $runs = Invoke-Gh run list --repo $Repo --workflow dev-build.yml --branch develop --status success --limit 20 --json databaseId --jq '.[].databaseId'
    foreach ($id in $runs) {
        if ((Get-RunArtifacts ([long]$id)).Count -gt 0) { return [long]$id }
    }
    throw "no successful Development build on develop has artifacts left (they are kept for three days); run one: gh workflow run dev-build.yml --repo $Repo --ref develop"
}

# The installed service's program, from its registration.
function Get-InstalledService {
    $service = Get-CimInstance Win32_Service -Filter "Name='DsaHost'" -ErrorAction SilentlyContinue
    if (-not $service) { return $null }
    if ($service.PathName -match '^"([^"]+)"') { $program = $Matches[1] } else { $program = ($service.PathName -split ' ')[0] }
    if (Test-Path $program) { return $program }
    $null
}

function Get-Sha256([string]$path) {
    (Get-FileHash -Algorithm SHA256 $path).Hash.ToLowerInvariant()
}

function Install-Downloaded([string]$dir) {
    $installerDir = Join-Path $dir "development-installer"
    $imageDir = Join-Path $dir "development-image"
    $installed = @()

    if (Test-Path $installerDir) {
        $setup = Get-ChildItem $installerDir -Filter "DSA-v*-win-x64-setup.exe" | Select-Object -First 1
        $log = Join-Path $dir "setup.log"
        Write-Host "Installing $($setup.Name) (every user's Android stops while setup runs)"
        $process = Start-Process -FilePath $setup.FullName -ArgumentList "/quiet", "/norestart", "/log", "`"$log`"" -Wait -PassThru
        # 3010: installed, and Windows asks for a restart.
        if ($process.ExitCode -ne 0 -and $process.ExitCode -ne 3010) {
            throw "setup ended with exit code $($process.ExitCode); $log says why"
        }
        $version = ($setup.Name -replace '^DSA-v', '' -replace '-win-x64-setup\.exe$', '')
        $installed += "DSA $version"
    }

    if (Test-Path $imageDir) {
        $hostd = Get-InstalledService
        if (-not $hostd) { throw "DSA is not installed here; install a Development build that made the installer first (or one with build set to all)" }
        $dsa = Join-Path (Split-Path $hostd) "dsa.exe"
        $help = & $dsa android install-image 2>&1 | Out-String
        if ($help -notmatch "install-image needs the folder") {
            throw "the installed DSA has no 'dsa android install-image'; install a Development build's installer first (run the Development build with build set to all or installer, then this script)"
        }
        Write-Host "Installing the Android image through the service's update checks"
        & $dsa android install-image $imageDir
        if ($LASTEXITCODE -ne 0) { throw "the image was not installed (the reason is above)" }
        $list = Get-Content -Raw (Join-Path $imageDir "images.json") | ConvertFrom-Json
        $installed += "Android $($list.images[0].version)"
    }

    Write-Host ""
    Write-Host "Installed: $($installed -join ', ')."
    if (Test-Path $imageDir) {
        Write-Host "The image boots on trial at the next start of Android; 'dsa android version' shows it, 'dsa android restart' restarts into it now."
    }
    $releaseSetup = "DSA-v$Release-win-x64-setup.exe"
    $back = Join-Path $root ".scratch\dev-build\v$Release"
    $uninstall = if ($setup) { "Start-Process -Wait '$($setup.FullName)' -ArgumentList '/uninstall','/quiet'" } else { "Settings > Apps > Installed apps > Desktop Subsystem for Android > Uninstall" }
    Write-Host @"

To go back to the release v${Release}:
  1. Uninstall DSA. Each user's Android disk stays; the downloaded images and setup files go.
     $uninstall
  2. Install v$Release. Its service then downloads the newest released Android image.
     gh release download v$Release --repo $Repo --pattern '$releaseSetup' --dir '$back'
     Start-Process -Wait '$(Join-Path $back $releaseSetup)' -ArgumentList '/quiet','/norestart'
  A development build is newer than v$Release, so v$Release's setup alone says a newer version
  is installed; the release after v$Release replaces a development build as an ordinary update.
"@
}

if ($ElevatedLog) {
    # The second start, as administrator: install what the first one checked.
    Start-Transcript -Path $ElevatedLog -Force | Out-Null
    try {
        Install-Downloaded (Join-Path $root ".scratch\dev-build\$Run")
    } finally {
        Stop-Transcript | Out-Null
    }
    exit 0
}

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw "the GitHub CLI (gh) is needed to download the run's artifacts" }

if (-not $Run) { $Run = Find-Run }
$info = Invoke-Gh api "repos/$Repo/actions/runs/$Run" | ConvertFrom-Json
if ($info.path -notlike ".github/workflows/dev-build.yml*") { throw "run $Run is not a Development build (it is $($info.path))" }
if ($info.head_branch -ne "develop") { throw "run $Run was made from $($info.head_branch), not develop" }
if ($info.conclusion -ne "success") { throw "run $Run did not pass (it is $($info.status), $($info.conclusion))" }
if ($info.repository.full_name -ne $Repo) { throw "run $Run is in $($info.repository.full_name), not $Repo" }
$present = Get-RunArtifacts $Run
if ($present.Count -eq 0) { throw "run $Run has no artifacts left (they are kept for three days)" }
if (($present -contains "development-image") -ne ($present -contains "development-image-list")) {
    throw "run $Run has only part of its image (the image and its signed list come together)"
}
Write-Host "Development build run $Run of $($info.head_sha.Substring(0, 8)) on develop: $($present -join ', ')"

$dir = Join-Path $root ".scratch\dev-build\$Run"
New-Item -ItemType Directory -Force $dir | Out-Null
foreach ($name in $present) {
    $target = Join-Path $dir $name
    $done = Join-Path $dir ".$name.complete"
    if (Test-Path $done) { continue }
    Remove-Item -Recurse -Force $target -ErrorAction SilentlyContinue
    Write-Host "Downloading $name"
    Invoke-Gh run download $Run --repo $Repo --name $name --dir $target | Out-Null
    New-Item -ItemType File -Force $done | Out-Null
}

# The installer: SHA256SUMS.txt, and the signed program list as the service checks an update.
$installerDir = Join-Path $dir "development-installer"
if (Test-Path $installerDir) {
    $setup = Get-ChildItem $installerDir -Filter "DSA-v*-win-x64-setup.exe" | Select-Object -First 1
    if (-not $setup) { throw "the installer artifact has no DSA-v*-win-x64-setup.exe" }
    $sums = Get-Content (Join-Path $installerDir "SHA256SUMS.txt")
    $pattern = "^([0-9a-f]{64})\s+\*?$([regex]::Escape($setup.Name))$"
    $line = $sums | Where-Object { $_ -match $pattern } | Select-Object -First 1
    if (-not ($line -and $line -match $pattern)) { throw "SHA256SUMS.txt does not name $($setup.Name)" }
    if ((Get-Sha256 $setup.FullName) -ne $Matches[1]) { throw "$($setup.Name) does not match SHA256SUMS.txt" }
    $hostd = Get-InstalledService
    $checked = $false
    if ($hostd) {
        $said = & $hostd check-setup $installerDir 2>&1 | Out-String
        if ($LASTEXITCODE -eq 0) {
            Write-Host "Setup file checked against its signed program list: $($said.Trim())"
            $checked = $true
        } elseif ($said -notmatch "usage: dsa-hostd") {
            throw "the setup file does not pass the service's check: $($said.Trim())"
        }
    }
    if (-not $checked) {
        Write-Warning "the installed DSA cannot check a program list (it is older than the Development build, or not installed), so the setup file was checked against SHA256SUMS.txt from the same run only; later Development builds are checked against the signed program list"
    }
}

# The image: every file it names, at the size and SHA-256 its list gives. The service checks
# the list's signature and the hashes again when it installs it.
$imageDir = Join-Path $dir "development-image"
if (Test-Path $imageDir) {
    Copy-Item (Join-Path $dir "development-image-list\images.json*") $imageDir -Force
    $list = Get-Content -Raw (Join-Path $imageDir "images.json") | ConvertFrom-Json
    if ($list.images.Count -ne 1) { throw "the image list names $($list.images.Count) images, not one" }
    $image = $list.images[0]
    foreach ($file in $image.files) {
        $path = Join-Path $imageDir $file.name
        if (-not (Test-Path $path)) { throw "the image has no $($file.name)" }
        if ((Get-Item $path).Length -ne $file.size) { throw "$($file.name) is not the size the list names" }
        if ((Get-Sha256 $path) -ne $file.sha256.ToLowerInvariant()) { throw "$($file.name) does not match the list's SHA-256" }
    }
    Write-Host "Android image $($image.version) checked against its list ($($image.files.Count) files)"
}

if (Test-Administrator) {
    Install-Downloaded $dir
} else {
    $log = Join-Path $dir "install.log"
    Remove-Item -Force $log -ErrorAction SilentlyContinue
    Write-Host "Installing needs an administrator; starting this script again as administrator (Windows asks for consent)"
    $shell = (Get-Process -Id $PID).Path
    $arguments = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "`"$PSCommandPath`"", "-Run", $Run, "-Repo", $Repo, "-Release", $Release, "-ElevatedLog", "`"$log`"")
    $process = Start-Process -FilePath $shell -ArgumentList $arguments -Verb RunAs -Wait -PassThru
    if (Test-Path $log) { Get-Content $log | Write-Host }
    if ($process.ExitCode -ne 0) { throw "the installation as administrator ended with exit code $($process.ExitCode)" }
}
