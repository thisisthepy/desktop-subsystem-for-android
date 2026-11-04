<#
.SYNOPSIS
Measures the signed-in user's Android on this PC: idle memory, memory returned after a stop,
idle CPU, and how long an app's window takes to show.

.DESCRIPTION
Run it from a PowerShell 7 prompt as the user whose Android is measured. Some numbers need
an elevated prompt: the VM's own process belongs to Hyper-V, and only an administrator may
read its CPU time and the service's memory. What cannot be read is printed as such, never
guessed.

Each part prints its numbers and the environment they were taken in (RAM, CPU, Windows
build), so they can be recorded as they are. -Json prints the same as one JSON object.

  -Part memory   Waits until Android is Dozing and has run -SettleMinutes (10) since it
                 started, then reads `dsa memory`: the guest's total PSS, the VM's working
                 set, the window processes and the service. The sum is compared with 800 MB.
  -Part cpu      Samples the CPU time of the VM and of the DSA processes for -CpuMinutes
                 (10) while Android is Dozing, as a percentage of one logical core.
  -Part window   For each -Package, with Android Dozing: the time from `dsa launch` to the
                 app's window being visible. With -Cold, it also stops Android first and
                 measures the start to Active, then the launch.
  -Part return   Opens each -Package, stops Android, and measures how long the VM's memory
                 takes to go back to the host, and how much came back.
  -Part all      memory, cpu, window and return, in that order (the default).

.EXAMPLE
pwsh -File measure.ps1 -Part memory
pwsh -File measure.ps1 -Part window -Package com.android.settings -Cold
#>
[CmdletBinding()]
param(
    [ValidateSet('all', 'memory', 'cpu', 'window', 'return')]
    [string]$Part = 'all',
    [string]$Dsa = 'dsa',
    [string[]]$Package = @('com.android.settings'),
    [double]$SettleMinutes = 10,
    [double]$CpuMinutes = 10,
    [int]$Repeat = 3,
    [switch]$Cold,
    [double]$ReturnTimeoutSeconds = 120,
    [switch]$Json,
    # Defines the functions and returns, for the tests.
    [switch]$FunctionsOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# The memory target for a dozing Android, guest VM and companion processes together, in MB.
$IdleMemoryTargetMb = 800

function ConvertTo-Mib([double]$Kb) { [math]::Round($Kb / 1024, 1) }

# The CPU used between two samples as a percentage of one logical core.
function Get-CorePercent([double]$CpuSecondsBefore, [double]$CpuSecondsAfter, [double]$WallSeconds) {
    if ($WallSeconds -le 0) { return $null }
    [math]::Round(($CpuSecondsAfter - $CpuSecondsBefore) / $WallSeconds * 100, 3)
}

# Sums what `dsa memory --json` reports and the service's working set into the idle memory
# figures. `$ServiceKb` is $null when the service could not be read.
function Get-IdleMemorySummary($Memory, $ServiceKb) {
    $roles = @{}
    foreach ($p in @($Memory.host)) {
        if (-not $roles.ContainsKey($p.role)) { $roles[$p.role] = 0 }
        $roles[$p.role] += [double]$p.working_set_kb
    }
    $vmKb = if ($roles.ContainsKey('vm')) { $roles['vm'] } else { $null }
    $windowsKb = 0
    foreach ($role in $roles.Keys) { if ($role -ne 'vm') { $windowsKb += $roles[$role] } }
    $totalKb = [double]$Memory.host_total_kb
    if ($null -ne $ServiceKb) { $totalKb += $ServiceKb }
    $guestPssKb = if ($Memory.guest) { [double]$Memory.guest.total_pss_kb } else { $null }
    [ordered]@{
        guest_total_pss_mib = if ($null -ne $guestPssKb) { ConvertTo-Mib $guestPssKb } else { $null }
        vm_working_set_mib = if ($null -ne $vmKb) { ConvertTo-Mib $vmKb } else { $null }
        window_processes_mib = ConvertTo-Mib $windowsKb
        service_working_set_mib = if ($null -ne $ServiceKb) { ConvertTo-Mib $ServiceKb } else { $null }
        total_mb = [math]::Round($totalKb * 1024 / 1e6, 1)
        target_mb = $IdleMemoryTargetMb
        complete = ($null -ne $vmKb) -and ($null -ne $ServiceKb)
        within_target = ($totalKb * 1024 / 1e6) -le $IdleMemoryTargetMb
    }
}

# The middle of a list of numbers.
function Get-Median([double[]]$Values) {
    if ($Values.Count -eq 0) { return $null }
    $sorted = $Values | Sort-Object
    $mid = [int][math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2) { return $sorted[$mid] }
    ($sorted[$mid - 1] + $sorted[$mid]) / 2
}

if ($FunctionsOnly) { return }

function Invoke-Dsa {
    $output = & $Dsa @args 2>&1
    if ($LASTEXITCODE -ne 0) { throw "dsa $($args -join ' ') failed: $output" }
    ($output | Where-Object { $_ -is [string] }) -join "`n"
}

function Get-DsaJson { Invoke-Dsa @args --json | ConvertFrom-Json }

function Get-State { (Get-DsaJson status).state }

function Get-Environment {
    $os = Get-CimInstance Win32_OperatingSystem
    $cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
    [ordered]@{
        ram_gib = [math]::Round($os.TotalVisibleMemorySize / 1MB, 1)
        cpu = $cpu.Name.Trim()
        logical_cores = [Environment]::ProcessorCount
        windows_build = "$($os.Version) ($($os.Caption))"
        elevated = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
            [Security.Principal.WindowsBuiltInRole]::Administrator)
        taken_at = (Get-Date).ToString('s')
    }
}

function Wait-State([string]$State, [double]$TimeoutSeconds) {
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if ((Get-State) -eq $State) { return $true }
        Start-Sleep -Milliseconds 500
    }
    $false
}

# Every window of the user's Android apps: the processes run from %LOCALAPPDATA%\DSA\apps.
function Get-AppWindowProcesses([string]$PackageName = '*') {
    $root = Join-Path $env:LOCALAPPDATA 'DSA\apps'
    Get-Process | Where-Object {
        try { $_.Path -and $_.Path -like (Join-Path $root "$PackageName\*") } catch { $false }
    }
}

function Wait-Dozing {
    if ((Get-State) -eq 'Stopped') {
        Write-Host 'starting Android'
        Invoke-Dsa start --wait | Out-Null
    }
    foreach ($process in Get-AppWindowProcesses) {
        Write-Host "closing $($process.ProcessName)"
        $null = $process.CloseMainWindow()
    }
    Write-Host 'waiting for Android to be Dozing'
    if (-not (Wait-State 'Dozing' 600)) { throw "Android did not reach Dozing in 10 minutes (it is $(Get-State))" }
}

function Get-ProcessSafe([int]$Id) {
    try { Get-Process -Id $Id } catch { $null }
}

function Get-ServiceProcess {
    try { Get-Process -Name 'dsa-hostd' | Select-Object -First 1 } catch { $null }
}

# The CPU seconds used so far by the VM and by the DSA processes on the host.
function Get-CpuSeconds($VmPid) {
    $vm = $null
    if ($VmPid) {
        $p = Get-ProcessSafe $VmPid
        if ($p -and $null -ne $p.TotalProcessorTime) { $vm = $p.TotalProcessorTime.TotalSeconds }
    }
    $hostSeconds = 0.0
    $readable = $true
    $companions = @(Get-AppWindowProcesses) + @(Get-Process -Name 'dsa-wm', 'dsa-app-window' -ErrorAction SilentlyContinue)
    $service = Get-ServiceProcess
    if ($service) { $companions += $service }
    foreach ($p in $companions) {
        if ($null -eq $p.TotalProcessorTime) { $readable = $false; continue }
        $hostSeconds += $p.TotalProcessorTime.TotalSeconds
    }
    if (-not $service -or $null -eq $service.TotalProcessorTime) { $readable = $false }
    [ordered]@{ vm = $vm; companions = $hostSeconds; companions_complete = $readable }
}

function Measure-IdleMemory {
    Wait-Dozing
    # The memory is read once Android has run long enough to settle.
    # The VM's process started with Android; when its start time cannot be read, the whole
    # settling time is waited.
    $waitSeconds = $SettleMinutes * 60
    $vm = @((Get-DsaJson memory).host) | Where-Object { $_.role -eq 'vm' } | Select-Object -First 1
    $vmProcess = if ($vm) { Get-ProcessSafe $vm.pid } else { $null }
    $startTime = if ($vmProcess) { try { $vmProcess.StartTime } catch { $null } } else { $null }
    if ($startTime) { $waitSeconds = [math]::Max(0, $SettleMinutes * 60 - ((Get-Date) - $startTime).TotalSeconds) }
    if ($waitSeconds -gt 0) {
        Write-Host "waiting $([math]::Round($waitSeconds / 60, 1)) minutes for Android to settle"
        Start-Sleep -Seconds $waitSeconds
    }
    $state = Get-State
    $memory = Get-DsaJson memory
    $service = Get-ServiceProcess
    $serviceKb = if ($service -and $service.WorkingSet64) { $service.WorkingSet64 / 1KB } else { $null }
    $summary = Get-IdleMemorySummary $memory $serviceKb
    $summary['state'] = $state
    $summary
}

function Measure-IdleCpu {
    Wait-Dozing
    $memory = Get-DsaJson memory
    $vmPid = (@($memory.host) | Where-Object { $_.role -eq 'vm' } | Select-Object -First 1).pid
    $before = Get-CpuSeconds $vmPid
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $notDozing = 0
    $samples = 0
    Write-Host "sampling the CPU for $CpuMinutes minutes"
    while ($clock.Elapsed.TotalMinutes -lt $CpuMinutes) {
        Start-Sleep -Seconds 10
        $samples++
        if ((Get-State) -ne 'Dozing') { $notDozing++ }
    }
    $after = Get-CpuSeconds $vmPid
    $wall = $clock.Elapsed.TotalSeconds
    $vmPercent = if ($null -ne $before.vm -and $null -ne $after.vm) { Get-CorePercent $before.vm $after.vm $wall } else { $null }
    $hostPercent = Get-CorePercent $before.companions $after.companions $wall
    [ordered]@{
        minutes = [math]::Round($wall / 60, 1)
        vm_percent_of_one_core = $vmPercent
        companions_percent_of_one_core = $hostPercent
        total_percent_of_one_core = if ($null -ne $vmPercent) { $vmPercent + $hostPercent } else { $null }
        complete = ($null -ne $vmPercent) -and $after.companions_complete
        samples_not_dozing = "$notDozing of $samples"
    }
}

function Measure-WindowShow([string]$PackageName) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    Invoke-Dsa launch $PackageName | Out-Null
    $launched = $clock.Elapsed.TotalMilliseconds
    while ($clock.Elapsed.TotalSeconds -lt 30) {
        $window = Get-AppWindowProcesses $PackageName | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
        if ($window) {
            $shown = $clock.Elapsed.TotalMilliseconds
            $state = Get-State
            $null = $window.CloseMainWindow()
            return [ordered]@{ launch_ms = [math]::Round($launched); window_ms = [math]::Round($shown); state_after = $state }
        }
        Start-Sleep -Milliseconds 10
    }
    throw "no window of $PackageName was shown within 30 seconds"
}

function Measure-Windows {
    $results = [ordered]@{}
    foreach ($name in $Package) {
        $warm = @()
        for ($i = 0; $i -lt $Repeat; $i++) {
            Wait-Dozing
            $warm += [double](Measure-WindowShow $name).window_ms
        }
        $entry = [ordered]@{ from_dozing_ms = $warm; from_dozing_median_ms = Get-Median $warm }
        if ($Cold) {
            Write-Host 'stopping Android for a cold start'
            Invoke-Dsa stop --wait | Out-Null
            $clock = [Diagnostics.Stopwatch]::StartNew()
            Invoke-Dsa start --wait | Out-Null
            $entry['cold_start_to_active_ms'] = [math]::Round($clock.Elapsed.TotalMilliseconds)
            $entry['cold_first_window_ms'] = (Measure-WindowShow $name).window_ms
        }
        $results[$name] = $entry
    }
    $results
}

function Get-AvailableMib { [math]::Round((Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory / 1KB, 1) }

function Measure-MemoryReturn {
    if ((Get-State) -eq 'Stopped') { Invoke-Dsa start --wait | Out-Null }
    foreach ($name in $Package) {
        Invoke-Dsa launch $name | Out-Null
        Start-Sleep -Seconds 5
    }
    $memory = Get-DsaJson memory
    $vm = @($memory.host) | Where-Object { $_.role -eq 'vm' } | Select-Object -First 1
    $availableBefore = Get-AvailableMib
    $clock = [Diagnostics.Stopwatch]::StartNew()
    Invoke-Dsa stop --wait | Out-Null
    $stopped = $clock.Elapsed.TotalMilliseconds
    $gone = $null
    while ($clock.Elapsed.TotalSeconds -lt $ReturnTimeoutSeconds) {
        if (-not $vm -or -not (Get-ProcessSafe $vm.pid)) { $gone = $clock.Elapsed.TotalMilliseconds; break }
        Start-Sleep -Milliseconds 250
    }
    Start-Sleep -Seconds 2
    [ordered]@{
        apps_opened = $Package.Count
        vm_working_set_before_mib = if ($vm) { ConvertTo-Mib $vm.working_set_kb } else { $null }
        stop_ms = [math]::Round($stopped)
        vm_process_gone_ms = if ($null -ne $gone) { [math]::Round($gone) } else { $null }
        host_available_before_mib = $availableBefore
        host_available_after_mib = Get-AvailableMib
        returned = $null -ne $gone
    }
}

$report = [ordered]@{ environment = Get-Environment }
if ($Part -in 'all', 'memory') { $report['idle_memory'] = Measure-IdleMemory }
if ($Part -in 'all', 'cpu') { $report['idle_cpu'] = Measure-IdleCpu }
if ($Part -in 'all', 'window') { $report['window_show'] = Measure-Windows }
if ($Part -in 'all', 'return') { $report['memory_return'] = Measure-MemoryReturn }

if ($Json) {
    $report | ConvertTo-Json -Depth 6
} else {
    foreach ($key in $report.Keys) {
        Write-Output "== $key"
        Write-Output ($report[$key] | ConvertTo-Json -Depth 5)
    }
}
