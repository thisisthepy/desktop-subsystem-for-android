# A stand-in for dsa.exe for the pictures of the settings app's Memory page. The installer
# test has no real Android image, so no guest reports its memory; this answers
# `dsa memory --json` with a VM, a window manager and a guest report that has the kernel's
# breakdown, and hands every other command (the settings among them) to the real dsa.exe.
# Dot-source it, then:
#
#   $fake = New-MemoryFixture "$env:RUNNER_TEMP\memory" "$env:PROGRAM_DIR\dsa.exe"
#   $env:DSA_CLI = $fake

function New-MemoryFixture([string]$Dir, [string]$RealDsa) {
    New-Item -ItemType Directory -Force $Dir | Out-Null
    $report = [ordered]@{
        host = @(
            [ordered]@{ role = 'vm'; pid = 4120; working_set_kb = 1153024; private_kb = 1150000 }
            [ordered]@{ role = 'window manager'; pid = 5312; working_set_kb = 61440; private_kb = 30720 }
        )
        host_total_kb = 1214464
        vm_beyond_guest_kb = 131072
        guest = [ordered]@{
            mem_total_kb = 2936012; mem_free_kb = 1048576; mem_available_kb = 1912602
            buffers_kb = 20480; cached_kb = 512000; anon_pages_kb = 409600; shmem_kb = 8192
            slab_reclaimable_kb = 40960; swap_total_kb = 1048576; swap_free_kb = 1000000
            zram_original_kb = 49152; zram_used_kb = 12288; total_pss_kb = 614400
            kernel = [ordered]@{
                slab_unreclaimable_kb = 61440; kernel_stack_kb = 12288; page_tables_kb = 18432
                vmalloc_used_kb = 24576; percpu_kb = 2048; cma_total_kb = 0; hugetlb_kb = 0
                mem_online_kb = 3145728; unaccounted_kb = 36864; reserved_at_boot_kb = 209716
            }
            age_seconds = 2
            top_processes = @(
                [ordered]@{ pid = 612; name = 'system_server'; pss_kb = 184320; swap_pss_kb = 0 }
                [ordered]@{ pid = 1840; name = 'com.example.notes'; pss_kb = 92160; swap_pss_kb = 0 }
                [ordered]@{ pid = 455; name = 'surfaceflinger'; pss_kb = 30720; swap_pss_kb = 0 }
            )
        }
    }
    $report | ConvertTo-Json -Depth 6 -Compress | Set-Content -Encoding ascii "$Dir\memory.json"
    $cmd = @"
@echo off
if "%~1"=="memory" (
  type "%~dp0memory.json"
  exit /b 0
)
"$RealDsa" %*
"@
    Set-Content -Encoding ascii "$Dir\dsa.cmd" $cmd
    "$Dir\dsa.cmd"
}
