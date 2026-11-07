# Compile-only check for background agents on the owner's Windows machine.
#
# Runs `cargo check`, `cargo clippy` or `dotnet build` on four cores at low priority, and only
# one at a time across every worktree, so the owner's own work keeps the rest of the machine.
# Nothing else that builds or tests may run on this machine (AGENTS.md, Background agents).
#
#   pwsh scripts/compile-check.ps1 cargo-check  dsa/host/core/Cargo.toml
#   pwsh scripts/compile-check.ps1 cargo-clippy dsa/host/core/Cargo.toml -Extra '--all-targets','--','-D','warnings'
#   pwsh scripts/compile-check.ps1 dotnet-build dsa/host/platform/windows/settings/DsaSettings.sln
param(
    [Parameter(Mandatory, Position = 0)]
    [ValidateSet('cargo-check', 'cargo-clippy', 'dotnet-build')]
    [string]$Kind,
    [Parameter(Mandatory, Position = 1)]
    [string]$Path,
    [string[]]$Extra = @()
)
$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $Path)) { throw "no such manifest or project: $Path" }
$target = (Resolve-Path -LiteralPath $Path).Path

# One lock for all worktrees: it lives in the main checkout's .scratch, found through git's
# common directory, which every worktree shares.
$common = (git -C (Split-Path $target) rev-parse --path-format=absolute --git-common-dir).Trim()
$scratch = Join-Path (Split-Path $common) '.scratch'
New-Item -ItemType Directory -Force -Path $scratch | Out-Null
$lockPath = Join-Path $scratch 'compile-check.lock'

$lock = $null
$waited = $false
while (-not $lock) {
    try {
        $lock = [System.IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None')
    } catch [System.IO.IOException] {
        if (-not $waited) { Write-Host 'Another compile check is running; waiting for it.'; $waited = $true }
        Start-Sleep -Seconds 5
    }
}

try {
    $quoted = ($Extra | ForEach-Object { if ($_ -match '\s') { "`"$_`"" } else { $_ } }) -join ' '
    switch ($Kind) {
        'cargo-check'  { $command = "cargo check -j 4 --manifest-path `"$target`" $quoted" }
        'cargo-clippy' { $command = "cargo clippy -j 4 --manifest-path `"$target`" $quoted" }
        'dotnet-build' { $command = "dotnet build `"$target`" -m:4 -nologo $quoted" }
    }
    # `start /low` gives the tool and every compiler it starts the Idle priority class, so
    # anything the owner runs is scheduled first.
    $process = Start-Process -FilePath 'cmd.exe' -ArgumentList "/d /c start `"`" /low /wait /b $command" `
        -NoNewWindow -Wait -PassThru
    exit $process.ExitCode
} finally {
    $lock.Dispose()
}
