<#
.SYNOPSIS
Measures the signed-in user's Android on this PC: idle memory, memory returned after a stop,
idle CPU, how long an app's window takes to show its first frame, and how long a click takes
to show.

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
  -Part window   For each -Package, with Android Dozing: the time from the launch request
                 to the app's new window presenting its first frame and to that frame being
                 shown on the screen, as `dsa launch --wait-first-frame` reads them from the
                 window. With -Cold, it also stops Android first and measures the start to
                 Active, then the launch, and their sum.
  -Part click    Opens the first -Package and presses at -TapX, -TapY (Android display
                 pixels in its task) -Repeat times with `dsa tap`, which sends the press
                 through the local API: the mouse never moves and no other window gets
                 input. Then reads the input trace with scripts/input-trace.py and reports,
                 for each press, the time to the first app frame, to the first visible change
                 around the press point, and to the next screen. The input trace must be on
                 (`dsa settings set input-trace on` as an administrator, then reopen the
                 app). With -Relaunch the app is stopped and opened again before each press,
                 so a press that opens another screen is measured from the same screen.
  -Part return   Opens each -Package, stops Android, and measures how long the VM's memory
                 takes to go back to the host, and how much came back.
  -Part open     Opens the first -Package, leaves it untouched for a minute, then reads each
                 DSA process's private working set and `dsa memory` (with the guest kernel's
                 breakdown). The sum is compared with the near-term 400 MiB target.
  -Part close    Opens the first -Package, closes its window, and reads the VM's working set
                 every second for 30 seconds: how much went back to the host, and how fast.
  -Part grow     Reads the service log's "memory:" lines and the guest's total memory while
                 each -Package is opened in turn, then closed: whether the VM grew when the
                 guest ran short and shrank back once it was calm. Needs an elevated prompt
                 to read the service log.
  -Part all      memory, cpu, window and return, in that order (the default).

.EXAMPLE
pwsh -File measure.ps1 -Part memory
pwsh -File measure.ps1 -Part window -Package com.android.settings -Cold
pwsh -File measure.ps1 -Part click -Package com.android.settings -TapX 300 -TapY 400 -Repeat 10 -Relaunch
#>
[CmdletBinding()]
param(
    [ValidateSet('all', 'memory', 'cpu', 'window', 'return', 'open', 'close', 'click', 'grow')]
    [string]$Part = 'all',
    [string]$Dsa = 'dsa',
    [string[]]$Package = @('com.android.settings'),
    [double]$SettleMinutes = 10,
    [double]$CpuMinutes = 10,
    [int]$Repeat = 3,
    [switch]$Cold,
    [double]$ReturnTimeoutSeconds = 120,
    [switch]$Json,
    # Where -Part click presses, in Android display pixels from the task's top left corner.
    [int]$TapX = 200,
    [int]$TapY = 300,
    # How long -Part click leaves between presses, in milliseconds.
    [int]$TapGapMs = 2000,
    [switch]$Relaunch,
    # The input trace merge tool -Part click runs.
    [string]$InputTrace = (Join-Path $PSScriptRoot '../../../../../scripts/input-trace.py'),
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
    $mappedKb = 0
    foreach ($p in @($Memory.host)) {
        if (-not $roles.ContainsKey($p.role)) { $roles[$p.role] = 0 }
        $roles[$p.role] += [double]$p.working_set_kb
        # Pages of the disk files the VM maps directly, part of its working set; absent from a
        # service that does not count them.
        $mapped = $p.PSObject.Properties['mapped_kb']
        if ($null -ne $mapped -and $null -ne $mapped.Value) { $mappedKb += [double]$mapped.Value }
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
        vm_mapped_disk_pages_mib = ConvertTo-Mib $mappedKb
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

# The host's monotonic clock in milliseconds, as every DSA process logs it: the performance
# counter's ticks over its frequency.
function ConvertTo-HostMs([long]$Ticks, [long]$Frequency) {
    if ($Ticks -le 0 -or $Frequency -le 0) { return 0.0 }
    [double]([decimal]$Ticks * 1000 / [decimal]$Frequency)
}

# What `dsa launch --wait-first-frame --json` printed, as one launch's numbers. The shown
# time stands in for the first frame when it is known, since that is when the user sees it.
function Get-FirstFrameSummary($Frame, [double]$CommandMs) {
    [ordered]@{
        request_ms = $Frame.request_ms
        first_frame_ms = $Frame.first_frame_ms
        first_shown_ms = $Frame.first_shown_ms
        seen_ms = if ($null -ne $Frame.first_shown_ms) { $Frame.first_shown_ms } else { $Frame.first_frame_ms }
        command_ms = [math]::Round($CommandMs)
    }
}

# The click numbers scripts/input-trace.py --json printed: each press's three times and the
# median and 90th percentile of each.
function Get-ClickSummary($Trace) {
    $pick = { param($name) if ($Trace.$name) { [ordered]@{ count = $Trace.$name.count; median_ms = $Trace.$name.median; p90_ms = $Trace.$name.p90 } } else { $null } }
    [ordered]@{
        presses = @($Trace.clicks).Count
        first_frame = & $pick 'first_frame'
        first_visible_change = & $pick 'first_visible_change'
        next_screen = & $pick 'next_screen'
        clicks = @($Trace.clicks | ForEach-Object {
            [ordered]@{ first_frame_ms = $_.first_frame; first_visible_change_ms = $_.first_visible_change; next_screen_ms = $_.next_screen }
        })
    }
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

# Launches the app and waits for its new window's first frame, then closes the window.
function Measure-WindowShow([string]$PackageName) {
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $frame = Get-DsaJson launch $PackageName --wait-first-frame --timeout 60
    $summary = Get-FirstFrameSummary $frame $clock.Elapsed.TotalMilliseconds
    $summary['state_after'] = Get-State
    foreach ($process in Get-AppWindowProcesses $PackageName) { $null = $process.CloseMainWindow() }
    $summary
}

function Measure-Windows {
    $results = [ordered]@{}
    foreach ($name in $Package) {
        $warm = @()
        $runs = @()
        for ($i = 0; $i -lt $Repeat; $i++) {
            Wait-Dozing
            $run = Measure-WindowShow $name
            $runs += $run
            $warm += [double]$run.seen_ms
        }
        $entry = [ordered]@{
            from_dozing = $runs
            from_dozing_first_frame_shown_ms = $warm
            from_dozing_median_ms = Get-Median $warm
        }
        if ($Cold) {
            Write-Host 'stopping Android for a cold start'
            Invoke-Dsa stop --wait | Out-Null
            $clock = [Diagnostics.Stopwatch]::StartNew()
            Invoke-Dsa start --wait | Out-Null
            $toActive = $clock.Elapsed.TotalMilliseconds
            $entry['cold_start_to_active_ms'] = [math]::Round($toActive)
            $cold = Measure-WindowShow $name
            $entry['cold_launch'] = $cold
            $entry['cold_start_to_first_frame_shown_ms'] = [math]::Round($toActive + $cold.seen_ms)
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

# The private working set of a process in KiB, or $null when it cannot be read.
function Get-PrivateWorkingSetKb([int]$Id) {
    try {
        $sample = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess=$Id"
        if ($sample) { return [double]$sample.WorkingSetPrivate / 1KB }
    } catch { }
    $null
}

function Measure-OpenApp {
    if ((Get-State) -eq 'Stopped') { Invoke-Dsa start --wait | Out-Null }
    $name = $Package[0]
    Invoke-Dsa launch $name | Out-Null
    Write-Host "waiting a minute with $name open"
    Start-Sleep -Seconds 60
    $memory = Get-DsaJson memory
    $processes = @()
    $ids = @(@($memory.host) | ForEach-Object { [int]$_.pid })
    $service = Get-ServiceProcess
    if ($service) { $ids += $service.Id }
    $totalKb = 0.0
    $complete = $true
    foreach ($id in $ids) {
        $kb = Get-PrivateWorkingSetKb $id
        if ($null -eq $kb) { $complete = $false } else { $totalKb += $kb }
        $role = (@($memory.host) | Where-Object { $_.pid -eq $id } | Select-Object -First 1).role
        if (-not $role) { $role = 'service' }
        $processes += [ordered]@{ role = $role; pid = $id; private_working_set_mib = if ($null -ne $kb) { ConvertTo-Mib $kb } else { $null } }
    }
    [ordered]@{
        package = $name
        state = Get-State
        processes = $processes
        total_private_working_set_mib = ConvertTo-Mib $totalKb
        near_term_target_mib = 400
        final_target_mib = 250
        complete = $complete
        guest_kernel = if ($memory.guest) { $memory.guest.kernel } else { $null }
    }
}

function Measure-CloseApp {
    if ((Get-State) -eq 'Stopped') { Invoke-Dsa start --wait | Out-Null }
    $name = $Package[0]
    Invoke-Dsa launch $name | Out-Null
    Start-Sleep -Seconds 20
    $vm = @((Get-DsaJson memory).host) | Where-Object { $_.role -eq 'vm' } | Select-Object -First 1
    $read = { $p = if ($vm) { Get-ProcessSafe $vm.pid } else { $null }; if ($p) { ConvertTo-Mib ($p.WorkingSet64 / 1KB) } else { $null } }
    $before = & $read
    foreach ($process in Get-AppWindowProcesses $name) { $null = $process.CloseMainWindow() }
    $samples = @()
    for ($i = 1; $i -le 30; $i++) {
        Start-Sleep -Seconds 1
        $samples += & $read
    }
    [ordered]@{
        package = $name
        vm_working_set_before_mib = $before
        vm_working_set_each_second_mib = $samples
        returned_mib = if ($null -ne $before -and $samples.Count) { [math]::Round($before - ($samples | Measure-Object -Minimum).Minimum, 1) } else { $null }
    }
}

function Measure-Click {
    if ((Get-State) -eq 'Stopped') { Invoke-Dsa start --wait | Out-Null }
    $settings = Get-DsaJson settings
    $traceOn = if ($settings.PSObject.Properties['input_trace']) { $settings.input_trace } else { $settings.settings.input_trace }
    if (-not $traceOn) {
        throw 'the input trace is off: run `dsa settings set input-trace on` as an administrator, close the app''s windows, and measure again'
    }
    if (-not (Test-Path $InputTrace)) { throw "scripts/input-trace.py is not at $InputTrace; pass -InputTrace" }
    $name = $Package[0]
    $after = ConvertTo-HostMs ([Diagnostics.Stopwatch]::GetTimestamp()) ([Diagnostics.Stopwatch]::Frequency)
    $traced = 0
    for ($i = 0; $i -lt $Repeat; $i++) {
        if ($i -eq 0 -or $Relaunch) {
            if ($Relaunch) { Invoke-Dsa stop-app $name | Out-Null; Start-Sleep -Seconds 2 }
            if (-not (Get-AppWindowProcesses $name | Where-Object { $_.MainWindowHandle -ne 0 })) {
                Get-DsaJson launch $name --wait-first-frame --timeout 60 | Out-Null
            }
            # The app settles after its first frame before it is pressed.
            Start-Sleep -Seconds 3
        }
        $tap = Get-DsaJson tap $name $TapX $TapY
        if ($tap.traced) { $traced++ }
        Start-Sleep -Milliseconds $TapGapMs
    }
    $output = & uv run $InputTrace --json --after $after.ToString([Globalization.CultureInfo]::InvariantCulture) 2>$null
    if ($LASTEXITCODE -ne 0) { throw "input-trace.py failed: $output" }
    $summary = Get-ClickSummary (($output | Where-Object { $_ -is [string] }) -join "`n" | ConvertFrom-Json)
    $summary['package'] = $name
    $summary['point'] = "$TapX,$TapY"
    $summary['presses_traced_by_the_window'] = "$traced of $Repeat"
    $summary
}

# The service's "memory:" log lines written since $Since.
function Get-MemoryLogLines([datetime]$Since) {
    $log = Join-Path $env:ProgramData 'DSA\logs\dsa-hostd.log'
    try {
        Get-Content $log -ErrorAction Stop | Where-Object { $_ -match 'memory: ' } | Where-Object {
            $stamp = $null
            if ($_ -match '^(\S+)') { [datetime]::TryParse($Matches[1], [ref]$stamp) | Out-Null }
            -not $stamp -or $stamp -ge $Since
        }
    } catch { @("the service log could not be read: $_") }
}

function Measure-Growth {
    if ((Get-State) -eq 'Stopped') { Invoke-Dsa start --wait | Out-Null }
    $since = Get-Date
    $samples = @()
    $sample = {
        param($Step)
        $memory = Get-DsaJson memory
        $vm = @($memory.host) | Where-Object { $_.role -eq 'vm' } | Select-Object -First 1
        [ordered]@{
            step = $Step
            guest_total_mib = if ($memory.guest) { ConvertTo-Mib $memory.guest.mem_total_kb } else { $null }
            guest_available_mib = if ($memory.guest) { ConvertTo-Mib $memory.guest.mem_available_kb } else { $null }
            vm_working_set_mib = if ($vm) { ConvertTo-Mib $vm.working_set_kb } else { $null }
        }
    }
    $samples += & $sample 'start'
    foreach ($name in $Package) {
        Invoke-Dsa launch $name | Out-Null
        Start-Sleep -Seconds 20
        $samples += & $sample "opened $name"
    }
    foreach ($process in Get-AppWindowProcesses) { $null = $process.CloseMainWindow() }
    Start-Sleep -Seconds 90
    $samples += & $sample 'closed all, 90 s later'
    [ordered]@{ samples = $samples; log = @(Get-MemoryLogLines $since) }
}

$report = [ordered]@{ environment = Get-Environment }
if ($Part -eq 'open') { $report['open_app'] = Measure-OpenApp }
if ($Part -eq 'close') { $report['close_app'] = Measure-CloseApp }
if ($Part -eq 'click') { $report['click'] = Measure-Click }
if ($Part -eq 'grow') { $report['grow'] = Measure-Growth }
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
