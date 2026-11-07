# A stand-in for dsa.exe for the pictures of the settings app's Apps page. The installer test
# has no real Android image, so nothing is installed in it; this answers `dsa apps list --json`
# with nine apps (eight the user installed, one of them without an icon, and one that is part
# of Android), answers the commands the page's rows run and `dsa logcat`, writes every command
# to calls.txt beside itself, and hands the rest to the real dsa.exe. Dot-source it, then:
#
#   $fake = New-AppsFixture "$env:RUNNER_TEMP\apps" "$env:PROGRAM_DIR\dsa.exe"
#   $env:DSA_CLI = $fake

Add-Type -AssemblyName System.Drawing

# A square PNG launcher icon, `$Size` pixels on a side, as base64.
function New-AppIcon([int]$Size, [string]$Color, [string]$Text) {
    $bitmap = New-Object System.Drawing.Bitmap $Size, $Size
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $brush = New-Object System.Drawing.SolidBrush ([System.Drawing.ColorTranslator]::FromHtml($Color))
    $graphics.FillEllipse($brush, 0, 0, $Size - 1, $Size - 1)
    $font = New-Object System.Drawing.Font 'Segoe UI', ($Size * 0.42), ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
    $format = New-Object System.Drawing.StringFormat
    $format.Alignment = [System.Drawing.StringAlignment]::Center
    $format.LineAlignment = [System.Drawing.StringAlignment]::Center
    $graphics.DrawString($Text, $font, [System.Drawing.Brushes]::White, (New-Object System.Drawing.RectangleF 0, 0, $Size, $Size), $format)
    $stream = New-Object System.IO.MemoryStream
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
    [Convert]::ToBase64String($stream.ToArray())
}

function New-FixtureApp([string]$Package, [string]$Label, [string]$Version, [long]$Installed, [bool]$System,
                        [long]$AppBytes, [long]$DataBytes, [long]$CacheBytes, [string]$Icon) {
    [ordered]@{
        package = $Package; label = $Label; version_name = $Version; version_code = 1
        first_install_time_ms = $Installed; last_update_time_ms = $Installed; system_app = $System
        app_bytes = $AppBytes; data_bytes = $DataBytes; cache_bytes = $CacheBytes
        total_bytes = $AppBytes + $DataBytes + $CacheBytes; icon_png = $Icon
    }
}

# The names of the stand-in's apps, as the page's rows show them.
$script:AppsFixtureLabels = 'Calendar', 'Camera', 'Clock', 'Files', 'Music', 'Notes', 'Reader', 'Settings', 'Weather'

# Writes the stand-in to `$Dir` and returns the path of the command to run as dsa.
function New-AppsFixture([string]$Dir, [string]$RealDsa) {
    New-Item -ItemType Directory -Force $Dir | Out-Null
    $list = [ordered]@{
        apps = @(
            New-FixtureApp 'com.example.notes' 'Notes' '4.1.2' 1700000000000 $false 18874368 2621440 524288 (New-AppIcon 96 '#2F6FDE' 'N')
            New-FixtureApp 'com.example.weather' 'Weather' '2.0' 1712000000000 $false 35651584 8388608 3145728 (New-AppIcon 96 '#E8833A' 'W')
            New-FixtureApp 'com.example.reader' 'Reader' '1.0.3' 1720000000000 $false 9437184 1048576 0 ''
            New-FixtureApp 'com.android.settings' 'Settings' '16' 0 $true 41943040 0 0 (New-AppIcon 96 '#5F6368' 'S')
            New-FixtureApp 'com.example.calendar' 'Calendar' '3.2' 1721000000000 $false 12582912 1048576 0 (New-AppIcon 96 '#1E8E3E' 'C')
            New-FixtureApp 'com.example.camera' 'Camera' '7.0' 1722000000000 $false 25165824 4194304 0 (New-AppIcon 96 '#A142F4' 'A')
            New-FixtureApp 'com.example.clock' 'Clock' '1.4' 1723000000000 $false 6291456 524288 0 (New-AppIcon 96 '#D93025' 'K')
            New-FixtureApp 'com.example.files' 'Files' '5.5' 1724000000000 $false 15728640 2097152 0 (New-AppIcon 96 '#F9AB00' 'F')
            New-FixtureApp 'com.example.music' 'Music' '9.1' 1725000000000 $false 31457280 16777216 0 (New-AppIcon 96 '#12B5CB' 'M')
        )
    }
    $list | ConvertTo-Json -Depth 5 -Compress | Set-Content -Encoding ascii "$Dir\apps.json"
    # `dsa apps info com.example.notes`, for the uninstall window.
    $list.apps[0] | ConvertTo-Json -Depth 5 -Compress | Set-Content -Encoding ascii "$Dir\notes.json"
    # What `dsa logcat --json` prints: the Android log page's list.
    1..30 | ForEach-Object {
        [ordered]@{ time = '11-07 10:00:00.000'; priority = 'I'; buffer = 'main'; tag = 'Fixture'; pid = 100; tid = 100; message = "fixture line $_"; dropped = 0 } |
            ConvertTo-Json -Compress
    } | Set-Content -Encoding ascii "$Dir\logcat.txt"
    Remove-Item "$Dir\calls.txt" -ErrorAction SilentlyContinue
    $cmd = @"
@echo off
echo %* >>"%~dp0calls.txt"
if "%~1 %~2"=="apps list" (
  type "%~dp0apps.json"
  exit /b 0
)
if "%~1 %~2 %~3"=="apps info com.example.notes" (
  type "%~dp0notes.json"
  exit /b 0
)
rem The uninstall takes a while, so a picture shows the window's progress; nothing is removed.
if "%~1 %~2 %~3"=="apps uninstall com.example.notes" (
  ping -n 60 127.0.0.1 >nul
  echo {"package":"com.example.notes","uninstalled":true,"status":"uninstalled","message":""}
  exit /b 0
)
if "%~1 %~2"=="apps uninstall" (
  echo {"package":"%~3","uninstalled":true,"status":"uninstalled","message":""}
  exit /b 0
)
if "%~1 %~2"=="apps settings" (
  echo {}
  exit /b 0
)
if "%~1"=="launch" exit /b 0
if "%~1"=="app-window" (
  echo {"apps":[]}
  exit /b 0
)
if "%~1"=="logcat" (
  type "%~dp0logcat.txt"
  exit /b 0
)
"$RealDsa" %*
"@
    Set-Content -Encoding ascii "$Dir\dsa.cmd" $cmd
    "$Dir\dsa.cmd"
}
