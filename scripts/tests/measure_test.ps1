# Checks the pure functions of dsa/host/platform/windows/tools/measure.ps1.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot '../../dsa/host/platform/windows/tools/measure.ps1') -FunctionsOnly

$failures = 0
function Check([string]$Name, $Actual, $Expected) {
    if ($Actual -ne $Expected) {
        Write-Host "FAIL $Name`: got $Actual, expected $Expected"
        $script:failures++
    } else {
        Write-Host "ok   $Name"
    }
}

# nfr3: CPU time over wall time, as a share of one logical core.
Check 'nfr3_one_cpu_second_in_ten_minutes_is_a_sixth_of_a_percent' (Get-CorePercent 10 11 600) 0.167
Check 'nfr3_no_time_has_no_percentage' (Get-CorePercent 1 2 0) $null

# nfr1: the dozing total is the host processes and the service, in MB.
$memory = [pscustomobject]@{
    guest = [pscustomobject]@{ total_pss_kb = 512000 }
    host = @(
        [pscustomobject]@{ role = 'vm'; pid = 10; working_set_kb = 700000 }
        [pscustomobject]@{ role = 'window manager'; pid = 11; working_set_kb = 40000 }
        [pscustomobject]@{ role = 'app window'; pid = 12; working_set_kb = 10000 }
    )
    host_total_kb = 750000
}
$summary = Get-IdleMemorySummary $memory 30000
Check 'nfr1_the_vm_is_counted_apart' $summary.vm_working_set_mib 683.6
Check 'nfr1_the_window_processes_are_summed' $summary.window_processes_mib 48.8
Check 'nfr1_the_total_adds_the_service' $summary.total_mb 798.7
Check 'nfr1_under_800_mb_is_within_the_target' $summary.within_target $true
Check 'nfr1_with_the_service_read_the_figure_is_complete' $summary.complete $true
Check 'nfr1_the_guest_pss_is_reported' $summary.guest_total_pss_mib 500

$unread = Get-IdleMemorySummary $memory $null
Check 'nfr1_an_unread_service_makes_the_figure_incomplete' $unread.complete $false
Check 'nfr1_an_unread_service_is_not_guessed' $unread.service_working_set_mib $null

$over = Get-IdleMemorySummary ([pscustomobject]@{ guest = $null; host = @(); host_total_kb = 800000 }) 0
Check 'nfr1_over_800_mb_is_over_the_target' $over.within_target $false
Check 'nfr1_without_a_vm_the_figure_is_incomplete' $over.complete $false

# nfr4: the median of the repeated launches.
Check 'nfr4_the_median_of_an_odd_count_is_the_middle' (Get-Median @(900, 300, 500)) 500
Check 'nfr4_the_median_of_an_even_count_is_the_mean_of_the_middle_two' (Get-Median @(100, 400, 200, 300)) 250

# nfr4: a launch is timed to its first frame as the window says it, shown when known.
$frame = Get-FirstFrameSummary ([pscustomobject]@{ request_ms = 12.0; first_frame_ms = 845.3; first_shown_ms = 861.0 }) 900.4
Check 'nfr4_the_first_frame_shown_is_what_the_user_sees' $frame.seen_ms 861.0
Check 'nfr4_the_first_frame_presented_is_kept' $frame.first_frame_ms 845.3
Check 'nfr4_the_whole_command_is_rounded' $frame.command_ms 900
$unshown = Get-FirstFrameSummary ([pscustomobject]@{ request_ms = 12.0; first_frame_ms = 845.3; first_shown_ms = $null }) 900
Check 'nfr4_without_a_shown_time_the_presented_time_stands_in' $unshown.seen_ms 845.3

# fr5: the host clock in ms, as the DSA processes log it.
Check 'fr5_the_host_clock_is_ticks_over_frequency_in_ms' (ConvertTo-HostMs 25000000 10000000) 2500
Check 'fr5_no_ticks_is_zero' (ConvertTo-HostMs 0 10000000) 0

# fr5: the click numbers input-trace.py printed.
$trace = '{"clicks":[{"task":"7","host":1000.0,"first_frame":40.0,"first_visible_change":90.0,"next_screen":null}],"first_frame":{"count":1,"median":40.0,"p90":40.0},"first_visible_change":{"count":1,"median":90.0,"p90":90.0},"next_screen":null,"displayed":null}' | ConvertFrom-Json
$clicks = Get-ClickSummary $trace
Check 'fr5_each_press_is_counted' $clicks.presses 1
Check 'fr5_the_first_frame_median_is_reported' $clicks.first_frame.median_ms 40.0
Check 'fr5_the_first_visible_change_median_is_reported' $clicks.first_visible_change.median_ms 90.0
Check 'fr5_a_number_no_press_had_is_null' $clicks.next_screen $null
Check 'fr5_each_press_keeps_its_visible_change' $clicks.clicks[0].first_visible_change_ms 90.0
if ($failures) {
    Write-Host "$failures checks failed"
    exit 1
}
