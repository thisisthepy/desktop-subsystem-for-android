# Checks the pure functions of scripts/install-dev-build.ps1: which repository it installs
# from, and which runs it refuses. Nothing is downloaded or installed.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot '../install-dev-build.ps1') -FunctionsOnly

$failures = 0
function Check([string]$Name, $Actual, $Expected) {
    if ($Actual -ne $Expected) {
        Write-Host "FAIL $Name`: got '$Actual', expected '$Expected'"
        $script:failures++
    } else {
        Write-Host "ok   $Name"
    }
}
function Check-Throws([string]$Name, [scriptblock]$Block, [string]$Says) {
    try {
        & $Block | Out-Null
        Write-Host "FAIL $Name`: nothing was thrown"
        $script:failures++
    } catch {
        if ($_.Exception.Message -notlike "*$Says*") {
            Write-Host "FAIL $Name`: threw '$($_.Exception.Message)', which does not say '$Says'"
            $script:failures++
        } else {
            Write-Host "ok   $Name"
        }
    }
}

$fork = 'someone/desktop-subsystem-for-android'
$official = 'thisisthepy/desktop-subsystem-for-android'

Check 'repo_is_taken_from_the_repo_parameter' (Resolve-DevBuildRepo $fork 'https://github.com/thisisthepy/desktop-subsystem-for-android.git') $fork
Check 'repo_defaults_to_the_https_origin_of_a_forks_clone' (Resolve-DevBuildRepo '' "https://github.com/$fork.git") $fork
Check 'repo_defaults_to_the_ssh_origin_of_a_forks_clone' (Resolve-DevBuildRepo '' "git@github.com:$fork.git") $fork
Check 'repo_defaults_to_an_origin_without_git_suffix' (Resolve-DevBuildRepo '' "https://github.com/$fork") $fork
Check 'repo_defaults_to_an_ssh_url_origin' (Resolve-DevBuildRepo '' "ssh://git@github.com/$fork.git") $fork
Check 'repo_defaults_to_the_official_origin' (Resolve-DevBuildRepo '' "https://github.com/$official.git") $official
Check-Throws 'an_origin_that_is_not_github_is_refused' { Resolve-DevBuildRepo '' 'https://example.com/someone/dsa.git' } '-Repo owner/name'
Check-Throws 'no_origin_and_no_repo_is_refused' { Resolve-DevBuildRepo '' '' } '-Repo owner/name'
Check-Throws 'a_repo_that_is_not_owner_name_is_refused' { Resolve-DevBuildRepo 'https://github.com/someone/dsa' '' } 'owner/name'

function New-Run([string]$repo, [string]$head = $repo, [string]$path = '.github/workflows/dev-build.yml', [string]$branch = 'develop', [string]$conclusion = 'success') {
    [pscustomobject]@{
        id = 42
        path = $path
        head_branch = $branch
        status = 'completed'
        conclusion = $conclusion
        repository = [pscustomobject]@{ full_name = $repo }
        head_repository = [pscustomobject]@{ full_name = $head }
    }
}

Check 'a_forks_own_development_build_of_develop_is_accepted' (Test-DevBuildRun (New-Run $fork) $fork) $null
Check 'the_official_development_build_is_accepted' (Test-DevBuildRun (New-Run $official) $official) $null
Check 'a_run_of_another_repository_is_refused' (Test-DevBuildRun (New-Run $official) $fork) "run 42 is in $official, not $fork"
Check 'a_run_that_built_another_repositorys_code_is_refused' (Test-DevBuildRun (New-Run $fork 'other/fork') $fork) "run 42 built other/fork, not $fork"
Check 'a_run_of_another_workflow_is_refused' (Test-DevBuildRun (New-Run $fork $fork '.github/workflows/test.yml') $fork) 'run 42 is not a Development build (it is .github/workflows/test.yml)'
Check 'a_run_of_another_branch_is_refused' (Test-DevBuildRun (New-Run $fork $fork '.github/workflows/dev-build.yml' 'feat/x') $fork) 'run 42 was made from feat/x, not develop'
Check 'a_failed_run_is_refused' (Test-DevBuildRun (New-Run $fork $fork '.github/workflows/dev-build.yml' 'develop' 'failure') $fork) 'run 42 did not pass (it is completed, failure)'

# The script checks the service's verdict and installs only what it checked; those checks stay.
$script = Get-Content -Raw (Join-Path $PSScriptRoot '../install-dev-build.ps1')
foreach ($kept in @('check-setup $installerDir', 'does not match SHA256SUMS.txt', 'android install-image $imageDir', 'does not match the list''s SHA-256', 'Test-DevBuildRun $info $Repo')) {
    Check "the_script_still_runs_$($kept -replace '\W+', '_')" $script.Contains($kept) $true
}
Check 'going_back_installs_the_official_release' $script.Contains('gh release download v$Release --repo $officialRepo') $true

if ($failures) {
    Write-Host "$failures check(s) failed"
    exit 1
}
