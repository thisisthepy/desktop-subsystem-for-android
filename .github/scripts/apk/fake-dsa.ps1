# A stand-in for dsa.exe that the installer test points the APK install window at (with
# DSA_CLI), so that pictures can be taken of the screens a running Android leads to: starting
# Android, an update, a refused downgrade or signer, progress, done and a failure. The runner
# has no Android that boots, so this makes up what Android would say; the APK itself is read
# by the real dsa (REAL_DSA).
#
#   FAKE_DSA_SCENARIO  new, update, downgrade, signature or failure
#   FAKE_DSA_STATE     a file that exists while the made-up Android runs; `start` makes it

$ErrorActionPreference = 'Stop'
$scenario = $env:FAKE_DSA_SCENARIO
$running = Test-Path $env:FAKE_DSA_STATE

switch ($args[0]) {
  'apk-info' {
    $json = & $env:REAL_DSA apk-info $args[1] --json
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $info = $json | ConvertFrom-Json
    if ($running) {
      $signer = $info.signer.sha256
      $info.android = 'running'
      switch ($scenario) {
        'update' {
          $info.installed = @{ version_code = 41; version_name = '4.1'; signers = @($signer) }
          $info.install = 'update'
        }
        'downgrade' {
          $info.installed = @{ version_code = 50; version_name = '5.0'; signers = @($signer) }
          $info.install = 'downgrade'
        }
        'signature' {
          $info.installed = @{ version_code = 41; version_name = '4.1'; signers = @('AA:BB') }
          $info.install = 'signature-mismatch'
        }
        default { $info.install = 'new' }
      }
    }
    $info | ConvertTo-Json -Depth 8 -Compress
  }
  'start' {
    Start-Sleep -Seconds 4
    New-Item -ItemType File -Force $env:FAKE_DSA_STATE | Out-Null
    '{"state":"Active"}'
  }
  'install' {
    'stage sending'
    foreach ($percent in 0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100) {
      "progress $percent"
      Start-Sleep -Milliseconds 700
    }
    'stage installing'
    Start-Sleep -Seconds 2
    if ($scenario -eq 'failure') {
      [Console]::Error.WriteLine('dsa: not installed (incompatible): notes.apk: The app does not run on this Android: it needs another processor type, a newer Android version or a feature this device does not have (com.example.notes). Android said: INSTALL_FAILED_NO_MATCHING_ABIS: Failed to extract native libraries, res=-113')
      exit 1
    }
    '{"installed":true,"status":"installed","package":"com.example.notes","message":""}'
  }
  'launch' { "Launching $($args[1])" }
  default {
    [Console]::Error.WriteLine("dsa: the stand-in does not know $($args -join ' ')")
    exit 2
  }
}
exit 0
